//! 处理上下文消息的键盘、多选、框选、复制与边缘自动滚动交互。

use std::{collections::HashSet, time::Duration};

use gpui::{
    Bounds, ClipboardItem, Context, KeyDownEvent, Modifiers, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Size, Window,
};
use rust_i18n::t;

use super::{
    ContextMessageEditor, ContextSelectionDrag, PersonaPage, PersonaSettingsView,
    memory::PendingConfirm,
};

const CONTEXT_SELECTION_AUTO_SCROLL_INTERVAL: Duration = Duration::from_millis(16);
const CONTEXT_SELECTION_AUTO_SCROLL_EDGE: f32 = 40.0;
const CONTEXT_SELECTION_AUTO_SCROLL_MAX_STEP: f32 = 24.0;

impl PersonaSettingsView {
    pub(super) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        if key.eq_ignore_ascii_case("escape") {
            if self.pending_confirm.is_some() {
                window.prevent_default();
                cx.stop_propagation();
                self.cancel_confirm(cx);
                return;
            }
            if let Some(message_id) = self.context_editing {
                window.prevent_default();
                cx.stop_propagation();
                self.cancel_context_message_edit(message_id, window, cx);
                return;
            }
            if let Some(edit) = self.input_edit.take() {
                window.prevent_default();
                cx.stop_propagation();
                self.loading_form = true;
                edit.restore(window, cx);
                self.loading_form = false;
                return;
            }
            if self.context_selection_drag.is_some() || !self.context_selected.is_empty() {
                window.prevent_default();
                cx.stop_propagation();
                self.cancel_context_selection();
                self.context_selected.clear();
                cx.notify();
            }
            return;
        }
        if self.active_page != PersonaPage::Context
            || self.context_editing.is_some()
            || self.pending_confirm.is_some()
        {
            return;
        }
        if event.keystroke.modifiers.alt && key.eq_ignore_ascii_case("a") {
            window.prevent_default();
            cx.stop_propagation();
            self.select_all_context_messages(cx);
        } else if key.eq_ignore_ascii_case("delete") && !self.context_selected.is_empty() {
            window.prevent_default();
            cx.stop_propagation();
            self.request_delete_selected_context_messages(cx);
        }
    }

    fn select_all_context_messages(&mut self, cx: &mut Context<Self>) {
        self.cancel_context_selection();
        self.context_selected = self
            .context_editors
            .iter()
            .map(|message| message.id)
            .collect();
        cx.notify();
    }

    pub(super) fn prepare_context_menu_selection(
        &mut self,
        message_id: u64,
        cx: &mut Context<Self>,
    ) {
        self.cancel_context_selection();
        if !self.context_selected.contains(&message_id) {
            self.context_selected.clear();
            self.context_selected.insert(message_id);
        }
        cx.notify();
    }

    pub(super) fn copy_selected_context_messages(&mut self, cx: &mut Context<Self>) {
        let text = self
            .context_editors
            .iter()
            .filter(|message| self.context_selected.contains(&message.id))
            .map(|message| message.input.read(cx).value().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status(t!("persona.context_copied").to_string(), cx);
    }

    pub(super) fn start_context_selection_drag(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_loading {
            return;
        }
        self.context_focus.focus(window, cx);
        let content_position = event.position - self.context_scroll.offset();
        let anchor_id = self
            .context_editors
            .iter()
            .find(|message| {
                self.context_message_bounds(message)
                    .contains(&content_position)
            })
            .map(|message| message.id);
        self.cancel_context_selection();
        self.context_selection_drag = Some(ContextSelectionDrag {
            anchor_id,
            base: self.context_selected.clone(),
            start: content_position,
            current: content_position,
            cursor: event.position,
            moved: false,
            additive: selection_is_additive(event.modifiers),
        });
        cx.notify();
    }

    fn update_context_selection_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &mut self.context_selection_drag else {
            return;
        };
        // 滚轮和触控板滚动后，部分平台会发送不携带按键状态的过渡移动事件；
        // 忽略它但保留框选，手势只由明确的 MouseUp 结束。
        if !event.dragging() {
            return;
        }
        let content_position = event.position - self.context_scroll.offset();
        drag.current = content_position;
        drag.cursor = event.position;
        let dx = f32::from(content_position.x - drag.start.x).abs();
        let dy = f32::from(content_position.y - drag.start.y).abs();
        drag.moved |= dx >= 3.0 || dy >= 3.0;
        if !drag.moved {
            cx.notify();
            return;
        }
        self.recompute_context_selection();
        self.schedule_context_selection_auto_scroll(window, cx);
        cx.notify();
    }

    pub(super) fn context_selection_scrolled(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &mut self.context_selection_drag else {
            return;
        };
        drag.current = drag.cursor - self.context_scroll.offset();
        self.recompute_context_selection();
        self.schedule_context_selection_auto_scroll(window, cx);
        cx.notify();
    }

    fn schedule_context_selection_auto_scroll(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &self.context_selection_drag else {
            return;
        };
        if self.context_selection_auto_scroll_task.is_some()
            || !drag.moved
            || context_selection_auto_scroll_delta(drag.cursor, self.context_view_bounds.get())
                == Pixels::ZERO
        {
            return;
        }
        let generation = self.context_selection_auto_scroll_revision;
        let background = cx.background_executor().clone();
        self.context_selection_auto_scroll_task =
            Some(cx.spawn_in(window, async move |this, cx| {
                background
                    .timer(CONTEXT_SELECTION_AUTO_SCROLL_INTERVAL)
                    .await;
                let _ = cx.update(|window, app| {
                    let _ = this.update(app, |this, cx| {
                        if this.context_selection_auto_scroll_revision != generation {
                            return;
                        }
                        this.context_selection_auto_scroll_task = None;
                        if this.advance_context_selection_auto_scroll(cx) {
                            this.schedule_context_selection_auto_scroll(window, cx);
                        }
                    });
                });
            }));
    }

    fn advance_context_selection_auto_scroll(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(drag) = &self.context_selection_drag else {
            return false;
        };
        if !drag.moved {
            return false;
        }
        let delta =
            context_selection_auto_scroll_delta(drag.cursor, self.context_view_bounds.get());
        if delta == Pixels::ZERO {
            return false;
        }
        let offset = self.context_scroll.offset();
        let max_offset = self.context_scroll.max_offset();
        let next_y = (offset.y + delta).clamp(-max_offset.y, Pixels::ZERO);
        if next_y == offset.y {
            return false;
        }
        let next_offset = Point::new(offset.x, next_y);
        self.context_scroll.set_offset(next_offset);
        if let Some(drag) = &mut self.context_selection_drag {
            drag.current = drag.cursor - next_offset;
        }
        self.recompute_context_selection();
        cx.notify();
        true
    }

    fn stop_context_selection_auto_scroll(&mut self) {
        self.context_selection_auto_scroll_revision = self
            .context_selection_auto_scroll_revision
            .wrapping_add(1)
            .max(1);
        self.context_selection_auto_scroll_task = None;
    }

    pub(super) fn cancel_context_selection(&mut self) {
        self.context_selection_drag = None;
        self.stop_context_selection_auto_scroll();
    }

    fn recompute_context_selection(&mut self) {
        let Some(drag) = &self.context_selection_drag else {
            return;
        };
        let selection = selection_bounds(drag.start, drag.current);
        let mut selected = if drag.additive {
            drag.base.clone()
        } else {
            HashSet::new()
        };
        selected.extend(
            self.context_editors
                .iter()
                .filter(|message| self.context_message_bounds(message).intersects(&selection))
                .map(|message| message.id),
        );
        self.context_selected = selected;
    }

    fn context_message_bounds(&self, message: &ContextMessageEditor) -> Bounds<Pixels> {
        let layout = message.layout.get();
        Bounds::new(
            layout.bounds.origin - layout.scroll_offset,
            layout.bounds.size,
        )
    }

    pub(super) fn update_context_selection_position(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_context_selection_drag(event, window, cx);
    }

    pub(super) fn finish_context_selection_drag(
        &mut self,
        _event: &MouseUpEvent,
        cx: &mut Context<Self>,
    ) {
        self.stop_context_selection_auto_scroll();
        let Some(drag) = self.context_selection_drag.take() else {
            return;
        };
        if !drag.moved {
            self.context_selected = if drag.additive {
                drag.base
            } else {
                HashSet::new()
            };
            if let Some(anchor_id) = drag.anchor_id
                && !self.context_selected.insert(anchor_id)
            {
                self.context_selected.remove(&anchor_id);
            }
        }
        cx.notify();
    }

    pub(super) fn request_delete_selected_context_messages(&mut self, cx: &mut Context<Self>) {
        let message_ids = self
            .context_editors
            .iter()
            .filter(|message| self.context_selected.contains(&message.id))
            .map(|message| message.id)
            .collect::<Vec<_>>();
        self.request_delete_context_messages(message_ids, cx);
    }

    fn request_delete_context_messages(&mut self, message_ids: Vec<u64>, cx: &mut Context<Self>) {
        if self.context_loading || message_ids.is_empty() {
            return;
        }
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
        else {
            return;
        };
        let mut seen = HashSet::with_capacity(message_ids.len());
        let message_ids = message_ids
            .into_iter()
            .filter(|message_id| {
                seen.insert(*message_id)
                    && self
                        .context_editors
                        .iter()
                        .any(|message| message.id == *message_id)
            })
            .collect::<Vec<_>>();
        if message_ids.is_empty() {
            return;
        }
        self.pending_confirm = Some(PendingConfirm::DeleteContextMessages {
            persona: persona.id.clone(),
            message_ids,
        });
        cx.notify();
    }

    /// 切换一条上下文消息的多选状态。
    #[cfg(test)]
    pub(crate) fn toggle_context_message_selected_for_test(
        &mut self,
        message_id: u64,
        cx: &mut Context<Self>,
    ) {
        if !self.context_selected.insert(message_id) {
            self.context_selected.remove(&message_id);
        }
        cx.notify();
    }

    /// 请求删除当前多选集合，但不进行确认。
    #[cfg(test)]
    pub(crate) fn request_delete_selected_context_messages_for_test(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.request_delete_selected_context_messages(cx);
    }

    /// 返回当前多选消息数量。
    #[cfg(test)]
    pub(crate) fn selected_context_messages_for_test(&self) -> usize {
        self.context_selected.len()
    }

    /// 返回框选手势是否仍在进行，供测试覆盖滚动后的生命周期。
    #[cfg(test)]
    pub(crate) fn context_selection_active_for_test(&self) -> bool {
        self.context_selection_drag.is_some()
    }

    /// 返回边缘自动滚动定时任务是否已启动。
    #[cfg(test)]
    pub(crate) fn context_selection_auto_scroll_scheduled_for_test(&self) -> bool {
        self.context_selection_auto_scroll_task.is_some()
    }

    /// 执行一次边缘滚动，测试定时任务所调用的状态转换。
    #[cfg(test)]
    pub(crate) fn advance_context_selection_auto_scroll_for_test(
        &mut self,
        cx: &mut Context<Self>,
    ) -> bool {
        self.advance_context_selection_auto_scroll(cx)
    }

    /// 复制当前选择，供无头测试验证多选顺序与剪贴板内容。
    #[cfg(test)]
    pub(crate) fn copy_selected_context_messages_for_test(&mut self, cx: &mut Context<Self>) {
        self.copy_selected_context_messages(cx);
    }

    /// 返回上下文列表滚动偏移与最大偏移，供无头渲染验证默认置底。
    #[cfg(test)]
    pub(crate) fn context_scroll_for_test(&self) -> (gpui::Pixels, gpui::Pixels) {
        (
            self.context_scroll.offset().y,
            self.context_scroll.max_offset().y,
        )
    }
}

