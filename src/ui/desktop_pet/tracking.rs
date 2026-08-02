//! 驱动窗口内外的光标采样，并发布模型视线目标。

use gpui::{Context, Pixels, Point, Window};

use super::{CURSOR_TRACKING_INTERVAL, DesktopPetView, look_target_for_position};

impl DesktopPetView {
    pub(super) fn update_look_target(&self, position: Point<Pixels>, window: &Window) {
        if !self.eye_tracking_enabled || self.frame.is_none() || self.chat_input_open {
            return;
        }
        let look = look_target_for_position(position, window.viewport_size());
        let mut target = self.look_target.lock();
        if *target == look {
            return;
        }
        *target = look;
        drop(target);
        self.wake_model();
    }

    fn update_global_cursor_target(&self, window: &Window) {
        let position = self
            .global_cursor_tracker
            .as_ref()
            .and_then(|tracker| tracker.position(window));
        if let Some(position) = position {
            self.update_look_target(position, window);
        } else {
            self.reset_look_target();
        }
    }

    pub(super) fn handle_mouse_exit(&self, window: &Window) {
        self.update_global_cursor_target(window);
    }

    fn should_track_global_cursor(&self) -> bool {
        self.global_cursor_tracker.is_some()
            && self.eye_tracking_enabled
            && self.desktop_pet_visible
            && !self.chat_input_open
            && self.frame.is_some()
            && !self.close_after_gpu_shutdown
            && !self.quitting
            && !self.gpu_shutdown_pending
    }

    pub(super) fn sync_cursor_tracking_task(&mut self, cx: &mut Context<Self>) {
        if !self.should_track_global_cursor() {
            self.cursor_tracking_task = None;
            return;
        }
        if self.cursor_tracking_task.is_some() {
            return;
        }

        let background = cx.background_executor().clone();
        self.cursor_tracking_task = Some(cx.spawn(async move |this, cx| {
            loop {
                background.timer(CURSOR_TRACKING_INTERVAL).await;
                let keep_running = this
                    .update_in(cx, |this, window, _| {
                        if !this.should_track_global_cursor() {
                            return false;
                        }
                        this.update_global_cursor_target(window);
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    pub(super) fn reset_look_target(&self) {
        let mut target = self.look_target.lock();
        if *target == [0.0, 0.0] {
            return;
        }
        *target = [0.0, 0.0];
        drop(target);
        self.wake_model();
    }
}
