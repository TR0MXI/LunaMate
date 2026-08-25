//! 定义可持久化的界面语言、主题预设与自定义外观配置。

use gpui_component::ThemeMode;
use lunamate_agent::config::AppLanguage;
use rust_i18n::t;

/// 内置和用户自定义的主题预设。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemePreset {
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
    pub const fn id(self) -> &'static str {
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
    pub fn from_id(value: &str) -> Option<Self> {
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
pub struct CustomThemeSettings {
    /// 自定义强调色，使用 `#RRGGBB` 或 `#RRGGBBAA`。
    pub accent: String,
    /// 自定义窗口和页面背景色。
    pub background: String,
    /// 自定义主题的明暗模式。
    pub mode: ThemeMode,
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
pub struct AppearanceSettings {
    pub language: AppLanguage,
    pub theme: ThemePreset,
    pub custom: CustomThemeSettings,
}

impl AppearanceSettings {
    /// 校验并规范化用户输入的自定义颜色。
    pub fn normalized(mut self) -> Result<Self, String> {
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
