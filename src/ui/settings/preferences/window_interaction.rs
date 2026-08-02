//! 持久化帧调度、桌宠窗口与本地交互设置。

use std::time::Duration;

use gpui::{Context, Entity, Window};
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::config::{CONFIG, ConfigWriteError, FrameRate, ModelWindowSize};

use super::super::{SettingsEvent, SettingsView, next_save_revision};

const CUSTOM_FRAME_RATE_SAVE_DELAY: Duration = Duration::from_millis(250);

impl SettingsView {
    pub(in crate::ui::settings) fn set_frame_rate(
        &mut self,
        frame_rate: FrameRate,
        cx: &mut Context<Self>,
    ) {
        if !matches!(frame_rate, FrameRate::Custom(_)) {
            self.custom_frame_rate_input_revision =
                self.custom_frame_rate_input_revision.wrapping_add(1);
            self.custom_frame_rate_save_task = None;
        }
        if self.frame_rate == frame_rate {
            return;
        }
        self.frame_rate = frame_rate;
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.frame_rate);
        cx.notify();

        let config_revision = CONFIG.reserve_frame_rate_revision();
        self.persist_setting(
            move || CONFIG.set_frame_rate_at_revision(frame_rate, config_revision),
            move |this, result, cx| {
                this.finish_frame_rate_write(ui_revision, frame_rate, result, cx);
            },
            cx,
        );
    }

    fn finish_frame_rate_write(
        &mut self,
        ui_revision: u64,
        requested: FrameRate,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(())) if CONFIG.frame_rate() == requested => {
                self.applied.frame_rate = requested;
                self.emit_settings_event(SettingsEvent::FrameRateChanged, cx);
                cx.notify();
            }
            Ok(Some(())) | Ok(None) => {}
            Err(error) if self.preference_save_revisions.frame_rate == ui_revision => {
                self.frame_rate = CONFIG.frame_rate();
                self.set_status(
                    t!("status.frame_rate_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn select_custom_frame_rate(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.frame_rate, FrameRate::Custom(_)) {
            return;
        }
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        self.custom_frame_rate_save_task = None;
        let fps = custom_frame_rate_seed(self.frame_rate);
        if let Some(input) = &self.custom_frame_rate_input
            && input.read(cx).value() != fps.to_string()
        {
            input.update(cx, |input, cx| {
                input.set_value(fps.to_string(), window, cx);
            });
        }
        if let Ok(frame_rate) = FrameRate::custom(fps) {
            self.set_frame_rate(frame_rate, cx);
        }
    }

    pub(in crate::ui::settings) fn schedule_custom_frame_rate_save(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.frame_rate, FrameRate::Custom(_)) {
            return;
        }
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        let revision = self.custom_frame_rate_input_revision;
        self.custom_frame_rate_save_task = None;
        let input = input.clone();
        let background = cx.background_executor().clone();
        self.custom_frame_rate_save_task = Some(cx.spawn(async move |this, cx| {
            background.timer(CUSTOM_FRAME_RATE_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.custom_frame_rate_input_revision == revision {
                    this.apply_custom_frame_rate_input(&input, cx);
                }
            });
        }));
    }

    pub(in crate::ui::settings) fn commit_custom_frame_rate_input(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        self.custom_frame_rate_save_task = None;
        if self.apply_custom_frame_rate_input(input, cx) {
            let Some(fps) = self.frame_rate.limit() else {
                return;
            };
            if input.read(cx).value() != fps.to_string() {
                input.update(cx, |input, cx| {
                    input.set_value(fps.to_string(), window, cx);
                });
            }
            return;
        }
        let Some(fps) = self.frame_rate.limit() else {
            return;
        };
        input.update(cx, |input, cx| {
            input.set_value(fps.to_string(), window, cx);
        });
    }

    fn apply_custom_frame_rate_input(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.frame_rate, FrameRate::Custom(_)) {
            return false;
        }
        let Some(frame_rate) = parse_custom_frame_rate(&input.read(cx).value()) else {
            return false;
        };
        self.set_frame_rate(frame_rate, cx);
        true
    }

    pub(in crate::ui::settings) fn flush_custom_frame_rate_input(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        self.custom_frame_rate_save_task = None;
        if let Some(input) = self.custom_frame_rate_input.clone() {
            self.apply_custom_frame_rate_input(&input, cx);
        }
    }

    pub(in crate::ui::settings) fn set_model_window_size(
        &mut self,
        size: ModelWindowSize,
        cx: &mut Context<Self>,
    ) {
        if self.model_window_size == size {
            return;
        }
        self.model_window_size = size;
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.model_window_size);
        cx.notify();
        let config_revision = CONFIG.reserve_model_window_size_revision();
        self.persist_setting(
            move || CONFIG.set_model_window_size_at_revision(size, config_revision),
            move |this, result, cx| {
                this.finish_model_window_size_write(ui_revision, size, result, cx);
            },
            cx,
        );
    }

    pub(in crate::ui::settings) fn finish_model_window_size_write(
        &mut self,
        ui_revision: u64,
        requested: ModelWindowSize,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(())) if CONFIG.model_window_size() == requested => {
                self.applied.model_window_size = requested;
                self.emit_settings_event(SettingsEvent::ModelWindowSizeChanged(requested), cx);
                cx.notify();
            }
            Ok(Some(())) | Ok(None) => {}
            Err(error) if self.preference_save_revisions.model_window_size == ui_revision => {
                self.model_window_size = CONFIG.model_window_size();
                self.set_status(
                    t!("status.setting_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn set_remember_window_positions(
        &mut self,
        remember: bool,
        cx: &mut Context<Self>,
    ) {
        if self.remember_window_positions == remember {
            return;
        }
        self.remember_window_positions = remember;
        let ui_revision =
            next_save_revision(&mut self.preference_save_revisions.remember_window_positions);
        cx.notify();

        let config_revision = CONFIG.reserve_remember_positions_revision();
        self.persist_setting(
            move || CONFIG.set_remember_window_positions_at_revision(remember, config_revision),
            move |this, result, cx| {
                this.finish_remember_window_positions_write(ui_revision, remember, result, cx);
            },
            cx,
        );
    }

    fn finish_remember_window_positions_write(
        &mut self,
        ui_revision: u64,
        requested: bool,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(())) if CONFIG.remember_window_positions() == requested => {
                self.applied.remember_window_positions = requested;
                cx.notify();
            }
            Ok(Some(())) | Ok(None) => {}
            Err(error)
                if self.preference_save_revisions.remember_window_positions == ui_revision =>
            {
                self.remember_window_positions = CONFIG.remember_window_positions();
                self.set_status(
                    t!("status.setting_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn set_eye_tracking(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if self.eye_tracking == enabled {
            return;
        }
        self.eye_tracking = enabled;
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.eye_tracking);
        cx.notify();

        let config_revision = CONFIG.reserve_eye_tracking_revision();
        self.persist_setting(
            move || CONFIG.set_eye_tracking_at_revision(enabled, config_revision),
            move |this, result, cx| {
                this.finish_eye_tracking_write(ui_revision, enabled, result, cx);
            },
            cx,
        );
    }

    pub(in crate::ui::settings) fn finish_eye_tracking_write(
        &mut self,
        ui_revision: u64,
        requested: bool,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(())) if CONFIG.eye_tracking() == requested => {
                self.applied.eye_tracking = requested;
                self.emit_settings_event(SettingsEvent::EyeTrackingChanged(requested), cx);
                cx.notify();
            }
            Ok(Some(())) | Ok(None) => {}
            Err(error) if self.preference_save_revisions.eye_tracking == ui_revision => {
                self.eye_tracking = CONFIG.eye_tracking();
                self.set_status(
                    t!("status.setting_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn set_show_fps(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_fps == show {
            return;
        }
        self.show_fps = show;
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.show_fps);
        cx.notify();

        let config_revision = CONFIG.reserve_show_fps_revision();
        self.persist_setting(
            move || CONFIG.set_show_fps_at_revision(show, config_revision),
            move |this, result, cx| {
                this.finish_show_fps_write(ui_revision, show, result, cx);
            },
            cx,
        );
    }

    fn finish_show_fps_write(
        &mut self,
        ui_revision: u64,
        requested: bool,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(())) if CONFIG.show_fps() == requested => {
                self.applied.show_fps = requested;
                self.emit_settings_event(SettingsEvent::ShowFpsChanged(requested), cx);
                cx.notify();
            }
            Ok(Some(())) | Ok(None) => {}
            Err(error) if self.preference_save_revisions.show_fps == ui_revision => {
                self.show_fps = CONFIG.show_fps();
                self.set_status(
                    t!("status.setting_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn set_use_native_tray_menu(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if self.use_native_tray_menu == enabled {
            return;
        }
        self.use_native_tray_menu = enabled;
        let ui_revision =
            next_save_revision(&mut self.preference_save_revisions.use_native_tray_menu);
        cx.notify();

        let config_revision = CONFIG.reserve_use_native_tray_menu_revision();
        self.persist_setting(
            move || CONFIG.set_use_native_tray_menu_at_revision(enabled, config_revision),
            move |this, result, cx| {
                this.finish_use_native_tray_menu_write(ui_revision, enabled, result, cx);
            },
            cx,
        );
    }

    pub(in crate::ui::settings) fn finish_use_native_tray_menu_write(
        &mut self,
        ui_revision: u64,
        requested: bool,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(())) if CONFIG.use_native_tray_menu() == requested => {
                self.applied.use_native_tray_menu = requested;
                self.emit_settings_event(SettingsEvent::NativeTrayMenuChanged(requested), cx);
                cx.notify();
            }
            Ok(Some(())) | Ok(None) => {}
            Err(error) if self.preference_save_revisions.use_native_tray_menu == ui_revision => {
                self.use_native_tray_menu = CONFIG.use_native_tray_menu();
                self.set_status(
                    t!("status.setting_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn reset_window_positions(&mut self, cx: &mut Context<Self>) {
        let ui_revision =
            next_save_revision(&mut self.preference_save_revisions.reset_window_positions);
        self.set_status(t!("status.positions_clearing").to_string(), cx);

        let config_revision = CONFIG.reserve_reset_positions_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move { CONFIG.reset_window_positions_at_revision(config_revision) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if matches!(&result, Ok(Some(()))) {
                    this.emit_settings_event(SettingsEvent::WindowPositionsReset, cx);
                }
                if this.preference_save_revisions.reset_window_positions == ui_revision {
                    let status = match result {
                        Ok(Some(())) => t!("status.positions_reset").to_string(),
                        Ok(None) => t!("status.position_reset_replaced").to_string(),
                        Err(error) => t!("status.position_reset_failed", error = error.to_string())
                            .to_string(),
                    };
                    this.set_status(status, cx);
                }
            });
        });
        self.track_write_task(task);
    }
}

pub(in crate::ui) fn parse_custom_frame_rate(value: &str) -> Option<FrameRate> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u16>()
        .ok()
        .and_then(|fps| FrameRate::custom(fps).ok())
}

pub(in crate::ui) fn custom_frame_rate_seed(frame_rate: FrameRate) -> u16 {
    frame_rate.limit().unwrap_or(60)
}
