//! 计算 UI 窗口与渲染尺寸，并负责恢复和缓存窗口位置。

use gpui::{App, Bounds, Pixels, Size, Window, WindowBounds, point, px, size};

#[cfg(target_os = "linux")]
use std::sync::OnceLock;

use crate::{
    config::{CONFIG, ConfigWindow, ModelWindowSize, WindowPosition},
    model::GpuUnderlaySize,
};

pub(in crate::ui) const PHONE_ASPECT_RATIO: f32 = 16.0 / 9.0;
const WINDOW_WIDTH_FRACTION: f32 = 0.18;
pub(in crate::ui) const WINDOW_HEIGHT_FRACTION: f32 = 0.52;
pub(in crate::ui) const DISPLAY_MARGIN_FRACTION: f32 = 0.90;
pub(in crate::ui) const MIN_WINDOW_WIDTH: f32 = 220.0;
pub(in crate::ui) const MAX_WINDOW_WIDTH: f32 = 420.0;
const RENDER_SUPERSAMPLE: f32 = 1.25;
pub(in crate::ui) const MAX_RASTER_DIMENSION: u32 = 1_280;
const SETTINGS_WINDOW_WIDTH: f32 = 980.0;
const SETTINGS_WINDOW_HEIGHT: f32 = 620.0;
const SETTINGS_WINDOW_MIN_WIDTH: f32 = 900.0;
const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 520.0;
const MIN_VISIBLE_WINDOW_EDGE: f32 = 48.0;

/// 返回当前窗口对应的 GPU 物理尺寸与合成器逻辑尺寸。
pub(crate) fn gpu_underlay_size_for_window(window: &Window) -> GpuUnderlaySize {
    let viewport = window.viewport_size();
    gpu_underlay_size(
        f32::from(viewport.width),
        f32::from(viewport.height),
        window.scale_factor(),
    )
}

/// 将逻辑窗口尺寸换算为 GPU underlay 使用的物理与逻辑像素尺寸。
///
/// 物理尺寸由取整后的逻辑尺寸派生，使二者在整数缩放下保持整除关系；否则分数窗口高度会让
/// Wayland 的 `exact_buffer_scale` 失败并退回 CPU renderer。
pub(crate) fn gpu_underlay_size(width: f32, height: f32, scale_factor: f32) -> GpuUnderlaySize {
    let logical = [
        width.max(1.0).round().max(1.0) as u32,
        height.max(1.0).round().max(1.0) as u32,
    ];
    let scale_factor = scale_factor.max(1.0);
    let physical = [
        (f64::from(logical[0]) * f64::from(scale_factor)).round() as u32,
        (f64::from(logical[1]) * f64::from(scale_factor)).round() as u32,
    ];
    GpuUnderlaySize { physical, logical }
}

/// 根据显示器可用区域和用户预设计算桌宠窗口尺寸。
pub(crate) fn desktop_pet_window_size(
    display_width: f32,
    display_height: f32,
    configured_size: ModelWindowSize,
) -> [f32; 2] {
    let preferred_width = configured_size.width().unwrap_or_else(|| {
        (display_width * WINDOW_WIDTH_FRACTION)
            .min(display_height * WINDOW_HEIGHT_FRACTION / PHONE_ASPECT_RATIO)
    });
    let maximum_fitting_width = desktop_pet_maximum_fitting_width(display_width, display_height);
    let width = preferred_width
        .clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_WIDTH)
        .min(maximum_fitting_width);

    [width, width * PHONE_ASPECT_RATIO]
}

fn desktop_pet_maximum_fitting_width(display_width: f32, display_height: f32) -> f32 {
    (display_width * DISPLAY_MARGIN_FRACTION)
        .min(display_height * DISPLAY_MARGIN_FRACTION / PHONE_ASPECT_RATIO)
        .max(1.0)
}

/// 根据显示器可用区域返回桌宠窗口允许的最小逻辑尺寸。
pub(crate) fn desktop_pet_window_min_size(display_width: f32, display_height: f32) -> Size<Pixels> {
    let width = MIN_WINDOW_WIDTH.min(desktop_pet_maximum_fitting_width(
        display_width,
        display_height,
    ));
    size(px(width), px(width * PHONE_ASPECT_RATIO))
}

