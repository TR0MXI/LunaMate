//! 管理设置实体的初始化、窗口绑定状态、Agent 编辑器生命周期与后台写任务。

use gpui::{AppContext, Context, Entity, KeyDownEvent, Task, Window};
use gpui_component::input::{InputEvent, InputState, MaskPattern};
use lunamate_agent::chat_limits;

use crate::{
    config::{
        CONFIG, CUSTOM_FRAME_RATE_MAX, CUSTOM_FRAME_RATE_MIN, LOGGING_MAX_FILE_SIZE_MB,
        LOGGING_MAX_KEEP_FILES, LOGGING_MIN_FILE_SIZE_MB, LOGGING_MIN_KEEP_FILES,
    },
    ui::{apply, apply_language},
};

use super::{
    AppliedSettings, ConfigSection, ContextMutationCompletion, InputEditSession,
    ModelSelectionBaseline, ModelSelectionWriteState, PersonaSettingsDraft, PersonaSettingsEvent,
    PersonaSettingsView, ProviderSettingsDraft, ProviderSettingsEvent, ProviderSettingsView,
    RetiredPersonaSettingsEditor, RetiredProviderSettingsEditor, SettingsEvent, SettingsView,
    custom_frame_rate_seed,
};

impl AppliedSettings {
    fn current() -> Self {
        Self {
            frame_rate: CONFIG.frame_rate(),
            model_window_size: CONFIG.model_window_size(),
            remember_window_positions: CONFIG.remember_window_positions(),
            eye_tracking: CONFIG.eye_tracking(),
            show_fps: CONFIG.show_fps(),
            use_native_tray_menu: CONFIG.use_native_tray_menu(),
            allow_agent_outfit_change: CONFIG.allow_agent_outfit_change(),
            appearance: CONFIG.appearance().as_ref().clone(),
            voice: CONFIG.voice_settings().as_ref().clone(),
            shortcuts: CONFIG.shortcut_settings().as_ref().clone(),
            model_resources: CONFIG.model_resource_settings(),
            global_model_selection: CONFIG.selected_model(),
        }
    }
}

impl SettingsView {
    /// 使用启动阶段得到的模型目录和配置诊断创建界面。
    pub(crate) fn new(
        catalog: crate::model::ModelCatalog,
        agent: std::sync::Arc<lunamate_agent::Agent>,
        status: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 最后一个窗口关闭时实体可能先于 quit observer 释放；配置写任务必须脱离实体继续完成。
        cx.on_release(|this, _| {
            for task in std::mem::take(&mut this.write_tasks) {
                task.detach();
            }
        })
        .detach();
        let applied = AppliedSettings::current();
        let model_selection_write_state = ModelSelectionWriteState::new(
            ModelSelectionBaseline::initial(&catalog, applied.global_model_selection.clone()),
        );
        let persisted_logging = *CONFIG.logging_settings();
        let mut view = Self {
            catalog,
            agent,
            provider_settings_view: None,
            provider_settings_draft: None,
            persona_settings_view: None,
            persona_settings_draft: None,
            custom_accent_input: None,
            custom_background_input: None,
            custom_frame_rate_input: None,
            log_max_size_input: None,
            log_keep_files_input: None,
            model_resource_name_input: None,
            input_edit: None,
            shortcut_focus: None,
            preview_capabilities: crate::model::ModelPreviewCapabilities::default(),
            model_resources: applied.model_resources.clone(),
            editing_model_resource: None,
            active_outfit: None,
            global_model_selection: applied.global_model_selection.clone(),
            applied_persona_id: None,
            applied_persona_model: None,
            section: ConfigSection::Model,
            status: None,
            frame_rate: applied.frame_rate,
            model_window_size: applied.model_window_size,
            remember_window_positions: applied.remember_window_positions,
            eye_tracking: applied.eye_tracking,
            show_fps: applied.show_fps,
            use_native_tray_menu: applied.use_native_tray_menu,
            allow_agent_screenshot: CONFIG.allow_agent_screenshot(),
            allow_agent_outfit_change: applied.allow_agent_outfit_change,
            screenshot_permission_retry_required: CONFIG
                .agent_screenshot_permission_retry_required(),
            logging: persisted_logging,
            persisted_logging,
            appearance: applied.appearance.clone(),
            voice: applied.voice.clone(),
            shortcuts: applied.shortcuts.clone(),
            applied,
            shortcut_recording: None,
            shortcut_runtime_errors: Vec::new(),
            shortcut_runtime_bindings: rapidhash::RapidHashMap::default(),
            is_refreshing: false,
            preference_save_revisions: Default::default(),
            catalog_revision: 0,
            model_selection_write_state,
            refresh_task: None,
            refresh_window_scoped: false,
            write_tasks: Vec::new(),
            provider_settings_subscription: None,
            persona_settings_subscription: None,
            retired_provider_settings_editors: Vec::new(),
            retired_persona_settings_editors: Vec::new(),
            custom_frame_rate_subscription: None,
            custom_frame_rate_input_revision: 0,
            custom_frame_rate_save_task: None,
            logging_input_subscriptions: Vec::new(),
            appearance_input_subscriptions: Vec::new(),
            model_resource_name_subscription: None,
            shortcut_focus_subscription: None,
            logging_input_revision: 0,
            logging_save_task: None,
            screenshot_permission_revision: 0,
            toast_revision: 0,
            toast_task: None,
            voice_save_revision: 0,
            shortcut_save_revision: 0,
            model_resource_save_revision: 0,
            capabilities_revision: 0,
            #[cfg(test)]
            persona_live2d_refresh_revision: 0,
            #[cfg(test)]
            persona_live2d_candidate_count: 0,
            #[cfg(test)]
            emitted_settings_events: Vec::new(),
        };
        if let Some(status) = status {
            view.set_status(status, cx);
        }
        view.start_pending_persona_cleanup(cx);
        view
    }

