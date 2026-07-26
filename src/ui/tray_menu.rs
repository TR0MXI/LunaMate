//! 渲染主题化托盘菜单，并计算菜单在托盘图标附近的屏幕位置。

use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, IntoElement, MouseButton, Pixels, Point, Render, Size,
    Subscription, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind,
    WindowOptions, div, point, prelude::*, px, size, svg, transparent_black,
};
use rust_i18n::t;

use crate::{
    config::{CONFIG, ThemePreset},
    platform::{SystemTray, SystemTrayAction, TrayMenuAnchor},
};

use super::{UiPalette, apply};

const MENU_WIDTH: f32 = 192.0;
const MENU_HEIGHT: f32 = 112.0;
const MENU_MARGIN: f32 = 8.0;
const MENU_ANCHOR_GAP: f32 = 8.0;
const MENU_SURFACE_OPACITY: f32 = 0.64;
const MENU_HOVER_OPACITY: f32 = 0.14;
const MENU_DANGER_HOVER_OPACITY: f32 = 0.12;
const MENU_DIVIDER_OPACITY: f32 = 0.24;
const _: () = assert!(MENU_SURFACE_OPACITY + MENU_HOVER_OPACITY <= 0.8);

/// 承载托盘动作的短生命周期弹出视图。
pub(in crate::ui) struct TrayMenuView {
    tray: Rc<SystemTray>,
    desktop_pet_hidden: bool,
    was_active: bool,
    _activation_subscription: Subscription,
    _appearance_subscription: Subscription,
}

impl TrayMenuView {
    pub(in crate::ui) fn new(
        tray: Rc<SystemTray>,
        desktop_pet_hidden: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let was_active = window.is_window_active();
        let activation_subscription = cx.observe_window_activation(window, |this, window, _| {
            if window.is_window_active() {
                this.was_active = true;
            } else if this.was_active {
                window.remove_window();
            }
        });
        let appearance = CONFIG.appearance();
        if appearance.theme == ThemePreset::System {
            apply(&appearance, Some(window), cx);
        }
        let appearance_subscription = window.observe_window_appearance(|window, cx| {
            let appearance = CONFIG.appearance();
            if appearance.theme == ThemePreset::System {
                apply(&appearance, Some(window), cx);
            }
        });
        Self {
            tray,
            desktop_pet_hidden,
            was_active,
            _activation_subscription: activation_subscription,
            _appearance_subscription: appearance_subscription,
        }
    }
}

impl Render for TrayMenuView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        div().size_full().bg(transparent_black()).p(px(2.0)).child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .justify_center()
                .overflow_hidden()
                .rounded_md()
                .bg(palette.popover.opacity(MENU_SURFACE_OPACITY))
                .p(px(2.0))
                .occlude()
                .child(tray_menu_item(
                    "tray-toggle-desktop-pet",
                    t!("tray.hide_desktop_pet").to_string(),
                    "icons/eye-off.svg",
                    self.desktop_pet_hidden,
                    SystemTrayAction::ToggleDesktopPet,
                    self.tray.clone(),
                    palette,
                ))
                .child(tray_menu_item(
                    "tray-open-settings",
                    t!("tray.settings").to_string(),
                    "icons/settings.svg",
                    false,
                    SystemTrayAction::OpenSettings,
                    self.tray.clone(),
                    palette,
                ))
                .child(
                    div()
                        .mx_2()
                        .my(px(2.0))
                        .h(px(1.0))
                        .bg(palette.border.opacity(MENU_DIVIDER_OPACITY)),
                )
                .child(tray_menu_item(
                    "tray-quit",
                    t!("tray.quit").to_string(),
                    "icons/x.svg",
                    false,
                    SystemTrayAction::Quit,
                    self.tray.clone(),
                    palette,
                )),
        )
    }
}

fn tray_menu_item(
    id: &'static str,
    label: String,
    icon: &'static str,
    checked: bool,
    action: SystemTrayAction,
    tray: Rc<SystemTray>,
    palette: UiPalette,
) -> AnyElement {
    let danger = matches!(action, SystemTrayAction::Quit);
    let foreground = if danger {
        palette.danger
    } else {
        palette.foreground
    };
    let icon = if checked { "icons/check.svg" } else { icon };
    div()
        .id(id)
        .h(px(32.0))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .rounded_sm()
        .px_2()
        .text_xs()
        .text_color(foreground)
        .cursor_pointer()
        .hover(move |style| {
            if danger {
                style.bg(palette.danger.opacity(MENU_DANGER_HOVER_OPACITY))
            } else {
                style.bg(palette.accent.opacity(MENU_HOVER_OPACITY))
            }
        })
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            tray.request_action(action);
            window.remove_window();
        })
        .child(
            svg()
                .path(icon)
                .size(px(14.0))
                .flex_none()
                .text_color(foreground),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .child(label),
        )
        .into_any_element()
}

