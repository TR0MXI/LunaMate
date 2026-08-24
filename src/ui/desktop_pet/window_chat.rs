//! 管理聊天输入、设置窗口、托盘菜单及窗口外观同步。

use gpui::{
    AnyWindowHandle, App, AppContext, Context, Hsla, Styled, Window, WindowBackgroundAppearance,
    WindowDecorations, WindowKind, WindowOptions, transparent_black,
};
use gpui_component::Root;

use super::DesktopPetView;
use crate::{
    config::ConfigWindow,
    model::RenderedModelFrame,
    platform::{
        APPLICATION_ID, NativeTrayMenuWindow, SystemTray, TrayIconStyle, TrayMenuAnchor,
        configure_settings_window, configure_tray_menu_window, set_desktop_pet_window_visible,
    },
    ui::{
        SettingsWindowView, TrayMenuView, UiPalette, restored_window_bounds, settings_window_sizes,
        tray_menu_window_options,
    },
};

impl DesktopPetView {
    pub(super) fn sync_system_tray_appearance(&self, cx: &App) {
        sync_system_tray_appearance(self.system_tray.as_deref(), cx);
    }

    fn set_chat_input_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open {
            // 桌宠在 macOS 上是非激活 NSPanel，鼠标点击不会自动把应用设为活动应用；
            // 必须先激活窗口，再让 InputState 请求第一响应者，否则输入法事件仍会发送给
            // 之前的活动应用。
            window.activate_window();
        }
        if self.chat_input_open == open {
            if open {
                self.chat.update(cx, |chat, cx| {
                    chat.set_input_visible(true, window, cx);
                });
            }
            return;
        }
        self.chat_input_open = open;
        if open {
            self.reset_look_target();
        }
        self.sync_cursor_tracking_task(cx);
        self.chat.update(cx, |chat, cx| {
            chat.set_input_visible(open, window, cx);
        });
        cx.notify();
    }

    pub(super) fn toggle_chat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_chat_input_open(!self.chat_input_open, window, cx);
    }

    fn open_chat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.desktop_pet_visible {
            if set_desktop_pet_window_visible(window, true).is_err() {
                log::warn!("event=desktop_pet_visibility_change_failed source=shortcut");
                return;
            }
            self.set_desktop_pet_visible(true, window, cx);
        }
        window.activate_window();
        self.set_chat_input_open(true, window, cx);
    }

    pub(super) fn toggle_chat_input_from_shortcut(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.desktop_pet_visible && self.chat_input_open {
            self.set_chat_input_open(false, window, cx);
        } else {
            self.open_chat_input(window, cx);
        }
    }

    pub(crate) fn open_config_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.config_window
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }

        let (window_size, window_min_size) = settings_window_sizes(cx);
        let window_bounds = restored_window_bounds(ConfigWindow::Settings, window_size, cx);
        let config = self.config.clone();
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                window_min_size: Some(window_min_size),
                titlebar: None,
                focus: true,
                show: true,
                kind: WindowKind::Normal,
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                is_resizable: true,
                is_minimizable: true,
                is_movable: true,
                app_owns_titlebar_drag: true,
                app_id: Some(APPLICATION_ID.to_owned()),
                ..Default::default()
            },
            move |window, cx| {
                if configure_settings_window(window).is_err() {
                    log::warn!("event=settings_window_config_failed");
                }
                window.set_window_title("LunaMate");
                let view = cx.new(|cx| SettingsWindowView::new(config, window, cx));
                cx.new(|cx| {
                    Root::new(view, window, cx)
                        .bordered(false)
                        .bg(transparent_black())
                })
            },
        );
        match result {
            Ok(handle) => {
                self.config_window = Some(handle.into());
                log::info!("event=settings_window_opened");
            }
            Err(_) => log::error!("event=settings_window_create_failed"),
        }
    }

    pub(super) fn toggle_config_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.config_window.take()
            && handle
                .update(cx, |_, window, _| window.remove_window())
                .is_ok()
        {
            log::info!("event=settings_window_closed");
            return;
        }
        self.open_config_window(cx);
    }

    pub(crate) fn toggle_tray_menu(&mut self, anchor: TrayMenuAnchor, cx: &mut Context<Self>) {
        let Some(tray) = self.system_tray.clone() else {
            return;
        };
        if tray.uses_native_menu() {
            self.close_tray_menu(cx);
            tray.show_native_menu();
            return;
        }
        if let Some(handle) = self.tray_menu_window.take()
            && handle
                .update(cx, |_, window, _| window.remove_window())
                .is_ok()
        {
            return;
        }
        let desktop_pet_hidden = !self.desktop_pet_visible;
        let (options, menu_bounds) = tray_menu_window_options(anchor, cx);
        let tray_for_window = tray.clone();
        let result = cx.open_window(options, move |window, cx| {
            if configure_tray_menu_window(window).is_err() {
                log::warn!("event=tray_menu_window_config_failed stage=create");
            }
            let view =
                cx.new(|cx| TrayMenuView::new(tray_for_window, desktop_pet_hidden, window, cx));
            cx.new(|cx| {
                Root::new(view, window, cx)
                    .bordered(false)
                    .bg(transparent_black())
            })
        });
        match result {
            Ok(handle) => {
                let handle: AnyWindowHandle = handle.into();
                let native_window = handle.update(cx, |_, window, _| {
                    NativeTrayMenuWindow::prepare(window, menu_bounds, anchor.scale_factor)
                });
                let native_window = native_window
                    .map_err(|error| error.to_string())
                    .and_then(|result| result);
                let native_window = match native_window {
                    Ok(native_window) => native_window,
                    Err(_) => {
                        log::warn!("event=tray_menu_window_config_failed stage=prepare");
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                        tray.show_native_menu();
                        return;
                    }
                };
                self.tray_menu_window = Some(handle);
                let tray_for_fallback = tray.clone();
                // 原生 SetWindowPos 会同步派发 WM_MOVE、WM_SIZE 与 WM_DPICHANGED，必须等
                // 当前 App borrow 结束后执行，避免重入 GPUI。显示前再校验当前窗口 generation。
                cx.spawn(async move |this, cx| {
                    let current = this
                        .update(cx, |this, _| this.tray_menu_window == Some(handle))
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                    if native_window.show().is_err() {
                        log::warn!("event=tray_menu_window_config_failed stage=show");
                        let _ = this.update(cx, |this, cx| {
                            if this.tray_menu_window == Some(handle) {
                                this.close_tray_menu(cx);
                                tray_for_fallback.show_native_menu();
                            }
                        });
                    }
                })
                .detach();
            }
            Err(_) => {
                log::warn!("event=tray_menu_create_failed fallback=native");
                tray.show_native_menu();
            }
        }
    }

    pub(super) fn close_tray_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.tray_menu_window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    pub(super) fn send_hit_area_event_at(
        &mut self,
        frame: &RenderedModelFrame,
        generation: u64,
        position: gpui::Point<gpui::Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.chat_input_open || self.model_generation != generation {
            return false;
        }

        let viewport = window.viewport_size();
        let Some(hit_area) = frame.hit_area_at_window_point(
            [f32::from(position.x), f32::from(position.y)],
            [f32::from(viewport.width), f32::from(viewport.height)],
        ) else {
            return false;
        };
        let part_name = hit_area.name().to_owned();
        let language = self.appearance.borrow().language;
        self.chat.update(cx, |chat, cx| {
            chat.send_model_click_event(&part_name, language, cx)
        })
    }
}

pub(super) fn sync_system_tray_appearance(tray: Option<&SystemTray>, cx: &App) {
    let Some(tray) = tray else {
        return;
    };
    let palette = UiPalette::from_app(cx);
    let style = TrayIconStyle::new(rgb8_over(palette.primary, palette.background));
    if tray.sync_appearance(style).is_err() {
        log::warn!("event=tray_appearance_sync_failed");
    }
}

fn rgb8_over(color: Hsla, background: Hsla) -> [u8; 3] {
    let color = background.to_rgb().blend(color.to_rgb());
    [color.r, color.g, color.b].map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
}
