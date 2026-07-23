//! 定义应用级外观设置，并把语义色发布给 GPUI Component 全局主题。
//!
//! 主题预设使用 GPUI Component 的语义色槽位，而不是只修改应用根节点的背景色，
//! 因此输入框、选择器、弹出层和通知等组件会使用同一份颜色配置。

use std::rc::Rc;

use gpui::{App, Hsla, SharedString, Window};
use gpui_component::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode, try_parse_color};
use rust_i18n::t;

/// 应用支持的界面语言。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AppLanguage {
    /// 简体中文。
    #[default]
    SimplifiedChinese,
    /// 繁体中文。
    TraditionalChinese,
    /// 英语。
    English,
    /// 日语。
    Japanese,
}

impl AppLanguage {
    /// 返回配置文件中稳定的语言标识。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::SimplifiedChinese => "zh-CN",
            Self::TraditionalChinese => "zh-TW",
            Self::English => "en",
            Self::Japanese => "ja",
        }
    }

    /// 从配置文件标识恢复语言；未知值不影响其余配置。
    pub(crate) fn from_id(value: &str) -> Option<Self> {
        match value {
            "zh-CN" | "zh" => Some(Self::SimplifiedChinese),
            "zh-TW" | "zh-HK" => Some(Self::TraditionalChinese),
            "en" | "en-US" | "en-GB" => Some(Self::English),
            "ja" | "ja-JP" => Some(Self::Japanese),
            _ => None,
        }
    }
}

/// 同步应用文本和 GPUI Component 内置文本使用的全局语言。
pub(crate) fn apply_language(language: AppLanguage) {
    rust_i18n::set_locale(language.id());
    gpui_component::set_locale(language.id());
}

/// 内置和用户自定义的主题预设。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThemePreset {
    /// 根据当前窗口的系统外观选择亮色或暗色。
    #[default]
    System,
    /// 中性亮色主题。
    Light,
    /// 中性暗色主题。
    Dark,
    /// 石墨灰主题。
    Graphite,
    /// 粉色强调的亮色主题。
    Sakura,
    /// 蓝色强调的深色主题。
    Ocean,
    /// 面向可读性的高对比度主题。
    HighContrast,
    /// 使用用户输入的颜色。
    Custom,
}

impl ThemePreset {
    /// 返回配置文件中稳定的主题标识。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Graphite => "graphite",
            Self::Sakura => "sakura",
            Self::Ocean => "ocean",
            Self::HighContrast => "high-contrast",
            Self::Custom => "custom",
        }
    }

    /// 从配置文件标识恢复主题；未知值回退到跟随系统。
    pub(crate) fn from_id(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "graphite" => Some(Self::Graphite),
            "sakura" => Some(Self::Sakura),
            "ocean" => Some(Self::Ocean),
            "high-contrast" => Some(Self::HighContrast),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// 自定义主题的基础颜色设置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CustomThemeSettings {
    /// 自定义强调色，使用 `#RRGGBB` 或 `#RRGGBBAA`。
    pub(crate) accent: String,
    /// 自定义窗口和页面背景色。
    pub(crate) background: String,
    /// 自定义主题的明暗模式。
    pub(crate) mode: ThemeMode,
}

impl Default for CustomThemeSettings {
    fn default() -> Self {
        Self {
            accent: "#2DD4BF".to_owned(),
            background: "#0F172A".to_owned(),
            mode: ThemeMode::Dark,
        }
    }
}

/// 可持久化的外观配置快照。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppearanceSettings {
    pub(crate) language: AppLanguage,
    pub(crate) theme: ThemePreset,
    pub(crate) custom: CustomThemeSettings,
}

impl AppearanceSettings {
    /// 校验并规范化用户输入的自定义颜色。
    pub(crate) fn normalized(mut self) -> Result<Self, String> {
        self.custom.accent =
            normalize_hex(&self.custom.accent, t!("system.custom_accent").as_ref())?;
        self.custom.background = normalize_hex(
            &self.custom.background,
            t!("system.custom_background").as_ref(),
        )?;
        Ok(self)
    }
}

fn normalize_hex(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim().trim_start_matches('#');
    if (value.len() != 6 && value.len() != 8) || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(t!("theme.invalid_color", label = label).to_string());
    }
    let mut normalized = String::with_capacity(value.len() + 1);
    normalized.push('#');
    normalized.push_str(value);
    normalized.make_ascii_uppercase();
    Ok(normalized)
}

