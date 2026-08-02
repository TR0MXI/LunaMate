//! 加载和编辑短期上下文，并把会话修改交还给会话持有者执行。

use std::{cell::Cell, collections::HashSet, rc::Rc, time::Duration};

use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use lunamate_agent::config::{PersonaConfig, SharedLlmSettings};
use lunamate_agent::memory::{ContextMessage, ContextUsage};
use lunamate_agent::{MAX_SESSION_TEXT_BYTES, chat_limits, context_message_tokens};
use rust_i18n::t;

use super::{
    super::set_input, ContextMessageEditor, ContextMessageLayout, ContextMutationCompletion,
    PersonaPage, PersonaSettingsEvent, PersonaSettingsView,
};

const CONTEXT_AUTO_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

impl PersonaSettingsView {
    pub(super) fn select_page(
        &mut self,
        page: PersonaPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_page == page {
            return;
        }
        self.commit_context_edits(cx);
        self.capture_current_form(cx);
        self.save(cx);
        self.active_page = page;
        self.cancel_context_selection();
        self.context_editing = None;
        if page == PersonaPage::Context {
            self.refresh_usage(cx);
            self.refresh_context(window, cx);
            self.start_context_auto_refresh(window, cx);
        } else {
            self.stop_context_auto_refresh();
            self.context_revision = self.context_revision.wrapping_add(1).max(1);
            self.context_task = None;
            self.context_loading = false;
        }
        cx.notify();
    }

    pub(super) fn start_context_auto_refresh(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_auto_refresh_revision =
            self.context_auto_refresh_revision.wrapping_add(1).max(1);
        self.observed_live_context_revision = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .and_then(|persona| self.memory.live_context_usage().revision_for(&persona.id));
        self.schedule_context_auto_refresh(window, cx);
    }

    pub(super) fn stop_context_auto_refresh(&mut self) {
        self.context_auto_refresh_revision =
            self.context_auto_refresh_revision.wrapping_add(1).max(1);
        self.context_auto_refresh_task = None;
    }