/// 按主显示器可用区域约束设置窗口的初始尺寸与最小尺寸。
pub(crate) fn settings_window_sizes(cx: &App) -> (Size<Pixels>, Size<Pixels>) {
    let display_size = cx
        .primary_display()
        .map(|display| display.visible_bounds().size)
        .unwrap_or_else(|| size(px(1280.0), px(720.0)));
    let available_width = f32::from(display_size.width) * 0.92;
    let available_height = f32::from(display_size.height) * 0.90;
    let width = SETTINGS_WINDOW_WIDTH.min(available_width.max(1.0));
    let height = SETTINGS_WINDOW_HEIGHT.min(available_height.max(1.0));
    let min_width = SETTINGS_WINDOW_MIN_WIDTH.min(width);
    let min_height = SETTINGS_WINDOW_MIN_HEIGHT.min(height);
    (
        size(px(width), px(height)),
        size(px(min_width), px(min_height)),
    )
}

/// 恢复仍与任一显示器相交的窗口位置，否则由 GPUI 居中窗口。
pub(crate) fn restored_window_bounds(
    window: ConfigWindow,
    window_size: Size<Pixels>,
    cx: &App,
) -> WindowBounds {
    if supports_absolute_window_positions()
        && CONFIG.remember_window_positions()
        && let Some(position) = CONFIG.window_position(window)
    {
        let candidate = Bounds {
            origin: point(px(position.x), px(position.y)),
            size: window_size,
        };
        if cx
            .displays()
            .iter()
            .any(|display| window_intersects_display(candidate, display.visible_bounds()))
        {
            return WindowBounds::Windowed(candidate);
        }
    }
    WindowBounds::centered(window_size, cx)
}

fn window_intersects_display(window: Bounds<Pixels>, display: Bounds<Pixels>) -> bool {
    let window_left = f32::from(window.origin.x);
    let window_top = f32::from(window.origin.y);
    let window_right = window_left + f32::from(window.size.width);
    let window_bottom = window_top + f32::from(window.size.height);
    let display_left = f32::from(display.origin.x);
    let display_top = f32::from(display.origin.y);
    let display_right = display_left + f32::from(display.size.width);
    let display_bottom = display_top + f32::from(display.size.height);

    window_right >= display_left + MIN_VISIBLE_WINDOW_EDGE
        && window_left <= display_right - MIN_VISIBLE_WINDOW_EDGE
        && window_bottom >= display_top + MIN_VISIBLE_WINDOW_EDGE
        && window_top <= display_bottom - MIN_VISIBLE_WINDOW_EDGE
}

/// 将窗口当前逻辑坐标写入配置的内存缓存。
pub(crate) fn cache_window_position(window: &Window, role: ConfigWindow) {
    if !supports_absolute_window_positions() {
        return;
    }
    let bounds = window.window_bounds().get_bounds();
    if let Some(position) =
        WindowPosition::new(f32::from(bounds.origin.x), f32::from(bounds.origin.y))
    {
        CONFIG.cache_window_position(role, position);
    }
}

fn supports_absolute_window_positions() -> bool {
    #[cfg(target_os = "linux")]
    {
        static SUPPORTED: OnceLock<bool> = OnceLock::new();
        *SUPPORTED.get_or_init(|| gpui::guess_compositor() != "Wayland")
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// 将逻辑窗口尺寸换算为受长边上限约束的 CPU 光栅尺寸。
pub(crate) fn raster_dimensions_for_window(width: f32, height: f32, scale_factor: f32) -> [u32; 2] {
    let raster_scale = scale_factor.max(1.0) * RENDER_SUPERSAMPLE;
    let raw_width = width.max(1.0) * raster_scale;
    let raw_height = height.max(1.0) * raster_scale;
    let limit_scale = (MAX_RASTER_DIMENSION as f32 / raw_width.max(raw_height)).min(1.0);

    [
        (raw_width * limit_scale)
            .ceil()
            .clamp(1.0, MAX_RASTER_DIMENSION as f32) as u32,
        (raw_height * limit_scale)
            .ceil()
            .clamp(1.0, MAX_RASTER_DIMENSION as f32) as u32,
    ]
}