#[derive(Clone, Copy)]
struct ThemeColors {
    background: &'static str,
    foreground: &'static str,
    muted_foreground: &'static str,
    border: &'static str,
    input: &'static str,
    muted: &'static str,
    primary: &'static str,
    primary_foreground: &'static str,
    secondary: &'static str,
    secondary_foreground: &'static str,
    accent: &'static str,
    accent_foreground: &'static str,
    sidebar: &'static str,
    popover: &'static str,
    danger: &'static str,
    warning: &'static str,
    success: &'static str,
    info: &'static str,
}

const LIGHT_COLORS: ThemeColors = ThemeColors {
    background: "#F8FAFC",
    foreground: "#172033",
    muted_foreground: "#64748B",
    border: "#CBD5E1",
    input: "#94A3B8",
    muted: "#E2E8F0",
    primary: "#0F766E",
    primary_foreground: "#F0FDFA",
    secondary: "#E2E8F0",
    secondary_foreground: "#334155",
    accent: "#CCFBF1",
    accent_foreground: "#115E59",
    sidebar: "#F1F5F9",
    popover: "#FFFFFF",
    danger: "#DC2626",
    warning: "#B45309",
    success: "#15803D",
    info: "#0369A1",
};

const DARK_COLORS: ThemeColors = ThemeColors {
    background: "#0F172A",
    foreground: "#E2E8F0",
    muted_foreground: "#94A3B8",
    border: "#334155",
    input: "#475569",
    muted: "#1E293B",
    primary: "#14B8A6",
    primary_foreground: "#042F2E",
    secondary: "#1E293B",
    secondary_foreground: "#CBD5E1",
    accent: "#134E4A",
    accent_foreground: "#99F6E4",
    sidebar: "#111C31",
    popover: "#172033",
    danger: "#F87171",
    warning: "#FBBF24",
    success: "#4ADE80",
    info: "#38BDF8",
};

fn colors_for(preset: ThemePreset, custom: &CustomThemeSettings) -> (ThemeColors, ThemeMode) {
    match preset {
        ThemePreset::System | ThemePreset::Light => (LIGHT_COLORS, ThemeMode::Light),
        ThemePreset::Dark => (DARK_COLORS, ThemeMode::Dark),
        ThemePreset::Graphite => (
            ThemeColors {
                background: "#18181B",
                foreground: "#E4E4E7",
                muted_foreground: "#A1A1AA",
                border: "#3F3F46",
                input: "#52525B",
                muted: "#27272A",
                primary: "#A1A1AA",
                primary_foreground: "#18181B",
                secondary: "#27272A",
                secondary_foreground: "#D4D4D8",
                accent: "#3F3F46",
                accent_foreground: "#FAFAFA",
                sidebar: "#202023",
                popover: "#27272A",
                danger: "#F87171",
                warning: "#FBBF24",
                success: "#4ADE80",
                info: "#93C5FD",
            },
            ThemeMode::Dark,
        ),
        ThemePreset::Sakura => (
            ThemeColors {
                background: "#FFF7F9",
                foreground: "#3B1F2B",
                muted_foreground: "#8C6675",
                border: "#F1C9D7",
                input: "#D8A8BA",
                muted: "#FCE7F3",
                primary: "#BE185D",
                primary_foreground: "#FFF1F2",
                secondary: "#FCE7F3",
                secondary_foreground: "#831843",
                accent: "#FBCFE8",
                accent_foreground: "#9D174D",
                sidebar: "#FFF0F5",
                popover: "#FFFFFF",
                danger: "#BE123C",
                warning: "#A16207",
                success: "#15803D",
                info: "#0369A1",
            },
            ThemeMode::Light,
        ),
        ThemePreset::Ocean => (
            ThemeColors {
                background: "#082F49",
                foreground: "#E0F2FE",
                muted_foreground: "#7DD3FC",
                border: "#155E75",
                input: "#0E7490",
                muted: "#0C4A6E",
                primary: "#38BDF8",
                primary_foreground: "#082F49",
                secondary: "#164E63",
                secondary_foreground: "#BAE6FD",
                accent: "#164E63",
                accent_foreground: "#BAE6FD",
                sidebar: "#06283D",
                popover: "#0C4A6E",
                danger: "#FDA4AF",
                warning: "#FDE68A",
                success: "#86EFAC",
                info: "#7DD3FC",
            },
            ThemeMode::Dark,
        ),
        ThemePreset::HighContrast => (
            ThemeColors {
                background: "#000000",
                foreground: "#FFFFFF",
                muted_foreground: "#D4D4D8",
                border: "#FFFFFF",
                input: "#FFFFFF",
                muted: "#18181B",
                primary: "#FACC15",
                primary_foreground: "#000000",
                secondary: "#27272A",
                secondary_foreground: "#FFFFFF",
                accent: "#3F3F46",
                accent_foreground: "#FFFFFF",
                sidebar: "#09090B",
                popover: "#09090B",
                danger: "#FF5F56",
                warning: "#FACC15",
                success: "#00D084",
                info: "#5CC8FF",
            },
            ThemeMode::Dark,
        ),
        ThemePreset::Custom => {
            let base = if custom.mode.is_dark() {
                DARK_COLORS
            } else {
                LIGHT_COLORS
            };
            // 自定义值在构造 ThemeConfig 时单独写入，静态色槽仍使用可读的基准方案。
            (base, custom.mode)
        }
    }
}