pub(in crate::ui) fn tray_menu_window_options(
    anchor: TrayMenuAnchor,
    cx: &App,
) -> (WindowOptions, Bounds<Pixels>) {
    let icon_bounds = Bounds {
        origin: point(px(anchor.icon_origin[0]), px(anchor.icon_origin[1])),
        size: size(px(anchor.icon_size[0]), px(anchor.icon_size[1])),
    };
    let icon_center = icon_bounds.center();
    let display = cx
        .displays()
        .into_iter()
        .find(|display| contains_point(display.bounds(), icon_center))
        .or_else(|| cx.primary_display());
    let (bounds, display_id) = if let Some(display) = display {
        (
            tray_menu_bounds_for_display(icon_bounds, display.bounds(), display.visible_bounds()),
            Some(display.id()),
        )
    } else {
        (
            Bounds {
                origin: point(
                    icon_center.x - px(MENU_WIDTH / 2.0),
                    icon_bounds.origin.y + icon_bounds.size.height + px(MENU_ANCHOR_GAP),
                ),
                size: menu_size(),
            },
            None,
        )
    };

    (
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: true,
            // Windows 先在隐藏状态完成目标 DPI 与物理坐标定位，再由平台层同步显示。
            show: !cfg!(target_os = "windows"),
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id,
            // Windows Acrylic 在关闭系统透明效果或被第三方壳层接管时会退化为黑底；逐像素
            // 透明交换链仍能稳定保留菜单表面自身的半透明。macOS 继续使用原生模糊材质。
            window_background: if cfg!(target_os = "windows") {
                WindowBackgroundAppearance::Transparent
            } else {
                WindowBackgroundAppearance::Blurred
            },
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("lunamate-tray-menu".to_owned()),
            ..Default::default()
        },
        bounds,
    )
}

pub(in crate::ui) fn tray_menu_bounds_for_display(
    icon: Bounds<Pixels>,
    display: Bounds<Pixels>,
    visible: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let menu = menu_size();
    let center = icon.center();
    let display_left = f32::from(display.origin.x);
    let display_top = f32::from(display.origin.y);
    let display_right = display_left + f32::from(display.size.width);
    let display_bottom = display_top + f32::from(display.size.height);
    let center_x = f32::from(center.x);
    let center_y = f32::from(center.y);
    let edge = [
        (TrayEdge::Top, (center_y - display_top).abs()),
        (TrayEdge::Bottom, (display_bottom - center_y).abs()),
        (TrayEdge::Left, (center_x - display_left).abs()),
        (TrayEdge::Right, (display_right - center_x).abs()),
    ]
    .into_iter()
    .min_by(|left, right| left.1.total_cmp(&right.1))
    .map(|(edge, _)| edge)
    .unwrap_or(TrayEdge::Bottom);

    let icon_left = f32::from(icon.origin.x);
    let icon_top = f32::from(icon.origin.y);
    let icon_right = icon_left + f32::from(icon.size.width);
    let icon_bottom = icon_top + f32::from(icon.size.height);
    let menu_width = f32::from(menu.width);
    let menu_height = f32::from(menu.height);
    let (x, y) = match edge {
        TrayEdge::Top => (center_x - menu_width / 2.0, icon_bottom + MENU_ANCHOR_GAP),
        TrayEdge::Bottom => (
            center_x - menu_width / 2.0,
            icon_top - menu_height - MENU_ANCHOR_GAP,
        ),
        TrayEdge::Left => (icon_right + MENU_ANCHOR_GAP, center_y - menu_height / 2.0),
        TrayEdge::Right => (
            icon_left - menu_width - MENU_ANCHOR_GAP,
            center_y - menu_height / 2.0,
        ),
    };

    let visible_left = f32::from(visible.origin.x) + MENU_MARGIN;
    let visible_top = f32::from(visible.origin.y) + MENU_MARGIN;
    let visible_right = f32::from(visible.origin.x) + f32::from(visible.size.width) - MENU_MARGIN;
    let visible_bottom = f32::from(visible.origin.y) + f32::from(visible.size.height) - MENU_MARGIN;
    Bounds {
        origin: point(
            px(clamp_origin(x, visible_left, visible_right - menu_width)),
            px(clamp_origin(y, visible_top, visible_bottom - menu_height)),
        ),
        size: menu,
    }
}

#[derive(Clone, Copy)]
enum TrayEdge {
    Top,
    Bottom,
    Left,
    Right,
}

fn menu_size() -> Size<Pixels> {
    size(px(MENU_WIDTH), px(MENU_HEIGHT))
}

fn contains_point(bounds: Bounds<Pixels>, point: Point<Pixels>) -> bool {
    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let right = left + f32::from(bounds.size.width);
    let bottom = top + f32::from(bounds.size.height);
    let x = f32::from(point.x);
    let y = f32::from(point.y);
    (left..=right).contains(&x) && (top..=bottom).contains(&y)
}

fn clamp_origin(value: f32, minimum: f32, maximum: f32) -> f32 {
    if maximum < minimum {
        minimum
    } else {
        value.clamp(minimum, maximum)
    }
}