    /// 启动时幂等清理持久化 tombstone，避免必须再次打开设置窗口才删除旧记忆。
    fn start_pending_persona_cleanup(&mut self, cx: &mut Context<Self>) {
        let memory = self.agent.memory();
        for persona in CONFIG.persona_settings().pending_deletions.clone() {
            let memory = memory.clone();
            if !memory.claim_deleted_persona_cleanup(&persona) {
                continue;
            }
            let cleanup_memory = memory.clone();
            let cleanup_persona = persona.clone();
            let cleanup = gpui_tokio::Tokio::spawn(cx, async move {
                cleanup_memory
                    .cleanup_deleted_persona(&cleanup_persona)
                    .await
            });
            let task = cx.spawn(async move |this, cx| match cleanup.await {
                Ok(Ok(())) => {
                    memory.complete_deleted_persona_cleanup(&persona);
                    log::info!("event=deleted_persona_memory_cleanup_completed phase=startup");
                    let _ = this.update(cx, |this, cx| {
                        this.finish_deleted_persona_cleanup(persona, cx);
                    });
                }
                Ok(Err(_)) => {
                    memory.fail_deleted_persona_cleanup(&persona);
                    log::error!("event=deleted_persona_memory_cleanup_failed phase=startup");
                }
                Err(_) => {
                    memory.fail_deleted_persona_cleanup(&persona);
                    log::error!(
                        "event=deleted_persona_memory_cleanup_failed phase=startup reason=task_join"
                    );
                }
            });
            self.write_tasks.push(task);
        }
    }