fn theme_config(preset: ThemePreset, custom: &CustomThemeSettings) -> ThemeConfig {
    let (base, mode) = colors_for(preset, custom);
    let mut colors = ThemeConfigColors::default();
    colors.background = Some(base.background.into());
    colors.foreground = Some(base.foreground.into());
    colors.muted_foreground = Some(base.muted_foreground.into());
    colors.border = Some(base.border.into());
    colors.input = Some(base.input.into());
    colors.muted = Some(base.muted.into());
    colors.primary = Some(base.primary.into());
    colors.primary_foreground = Some(base.primary_foreground.into());
    colors.secondary = Some(base.secondary.into());
    colors.secondary_foreground = Some(base.secondary_foreground.into());
    colors.accent = Some(base.accent.into());
    colors.accent_foreground = Some(base.accent_foreground.into());
    colors.sidebar = Some(base.sidebar.into());
    colors.popover = Some(base.popover.into());
    colors.danger = Some(base.danger.into());
    colors.warning = Some(base.warning.into());
    colors.success = Some(base.success.into());
    colors.info = Some(base.info.into());
    colors.button = Some(base.secondary.into());
    colors.button_foreground = Some(base.secondary_foreground.into());
    colors.list = Some(base.background.into());
    colors.list_hover = Some(base.accent.into());
    colors.list_active = Some(base.accent.into());
    colors.title_bar = Some(base.sidebar.into());
    colors.title_bar_border = Some(base.border.into());
    colors.status_bar = Some(base.sidebar.into());
    colors.status_bar_border = Some(base.border.into());
    colors.ring = Some(base.primary.into());
    colors.selection = Some(base.primary.into());

    if preset == ThemePreset::Custom {
        colors.background = Some(custom.background.clone().into());
        colors.primary = Some(custom.accent.clone().into());
        colors.accent_foreground = Some(base.foreground.into());
        colors.ring = Some(custom.accent.clone().into());
        colors.selection = Some(custom.accent.clone().into());
        if let (Ok(accent), Ok(background)) = (
            try_parse_color(&custom.accent),
            try_parse_color(&custom.background),
        ) {
            let accent_surface = blend_color(background, accent, 0.22);
            colors.accent = Some(accent_surface.clone().into());
            colors.list_hover = Some(accent_surface.clone().into());
            colors.list_active = Some(accent_surface.clone().into());
            colors.sidebar_accent = Some(accent_surface.into());
            colors.primary_foreground = Some(readable_foreground(accent).into());
        }
    }

    ThemeConfig {
        name: SharedString::from(preset.id()),
        mode,
        colors,
        ..ThemeConfig::default()
    }
}

fn readable_foreground(accent: Hsla) -> &'static str {
    let rgb = accent.to_rgb();
    let luminance = [rgb.r, rgb.g, rgb.b].map(|component| {
        if component <= 0.03928 {
            component / 12.92
        } else {
            ((component + 0.055) / 1.055).powf(2.4)
        }
    });
    let relative_luminance = 0.2126 * luminance[0] + 0.7152 * luminance[1] + 0.0722 * luminance[2];
    let black_contrast = (relative_luminance + 0.05) / 0.05;
    let white_contrast = 1.05 / (relative_luminance + 0.05);

    if black_contrast >= white_contrast {
        "#111827"
    } else {
        "#F8FAFC"
    }
}

fn blend_color(background: Hsla, foreground: Hsla, amount: f32) -> String {
    let background = background.to_rgb();
    let foreground = foreground.to_rgb();
    let amount = amount.clamp(0.0, 1.0);
    let mix = |base: f32, accent: f32| {
        ((base + (accent - base) * amount).clamp(0.0, 1.0) * 255.0).round() as u8
    };
    format!(
        "#{:02X}{:02X}{:02X}",
        mix(background.r, foreground.r),
        mix(background.g, foreground.g),
        mix(background.b, foreground.b),
    )
}