fn selection_bounds(start: Point<Pixels>, current: Point<Pixels>) -> Bounds<Pixels> {
    let left = start.x.min(current.x);
    let top = start.y.min(current.y);
    let width = (start.x - current.x).abs().max(Pixels::from(1.0));
    let height = (start.y - current.y).abs().max(Pixels::from(1.0));
    Bounds::new(Point::new(left, top), Size::new(width, height))
}

fn context_selection_auto_scroll_delta(cursor: Point<Pixels>, viewport: Bounds<Pixels>) -> Pixels {
    let edge = Pixels::from(CONTEXT_SELECTION_AUTO_SCROLL_EDGE);
    let top_edge = viewport.origin.y + edge;
    if cursor.y < top_edge {
        let proximity =
            (f32::from(top_edge - cursor.y) / CONTEXT_SELECTION_AUTO_SCROLL_EDGE).clamp(0.0, 1.0);
        return Pixels::from(CONTEXT_SELECTION_AUTO_SCROLL_MAX_STEP * proximity);
    }

    let bottom_edge = viewport.origin.y + viewport.size.height - edge;
    if cursor.y > bottom_edge {
        let proximity = (f32::from(cursor.y - bottom_edge) / CONTEXT_SELECTION_AUTO_SCROLL_EDGE)
            .clamp(0.0, 1.0);
        return Pixels::from(-CONTEXT_SELECTION_AUTO_SCROLL_MAX_STEP * proximity);
    }

    Pixels::ZERO
}

fn selection_is_additive(modifiers: Modifiers) -> bool {
    modifiers.shift || modifiers.secondary()
}