    fn schedule_context_auto_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.context_auto_refresh_revision;
        let live = self.memory.live_context_usage();
        let persona = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .map(|persona| persona.id.clone());
        let background = cx.background_executor().clone();
        self.context_auto_refresh_task = Some(cx.spawn_in(window, async move |this, cx| {
            background.timer(CONTEXT_AUTO_REFRESH_INTERVAL).await;
            let live_revision = persona
                .as_deref()
                .and_then(|persona| live.revision_for(persona));
            let _ = cx.update(|window, app| {
                let _ = this.update(app, |this, cx| {
                    if this.active_page != PersonaPage::Context
                        || this.context_auto_refresh_revision != generation
                    {
                        return;
                    }
                    if live_revision.is_some()
                        && live_revision != this.observed_live_context_revision
                        && this.context_editing.is_none()
                        && this.context_selection_drag.is_none()
                        && this.context_selected.is_empty()
                    {
                        this.observed_live_context_revision = live_revision;
                        this.refresh_usage(cx);
                        this.refresh_context(window, cx);
                    }
                    this.schedule_context_auto_refresh(window, cx);
                });
            });
        }));
    }

    pub(super) fn refresh_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_context_selection();
        self.context_revision = self.context_revision.wrapping_add(1).max(1);
        let revision = self.context_revision;
        self.context_task = None;
        self.context_loading = true;
        self.context_editors.clear();
        self.context_subscriptions.clear();
        self.context_selected.clear();
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
        else {
            self.context_editors.clear();
            self.context_subscriptions.clear();
            self.context_selected.clear();
            self.context_loading = false;
            self.context_error = None;
            return;
        };
        let limits = context_limit_usage(persona, &self.providers);
        let memory = self.memory.persona(&persona.id);
        let live = self.memory.live_context_usage();
        let load = Tokio::spawn(
            cx,
            async move { memory.context_messages(live, limits).await },
        );
        self.context_error = None;
        self.context_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            let _ = cx.update(|window, app| {
                let _ = this.update(app, |this, cx| {
                    if this.context_revision != revision {
                        return;
                    }
                    this.context_task = None;
                    this.context_loading = false;
                    match result {
                        Ok(Ok(messages)) => {
                            this.replace_context_editors(messages, window, cx);
                            this.context_error = None;
                        }
                        Ok(Err(error)) => {
                            this.context_editors.clear();
                            this.context_subscriptions.clear();
                            this.context_selected.clear();
                            this.context_error = Some(error.to_string());
                        }
                        Err(error) => {
                            this.context_editors.clear();
                            this.context_subscriptions.clear();
                            this.context_selected.clear();
                            this.context_error = Some(error.to_string());
                        }
                    }
                    cx.notify();
                });
            });
        }));
        cx.notify();
    }

    fn replace_context_editors(
        &mut self,
        messages: Vec<ContextMessage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_subscriptions.clear();
        self.context_selected.clear();
        let mut editors = Vec::with_capacity(messages.len());
        let mut subscriptions = Vec::with_capacity(messages.len());
        for message in messages {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .auto_grow(1, 12)
                    .default_value(message.content.clone())
            });
            let message_id = message.id;
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.enforce_context_message_size(message_id, input.clone(), window, cx);
                    }
                    InputEvent::Blur => {
                        this.commit_context_message(message_id, input.clone(), Some(window), cx);
                        if this.context_editing == Some(message_id) {
                            this.context_editing = None;
                            cx.notify();
                        }
                    }
                    InputEvent::Focus | InputEvent::PressEnter { .. } => {}
                },
            ));
            editors.push(ContextMessageEditor {
                id: message.id,
                role: message.role,
                input,
                layout: Rc::new(Cell::new(ContextMessageLayout::default())),
                saved_content: message.content,
                tokens: message.tokens,
                fixed_tokens: message.fixed_tokens,
            });
        }
        self.context_editors = editors;
        self.context_subscriptions = subscriptions;
        self.context_scroll.scroll_to_bottom();
    }

    fn enforce_context_message_size(
        &mut self,
        message_id: u64,
        input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input.read(cx).value().len() <= MAX_SESSION_TEXT_BYTES {
            return;
        }
        let Some(saved) = self
            .context_editors
            .iter()
            .find(|editor| editor.id == message_id)
            .map(|editor| editor.saved_content.clone())
        else {
            return;
        };
        set_input(&input, &saved, window, cx);
        self.set_status(t!("persona.context_message_file_too_large").to_string(), cx);
    }

    pub(super) fn commit_context_edits(&mut self, cx: &mut Context<Self>) {
        let pending = self
            .context_editors
            .iter()
            .map(|editor| (editor.id, editor.input.clone()))
            .collect::<Vec<_>>();
        for (message_id, input) in pending {
            self.commit_context_message(message_id, input, None, cx);
        }
    }

    fn commit_context_message(
        &mut self,
        message_id: u64,
        input: Entity<InputState>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.context_loading {
            return;
        }
        let value = input.read(cx).value().trim().to_owned();
        let Some(index) = self
            .context_editors
            .iter()
            .position(|editor| editor.id == message_id)
        else {
            return;
        };
        let saved = self.context_editors[index].saved_content.clone();
        if value == saved {
            return;
        }
        if value.is_empty() {
            if let Some(window) = window {
                set_input(&input, &saved, window, cx);
            }
            self.set_status(t!("persona.context_message_empty").to_string(), cx);
            return;
        }
        let new_tokens = context_message_tokens(&value, self.context_editors[index].fixed_tokens);
        let current_tokens = self.usage.map_or_else(
            || {
                self.context_editors
                    .iter()
                    .map(|editor| editor.tokens)
                    .sum()
            },
            |usage| usage.context.tokens,
        );
        let max_tokens = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .cloned()
            .map(|mut persona| {
                persona.context = self.capture_context_limits(cx);
                persona.model = self.selected_provider_id(cx);
                context_limit_usage(&persona, &self.providers).max_tokens
            })
            .unwrap_or_default();
        let next_tokens = current_tokens
            .saturating_sub(self.context_editors[index].tokens)
            .saturating_add(new_tokens);
        if next_tokens > max_tokens {
            if let Some(window) = window {
                set_input(&input, &saved, window, cx);
            }
            self.set_status(t!("persona.context_message_too_large").to_string(), cx);
            return;
        }
        self.context_editors[index].saved_content = value.clone();
        self.context_editors[index].tokens = new_tokens;
        self.usage_revision = self.usage_revision.wrapping_add(1).max(1);
        self.usage_task = None;
        if let Some(usage) = &mut self.usage {
            usage.context.tokens = next_tokens;
        }
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .map(|persona| persona.id.clone())
        else {
            return;
        };
        let completion = Some(match window {
            Some(window) => self.watch_context_mutation(persona.clone(), None, window, cx),
            None => self.watch_context_mutation_without_window(persona.clone(), cx),
        });
        cx.emit(PersonaSettingsEvent::EditContextMessage {
            persona,
            message_id,
            content: value,
            completion,
        });
        cx.notify();
    }

    pub(super) fn delete_context_messages(
        &mut self,
        persona: String,
        message_ids: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = message_ids.iter().copied().collect::<HashSet<_>>();
        if selected.is_empty() {
            return;
        }
        let editors = std::mem::take(&mut self.context_editors);
        let subscriptions = std::mem::take(&mut self.context_subscriptions);
        let mut removed_messages = 0_usize;
        let mut removed_tokens = 0_usize;
        for (editor, subscription) in editors.into_iter().zip(subscriptions) {
            if selected.contains(&editor.id) {
                removed_messages = removed_messages.saturating_add(1);
                removed_tokens = removed_tokens.saturating_add(editor.tokens);
            } else {
                self.context_editors.push(editor);
                self.context_subscriptions.push(subscription);
            }
        }
        if removed_messages == 0 {
            return;
        }
        self.usage_revision = self.usage_revision.wrapping_add(1).max(1);
        self.usage_task = None;
        self.context_selected.retain(|id| !selected.contains(id));
        if let Some(usage) = &mut self.usage {
            usage.context.messages = usage.context.messages.saturating_sub(removed_messages);
            usage.context.tokens = usage.context.tokens.saturating_sub(removed_tokens);
        }
        let completion = self.watch_context_mutation(
            persona.clone(),
            Some(t!("persona.context_messages_deleted", count = removed_messages).to_string()),
            window,
            cx,
        );
        cx.emit(PersonaSettingsEvent::DeleteContextMessages {
            persona,
            message_ids,
            completion: Some(completion),
        });
        cx.notify();
    }

    fn watch_context_mutation(
        &mut self,
        persona: String,
        success: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ContextMutationCompletion {
        let (sender, receiver) = async_channel::bounded::<Result<(), String>>(1);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = cx.update(|window, app| {
                let _ = this.update(app, |this, cx| {
                    match result {
                        Ok(()) => {
                            if let Some(success) = success {
                                this.set_status(success, cx);
                            }
                        }
                        Err(error) => {
                            let is_current = this
                                .editing_index
                                .and_then(|index| this.draft.personas.get(index))
                                .is_some_and(|current| current.id == persona);
                            if is_current {
                                this.refresh_usage(cx);
                                if this.active_page == PersonaPage::Context {
                                    this.refresh_context(window, cx);
                                }
                            }
                            this.set_status(
                                t!("persona.context_message_save_failed", error = error)
                                    .to_string(),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        sender
    }

    fn watch_context_mutation_without_window(
        &mut self,
        persona: String,
        cx: &mut Context<Self>,
    ) -> ContextMutationCompletion {
        let (sender, receiver) = async_channel::bounded::<Result<(), String>>(1);
        let task = cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    let is_current = this
                        .editing_index
                        .and_then(|index| this.draft.personas.get(index))
                        .is_some_and(|current| current.id == persona);
                    if is_current {
                        this.context_error = Some(error.clone());
                        this.refresh_usage(cx);
                    }
                    this.set_status(
                        t!("persona.context_message_save_failed", error = error).to_string(),
                        cx,
                    );
                }
            });
        });
        self.track_write_task(task);
        sender
    }

    pub(super) fn begin_context_message_edit(
        &mut self,
        message_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self
            .context_editors
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| message.input.clone())
        else {
            return;
        };
        self.context_editing = Some(message_id);
        cx.on_next_frame(window, move |this, window, cx| {
            if this.context_editing == Some(message_id) {
                input.update(cx, |input, cx| input.focus(window, cx));
            }
        });
        cx.notify();
    }

    pub(super) fn cancel_context_message_edit(
        &mut self,
        message_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((input, saved)) = self
            .context_editors
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| (message.input.clone(), message.saved_content.clone()))
        else {
            return;
        };
        set_input(&input, &saved, window, cx);
        self.context_editing = None;
        window.blur();
        cx.notify();
    }

    /// 直接装载可编辑消息，供无头测试覆盖自适应输入构造而不依赖 Tokio 唤醒。
    #[cfg(test)]
    pub(crate) fn load_context_messages_for_test(
        &mut self,
        messages: Vec<ContextMessage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_context_editors(messages, window, cx);
        cx.notify();
    }

    /// 直接切换上下文页，避免无头测试启动数据库查询和配置写入。
    #[cfg(test)]
    pub(crate) fn show_context_for_test(&mut self, cx: &mut Context<Self>) {
        self.active_page = PersonaPage::Context;
        cx.notify();
    }

    /// 返回当前已构造的消息编辑器数量。
    #[cfg(test)]
    pub(crate) fn context_editor_count_for_test(&self) -> usize {
        self.context_editors.len()
    }

    /// 返回当前卡片顺序，供测试断言拖拽使用稳定消息 ID。
    #[cfg(test)]
    pub(crate) fn context_message_ids_for_test(&self) -> Vec<u64> {
        self.context_editors
            .iter()
            .map(|message| message.id)
            .collect()
    }

    /// 进入指定消息的编辑态，供无头测试验证编辑布局与 Esc 回退。
    #[cfg(test)]
    pub(crate) fn begin_context_message_edit_for_test(
        &mut self,
        message_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_context_message_edit(message_id, window, cx);
    }

    /// 直接修改消息输入值，避免测试依赖平台文本输入法。
    #[cfg(test)]
    pub(crate) fn set_context_message_content_for_test(
        &mut self,
        message_id: u64,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self
            .context_editors
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| message.input.clone())
        {
            set_input(&input, value, window, cx);
        }
    }

    /// 返回消息编辑器当前文本，供测试确认取消编辑未提交修改。
    #[cfg(test)]
    pub(crate) fn context_message_content_for_test(
        &self,
        message_id: u64,
        cx: &Context<Self>,
    ) -> Option<String> {
        self.context_editors
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| message.input.read(cx).value().to_string())
    }
}

/// 把人格的上下文限制翻译为只包含上限的占用结构，供未加载人格的统计回填。
pub(super) fn context_limit_usage(
    persona: &PersonaConfig,
    providers: &SharedLlmSettings,
) -> ContextUsage {
    let limits = chat_limits(persona, providers);
    ContextUsage {
        messages: 0,
        max_messages: limits.max_messages,
        tokens: 0,
        max_tokens: limits.max_tokens,
    }
}