    fn finish_deleted_persona_cleanup(&mut self, _persona: String, cx: &mut Context<Self>) {
        let completed = self
            .agent
            .memory()
            .completed_deleted_persona_cleanups()
            .into_iter()
            .filter(|persona| {
                CONFIG
                    .persona_settings()
                    .pending_deletions
                    .contains(persona)
            })
            .collect::<Vec<_>>();
        if let Some(active) = self.persona_settings_view.clone() {
            active.update(cx, |active, cx| {
                for persona in &completed {
                    active.finish_persona_cleanup(persona, cx);
                }
            });
            self.release_published_persona_cleanups(cx);
            return;
        }

        let draft = self
            .persona_settings_draft
            .get_or_insert_with(PersonaSettingsDraft::current);
        let mut changed = false;
        for persona in &completed {
            changed |= draft.finish_persona_cleanup(persona);
        }
        if !changed {
            self.release_published_persona_cleanups(cx);
            return;
        }
        let Some(write) = draft.prepare_write() else {
            self.release_published_persona_cleanups(cx);
            return;
        };
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background.spawn(async move { write.persist() }).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.release_published_persona_cleanups(cx),
                Err(_) => log::error!("event=persona_tombstone_remove_failed"),
            });
        });
        self.track_write_task(task);
    }

    fn release_published_persona_cleanups(&mut self, cx: &mut Context<Self>) {
        let pending = CONFIG.persona_settings().pending_deletions.clone();
        let published = self
            .agent
            .memory()
            .completed_deleted_persona_cleanups()
            .into_iter()
            .filter(|persona| !pending.contains(persona))
            .collect::<Vec<_>>();
        if published.is_empty() {
            return;
        }
        if let Some(active) = self.persona_settings_view.clone() {
            active.update(cx, |active, cx| {
                for persona in &published {
                    active.persona_cleanup_was_published(persona, cx);
                }
            });
        }
        if let Some(draft) = &mut self.persona_settings_draft {
            for persona in &published {
                draft.persona_cleanup_was_published(persona);
            }
        }
        let memory = self.agent.memory();
        for persona in published {
            memory.release_deleted_persona_cleanup(&persona);
        }
    }

    /// 设置窗口打开时创建输入组件，并把当前外观同步到全局主题。
    pub(crate) fn activate_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.allow_agent_screenshot = CONFIG.requested_allow_agent_screenshot();
        self.screenshot_permission_retry_required =
            CONFIG.agent_screenshot_permission_retry_required();
        self.appearance = CONFIG.appearance().as_ref().clone();
        self.applied.appearance = self.appearance.clone();
        apply_language(self.appearance.language);
        apply(&self.appearance, Some(window), cx);
        let shortcut_focus = cx.focus_handle();
        self.shortcut_focus_subscription =
            Some(cx.on_blur(&shortcut_focus, window, |this, _, cx| {
                this.stop_shortcut_recording(cx)
            }));
        self.shortcut_focus = Some(shortcut_focus);
        let draft = self
            .provider_settings_draft
            .take()
            .unwrap_or_else(ProviderSettingsDraft::current);
        let provider_settings_view = cx.new(|cx| ProviderSettingsView::new(draft, window, cx));
        self.activate_persona_settings(window, cx);
        let custom_accent_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.accent.clone())
        });
        let custom_background_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.background.clone())
        });
        self.appearance_input_subscriptions =
            [custom_accent_input.clone(), custom_background_input.clone()]
                .into_iter()
                .map(|appearance_input| {
                    cx.subscribe_in(
                        &appearance_input,
                        window,
                        |this, input, event: &InputEvent, window, cx| match event {
                            InputEvent::Focus => this.begin_input_edit(input, cx),
                            InputEvent::PressEnter { .. } => {
                                this.apply_custom_theme(window, cx);
                                window.blur();
                            }
                            InputEvent::Blur => this.finish_input_edit(input),
                            InputEvent::Change => {}
                        },
                    )
                })
                .collect();
        self.custom_accent_input = Some(custom_accent_input);
        self.custom_background_input = Some(custom_background_input);
        let custom_frame_rate = custom_frame_rate_seed(self.frame_rate);
        let custom_frame_rate_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(custom_frame_rate.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(0),
                })
                .step(1.0)
                .min(f64::from(CUSTOM_FRAME_RATE_MIN))
                .max(f64::from(CUSTOM_FRAME_RATE_MAX))
        });
        self.custom_frame_rate_subscription = Some(cx.subscribe_in(
            &custom_frame_rate_input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.schedule_custom_frame_rate_save(input, cx),
                InputEvent::PressEnter { .. } => {
                    this.commit_custom_frame_rate_input(input, window, cx);
                    window.blur();
                }
                InputEvent::Blur => {
                    this.finish_input_edit(input);
                    this.commit_custom_frame_rate_input(input, window, cx);
                }
                InputEvent::Focus => this.begin_input_edit(input, cx),
            },
        ));
        self.custom_frame_rate_input = Some(custom_frame_rate_input);
        let log_max_size_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(self.logging.max_size_mb.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(0),
                })
                .step(1.0)
                .min(f64::from(LOGGING_MIN_FILE_SIZE_MB))
                .max(f64::from(LOGGING_MAX_FILE_SIZE_MB))
        });
        let log_keep_files_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(self.logging.keep_files.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(0),
                })
                .step(1.0)
                .min(f64::from(LOGGING_MIN_KEEP_FILES))
                .max(f64::from(LOGGING_MAX_KEEP_FILES))
        });
        self.logging_input_subscriptions = vec![
            cx.subscribe_in(
                &log_max_size_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.schedule_logging_save(input, Self::set_log_max_size_from_input, cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        this.commit_logging_input(input, Self::set_log_max_size_from_input, cx);
                        window.blur();
                    }
                    InputEvent::Blur => {
                        this.finish_input_edit(input);
                        this.commit_logging_input(input, Self::set_log_max_size_from_input, cx);
                    }
                    InputEvent::Focus => this.begin_input_edit(input, cx),
                },
            ),
            cx.subscribe_in(
                &log_keep_files_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.schedule_logging_save(input, Self::set_log_keep_files_from_input, cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        this.commit_logging_input(input, Self::set_log_keep_files_from_input, cx);
                        window.blur();
                    }
                    InputEvent::Blur => {
                        this.finish_input_edit(input);
                        this.commit_logging_input(input, Self::set_log_keep_files_from_input, cx);
                    }
                    InputEvent::Focus => this.begin_input_edit(input, cx),
                },
            ),
        ];
        self.log_max_size_input = Some(log_max_size_input);
        self.log_keep_files_input = Some(log_keep_files_input);
        let model_resource_name_input = cx.new(|cx| InputState::new(window, cx));
        self.model_resource_name_subscription = Some(cx.subscribe_in(
            &model_resource_name_input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Focus => this.begin_input_edit(input, cx),
                InputEvent::PressEnter { .. } => {
                    this.commit_model_resource_name(input, cx);
                    window.blur();
                }
                InputEvent::Blur => {
                    this.finish_input_edit(input);
                    this.commit_model_resource_name(input, cx);
                }
                InputEvent::Change => {}
            },
        ));
        self.model_resource_name_input = Some(model_resource_name_input);
        // 供应商目录变化会改变人格可绑定的候选项，两个编辑器必须保持同步。
        self.provider_settings_subscription = Some(cx.subscribe(
            &provider_settings_view,
            |this, editor, event: &ProviderSettingsEvent, cx| match event {
                ProviderSettingsEvent::Saved => {
                    this.emit_settings_event(SettingsEvent::AgentChanged, cx);
                }
                ProviderSettingsEvent::SaveFinished => {
                    let editor_id = editor.entity_id();
                    this.retired_provider_settings_editors
                        .retain(|retired| retired.view.entity_id() != editor_id);
                }
            },
        ));
        self.provider_settings_view = Some(provider_settings_view);
        cx.notify();
    }

    fn begin_input_edit(&mut self, input: &Entity<InputState>, cx: &Context<Self>) {
        self.input_edit = Some(InputEditSession::begin(input, cx));
    }

    fn finish_input_edit(&mut self, input: &Entity<InputState>) {
        if self
            .input_edit
            .as_ref()
            .is_some_and(|edit| edit.belongs_to(input))
        {
            self.input_edit = None;
        }
    }

    pub(super) fn handle_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.keystroke.key.eq_ignore_ascii_case("escape") {
            return;
        }
        let Some(edit) = self.input_edit.take() else {
            return;
        };
        window.prevent_default();
        cx.stop_propagation();
        edit.restore(window, cx);
    }

    fn activate_persona_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.write_tasks.retain(|task| !task.is_ready());
        let draft = self
            .persona_settings_draft
            .take()
            .unwrap_or_else(PersonaSettingsDraft::current);
        let memory = self.agent.memory();
        let live2d_models = self.persona_live2d_models();
        let view = cx.new(|cx| PersonaSettingsView::new(draft, memory, live2d_models, window, cx));
        self.persona_settings_subscription = Some(cx.subscribe(
            &view,
            |this, editor, event: &PersonaSettingsEvent, cx| match event {
                PersonaSettingsEvent::Saved => {
                    this.emit_settings_event(SettingsEvent::AgentChanged, cx);
                    this.apply_active_persona_live2d_model(cx);
                }
                PersonaSettingsEvent::SaveFinished => {
                    let editor_id = editor.entity_id();
                    this.retired_persona_settings_editors
                        .retain(|retired| retired.view.entity_id() != editor_id);
                }
                PersonaSettingsEvent::CleanupFinished { persona } => {
                    this.finish_deleted_persona_cleanup(persona.clone(), cx);
                }
                PersonaSettingsEvent::ClearContext {
                    persona,
                    completion,
                } => this.clear_agent_context(persona, completion.clone(), cx),
                PersonaSettingsEvent::EditContextMessage {
                    persona,
                    message_id,
                    content,
                    completion,
                } => this.edit_agent_context_message(
                    persona,
                    *message_id,
                    content.clone(),
                    completion.clone(),
                    cx,
                ),
                PersonaSettingsEvent::DeleteContextMessages {
                    persona,
                    message_ids,
                    completion,
                } => this.delete_agent_context_messages(
                    persona,
                    message_ids.clone(),
                    completion.clone(),
                    cx,
                ),
            },
        ));
        self.persona_settings_view = Some(view.clone());
        let completed = CONFIG
            .persona_settings()
            .pending_deletions
            .iter()
            .filter(|persona| {
                self.agent
                    .memory()
                    .deleted_persona_cleanup_is_completed(persona)
            })
            .cloned()
            .collect::<Vec<_>>();
        view.update(cx, |view, cx| {
            for persona in completed {
                view.finish_persona_cleanup(&persona, cx);
            }
            view.resume_pending_work(cx);
        });
        self.release_published_persona_cleanups(cx);
    }

    fn clear_agent_context(
        &self,
        persona: &str,
        completion: Option<ContextMutationCompletion>,
        cx: &Context<Self>,
    ) {
        let agent = self.agent.clone();
        let persona = persona.to_owned();
        gpui_tokio::Tokio::spawn(cx, async move {
            let result = agent
                .clear_context(&persona)
                .await
                .map_err(|error| error.to_string());
            complete_agent_context_mutation(completion.as_ref(), result);
        })
        .detach();
    }

    fn edit_agent_context_message(
        &self,
        persona: &str,
        message_id: u64,
        content: String,
        completion: Option<ContextMutationCompletion>,
        cx: &Context<Self>,
    ) {
        let Some(limits) = agent_context_limits(persona) else {
            complete_agent_context_mutation(completion.as_ref(), Err("人格不存在".to_owned()));
            return;
        };
        let agent = self.agent.clone();
        let persona = persona.to_owned();
        gpui_tokio::Tokio::spawn(cx, async move {
            let result = agent
                .edit_context_message(&persona, limits, message_id, content)
                .await
                .map_err(|error| error.to_string());
            complete_agent_context_mutation(completion.as_ref(), result);
        })
        .detach();
    }

    fn delete_agent_context_messages(
        &self,
        persona: &str,
        message_ids: Vec<u64>,
        completion: Option<ContextMutationCompletion>,
        cx: &Context<Self>,
    ) {
        let Some(limits) = agent_context_limits(persona) else {
            complete_agent_context_mutation(completion.as_ref(), Err("人格不存在".to_owned()));
            return;
        };
        let agent = self.agent.clone();
        let persona = persona.to_owned();
        gpui_tokio::Tokio::spawn(cx, async move {
            let result = agent
                .delete_context_messages(&persona, limits, message_ids)
                .await
                .map_err(|error| error.to_string());
            complete_agent_context_mutation(completion.as_ref(), result);
        })
        .detach();
    }

    /// 设置窗口关闭时丢弃绑定到旧窗口的输入状态。
    pub(crate) fn deactivate_window(&mut self, cx: &mut Context<Self>) {
        self.stop_shortcut_recording(cx);
        if self.is_refreshing && self.refresh_window_scoped {
            self.catalog_revision = self.catalog_revision.wrapping_add(1);
            self.refresh_task = None;
            self.is_refreshing = false;
            self.refresh_window_scoped = false;
        }
        if let Some(provider_settings_view) = self.provider_settings_view.take() {
            let (draft, pending) =
                provider_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.provider_settings_draft = Some(draft);
            let has_pending = pending.iter().any(|task| !task.is_ready());
            let subscription = self.provider_settings_subscription.take();
            if has_pending && let Some(subscription) = subscription {
                self.retired_provider_settings_editors
                    .push(RetiredProviderSettingsEditor {
                        view: provider_settings_view,
                        _subscription: subscription,
                    });
            }
            self.write_tasks.extend(pending);
        }
        if let Some(persona_settings_view) = self.persona_settings_view.take() {
            let (draft, pending, retain_editor) =
                persona_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.persona_settings_draft = Some(draft);
            let has_pending = retain_editor && pending.iter().any(|task| !task.is_ready());
            let subscription = self.persona_settings_subscription.take();
            if has_pending && let Some(subscription) = subscription {
                self.retired_persona_settings_editors
                    .push(RetiredPersonaSettingsEditor {
                        view: persona_settings_view,
                        _subscription: subscription,
                    });
            }
            self.write_tasks.extend(pending);
        }
        self.flush_custom_frame_rate_input(cx);
        self.flush_logging_inputs(cx);
        if let Some(input) = self.model_resource_name_input.clone() {
            self.commit_model_resource_name(&input, cx);
        }
        self.custom_accent_input = None;
        self.custom_background_input = None;
        self.custom_frame_rate_input = None;
        self.custom_frame_rate_save_task = None;
        self.log_max_size_input = None;
        self.log_keep_files_input = None;
        self.model_resource_name_input = None;
        self.input_edit = None;
        self.model_resource_name_subscription = None;
        self.editing_model_resource = None;
        self.shortcut_focus = None;
        self.shortcut_focus_subscription = None;
        self.provider_settings_subscription = None;
        self.persona_settings_subscription = None;
        self.custom_frame_rate_subscription = None;
        self.logging_input_subscriptions.clear();
        self.appearance_input_subscriptions.clear();
        cx.notify();
    }

    /// 当前设置窗口接到 Agent 配置发布事件后刷新人格可绑定的 Provider 候选。
    pub(crate) fn refresh_persona_providers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(persona) = &self.persona_settings_view {
            persona.update(cx, |persona, cx| persona.refresh_providers(window, cx));
        }
    }

    /// 取出设置主体、供应商与人格编辑器中尚未完成的写入任务。
    pub(crate) fn take_pending_write_tasks(&mut self, cx: &mut Context<Self>) -> Vec<Task<()>> {
        self.flush_custom_frame_rate_input(cx);
        self.flush_logging_inputs(cx);
        if let Some(input) = self.model_resource_name_input.clone() {
            self.commit_model_resource_name(&input, cx);
        }
        if let Some(provider_settings_view) = &self.provider_settings_view {
            let provider_settings_view = provider_settings_view.clone();
            let (draft, pending) =
                provider_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.provider_settings_draft = Some(draft);
            self.write_tasks.extend(pending);
        }
        if let Some(persona_settings_view) = &self.persona_settings_view {
            let persona_settings_view = persona_settings_view.clone();
            let (draft, pending, _) =
                persona_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.persona_settings_draft = Some(draft);
            self.write_tasks.extend(pending);
        }
        if let Some(write) = self
            .persona_settings_draft
            .as_ref()
            .and_then(PersonaSettingsDraft::prepare_write)
        {
            let background = cx.background_executor().clone();
            let task = cx.spawn(async move |_, _| {
                let result = background.spawn(async move { write.persist() }).await;
                if result.is_err() {
                    log::error!("event=persona_draft_save_failed phase=shutdown");
                }
            });
            self.write_tasks.push(task);
        }
        std::mem::take(&mut self.write_tasks)
    }

    pub(super) fn track_write_task(&mut self, task: Task<()>) {
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
    }

    pub(super) fn emit_settings_event(&mut self, event: SettingsEvent, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.emitted_settings_events.push(event.clone());
        cx.emit(event);
    }

    pub(super) fn set_section(&mut self, section: ConfigSection, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        if self.section == ConfigSection::Shortcut {
            self.stop_shortcut_recording(cx);
        }
        self.section = section;
        cx.notify();
    }

    pub(super) fn set_status(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        const TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_millis(3_000);

        self.toast_revision = self.toast_revision.wrapping_add(1).max(1);
        let revision = self.toast_revision;
        self.status = Some(message.into());
        let background = cx.background_executor().clone();
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            background.timer(TOAST_LIFETIME).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast_revision == revision {
                    this.status = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }
}

fn agent_context_limits(persona_id: &str) -> Option<lunamate_agent::ChatLimits> {
    let settings = CONFIG.llm_settings();
    CONFIG
        .persona_settings()
        .personas
        .iter()
        .find(|persona| persona.id == persona_id)
        .map(|persona| chat_limits(persona, &settings))
}

fn complete_agent_context_mutation(
    completion: Option<&ContextMutationCompletion>,
    result: Result<(), String>,
) {
    if let Some(completion) = completion {
        let _ = completion.try_send(result);
    }
}