/// 将外观配置应用到 GPUI Component 全局主题。
pub(crate) fn apply(settings: &AppearanceSettings, window: Option<&mut Window>, cx: &mut App) {
    if settings.theme == ThemePreset::System {
        let refresh_all = window.is_none();
        Theme::sync_system_appearance(window, cx);
        if refresh_all {
            cx.refresh_windows();
        }
        return;
    }

    let config = theme_config(settings.theme, &settings.custom);
    Theme::global_mut(cx).apply_config(&Rc::new(config));
    if let Some(window) = window {
        window.refresh();
    } else {
        cx.refresh_windows();
    }
}

/// 提取当前主题中的语义色，供桌宠自绘层使用。
#[derive(Clone, Copy)]
pub(crate) struct UiPalette {
    pub(crate) background: Hsla,
    pub(crate) foreground: Hsla,
    pub(crate) muted_foreground: Hsla,
    pub(crate) border: Hsla,
    pub(crate) input: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) primary: Hsla,
    pub(crate) primary_foreground: Hsla,
    pub(crate) secondary: Hsla,
    pub(crate) secondary_foreground: Hsla,
    pub(crate) accent: Hsla,
    pub(crate) accent_foreground: Hsla,
    pub(crate) sidebar: Hsla,
    pub(crate) popover: Hsla,
    pub(crate) danger: Hsla,
    pub(crate) danger_foreground: Hsla,
    pub(crate) warning: Hsla,
    pub(crate) warning_foreground: Hsla,
    pub(crate) info: Hsla,
}

impl UiPalette {
    /// 从 GPUI 全局主题读取当前帧需要的颜色。
    pub(crate) fn from_app(cx: &App) -> Self {
        let theme = Theme::global(cx);
        Self {
            background: theme.background,
            foreground: theme.foreground,
            muted_foreground: theme.muted_foreground,
            border: theme.border,
            input: theme.input,
            muted: theme.muted,
            primary: theme.primary,
            primary_foreground: theme.primary_foreground,
            secondary: theme.secondary,
            secondary_foreground: theme.secondary_foreground,
            accent: theme.accent,
            accent_foreground: theme.accent_foreground,
            sidebar: theme.sidebar,
            popover: theme.popover,
            danger: theme.danger,
            danger_foreground: theme.danger_foreground,
            warning: theme.warning,
            warning_foreground: theme.warning_foreground,
            info: theme.info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_colors_are_normalized_before_persistence() {
        let settings = AppearanceSettings {
            custom: CustomThemeSettings {
                accent: " 2563eb ".to_owned(),
                background: "#f8fafc".to_owned(),
                mode: ThemeMode::Light,
            },
            ..AppearanceSettings::default()
        }
        .normalized()
        .expect("有效十六进制颜色应当可以规范化");

        assert_eq!(settings.custom.accent, "#2563EB");
        assert_eq!(settings.custom.background, "#F8FAFC");
    }

    #[test]
    fn custom_theme_chooses_readable_primary_foreground() {
        let light_accent = CustomThemeSettings {
            accent: "#FACC15".to_owned(),
            ..CustomThemeSettings::default()
        };
        let dark_accent = CustomThemeSettings {
            accent: "#0F172A".to_owned(),
            ..CustomThemeSettings::default()
        };

        let light = theme_config(ThemePreset::Custom, &light_accent);
        let dark = theme_config(ThemePreset::Custom, &dark_accent);
        assert_eq!(light.colors.primary_foreground.as_deref(), Some("#111827"));
        assert_ne!(light.colors.accent.as_deref(), Some("#FACC15"));
        assert_eq!(dark.colors.primary_foreground.as_deref(), Some("#F8FAFC"));
    }

    #[test]
    fn core_settings_text_exists_in_every_supported_language() {
        assert_eq!(
            rust_i18n::t!("settings.system_title", locale = "zh-CN"),
            "系统设置"
        );
        assert_eq!(
            rust_i18n::t!("settings.system_title", locale = "zh-TW"),
            "系統設定"
        );
        assert_eq!(
            rust_i18n::t!("settings.system_title", locale = "en"),
            "System Settings"
        );
        assert_eq!(
            rust_i18n::t!("settings.system_title", locale = "ja"),
            "システム設定"
        );

        for locale in ["zh-CN", "zh-TW", "en", "ja"] {
            assert_ne!(
                rust_i18n::t!("settings.debug_title", locale = locale),
                "settings.debug_title"
            );
            assert_ne!(
                rust_i18n::t!("system.eye_tracking", locale = locale),
                "system.eye_tracking"
            );
            assert_ne!(
                rust_i18n::t!("llm.add_model", locale = locale),
                "llm.add_model"
            );
        }
    }
}
