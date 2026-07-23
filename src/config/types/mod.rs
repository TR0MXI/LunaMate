//! 定义配置模块对内外共享的领域值与持久化错误。

use std::{error::Error, fmt, io, path::PathBuf};

use rust_i18n::t;

pub(super) const UNLIMITED_FRAME_RATE_NAME: &str = "unlimited";
const UNLIMITED_FRAME_RATE_VALUE: u16 = 0;
const MODEL_WINDOW_SIZE_AUTO: u16 = 0;
const MODEL_WINDOW_SIZE_COMPACT: u16 = 240;
const MODEL_WINDOW_SIZE_STANDARD: u16 = 300;
const MODEL_WINDOW_SIZE_LARGE: u16 = 360;
const MODEL_WINDOW_SIZE_EXTRA_LARGE: u16 = 420;

/// 区分需要单独恢复位置的应用窗口。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigWindow {
    /// 透明桌宠主窗口。
    DesktopPet,
    /// 独立设置窗口。
    Settings,
}

impl ConfigWindow {
    pub(super) fn table_name(self) -> &'static str {
        match self {
            Self::DesktopPet => "desktop_pet",
            Self::Settings => "settings",
        }
    }
}

/// 可跨线程保存的逻辑窗口坐标。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowPosition {
    /// 屏幕逻辑坐标横轴。
    pub(crate) x: f32,
    /// 屏幕逻辑坐标纵轴。
    pub(crate) y: f32,
}

impl WindowPosition {
    /// 只接受有限坐标，避免损坏配置传入窗口后端。
    pub(crate) fn new(x: f32, y: f32) -> Option<Self> {
        (x.is_finite() && y.is_finite()).then_some(Self { x, y })
    }
}

/// 表示桌宠主窗口的预设尺寸；自动档位根据显示器大小计算。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ModelWindowSize {
    /// 按当前显示器自动计算尺寸。
    #[default]
    Auto,
    /// 紧凑尺寸，宽度约为 240 逻辑像素。
    Compact,
    /// 标准尺寸，宽度约为 300 逻辑像素。
    Standard,
    /// 大尺寸，宽度约为 360 逻辑像素。
    Large,
    /// 特大尺寸，宽度约为 420 逻辑像素。
    ExtraLarge,
}

impl ModelWindowSize {
    /// 返回配置文件中的稳定标识。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Compact => "compact",
            Self::Standard => "standard",
            Self::Large => "large",
            Self::ExtraLarge => "extra-large",
        }
    }

    /// 返回固定档位的目标宽度；自动档位返回 `None`。
    pub(crate) const fn width(self) -> Option<f32> {
        match self {
            Self::Auto => None,
            Self::Compact => Some(240.0),
            Self::Standard => Some(300.0),
            Self::Large => Some(360.0),
            Self::ExtraLarge => Some(420.0),
        }
    }

    pub(super) fn atomic_value(self) -> u16 {
        match self {
            Self::Auto => MODEL_WINDOW_SIZE_AUTO,
            Self::Compact => MODEL_WINDOW_SIZE_COMPACT,
            Self::Standard => MODEL_WINDOW_SIZE_STANDARD,
            Self::Large => MODEL_WINDOW_SIZE_LARGE,
            Self::ExtraLarge => MODEL_WINDOW_SIZE_EXTRA_LARGE,
        }
    }

    pub(super) fn from_atomic_value(value: u16) -> Self {
        match value {
            MODEL_WINDOW_SIZE_COMPACT => Self::Compact,
            MODEL_WINDOW_SIZE_STANDARD => Self::Standard,
            MODEL_WINDOW_SIZE_LARGE => Self::Large,
            MODEL_WINDOW_SIZE_EXTRA_LARGE => Self::ExtraLarge,
            _ => Self::Auto,
        }
    }

    pub(super) fn from_id(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "compact" => Some(Self::Compact),
            "standard" => Some(Self::Standard),
            "large" => Some(Self::Large),
            "extra-large" => Some(Self::ExtraLarge),
            _ => None,
        }
    }
}

/// 表示三个内置档位或无帧率上限模式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FrameRate {
    /// 低功耗内置档位。
    #[default]
    Fps30,
    /// 平衡流畅度与负载的内置档位。
    Fps60,
    /// 面向高刷新率设备的内置档位。
    Fps120,
    /// 不增加人工帧间隔，由实际渲染耗时决定帧率。
    Unlimited,
}

impl FrameRate {
    /// 返回有限模式的每秒目标渲染帧数；无限制模式返回 `None`。
    pub(crate) fn limit(self) -> Option<u16> {
        match self {
            Self::Fps30 => Some(30),
            Self::Fps60 => Some(60),
            Self::Fps120 => Some(120),
            Self::Unlimited => None,
        }
    }

    /// 返回适合界面状态提示的简短名称。
    pub(crate) fn display_name(self) -> String {
        self.limit()
            .map(|fps| format!("{fps} FPS"))
            .unwrap_or_else(|| t!("system.unlimited").to_string())
    }

    pub(super) fn atomic_value(self) -> u16 {
        self.limit().unwrap_or(UNLIMITED_FRAME_RATE_VALUE)
    }

    pub(super) fn from_atomic_value(value: u16) -> Self {
        if value == UNLIMITED_FRAME_RATE_VALUE {
            Self::Unlimited
        } else {
            Self::try_from(value).unwrap_or_default()
        }
    }
}

impl TryFrom<u16> for FrameRate {
    type Error = FrameRateError;

    fn try_from(fps: u16) -> Result<Self, Self::Error> {
        match fps {
            30 => Ok(Self::Fps30),
            60 => Ok(Self::Fps60),
            120 => Ok(Self::Fps120),
            _ => Err(FrameRateError { fps }),
        }
    }
}

/// 描述无法用于实时渲染调度的帧率值。
#[derive(Debug)]
pub(crate) struct FrameRateError {
    fps: u16,
}

impl fmt::Display for FrameRateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "帧率只能是 30、60、120 FPS 或无限制，当前为 {} FPS",
            self.fps
        )
    }
}

impl Error for FrameRateError {}

/// 描述配置修改无法持久化的原因。
#[derive(Debug)]
pub(crate) enum ConfigWriteError {
    /// 外部值不满足配置约束。
    InvalidValue(String),
    /// 写入开始前已经被同一配置项的新请求替代。
    #[cfg(test)]
    StaleConfigUpdate,
    /// 配置文件系统操作失败。
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ConfigWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue(message) => formatter.write_str(message),
            #[cfg(test)]
            Self::StaleConfigUpdate => formatter.write_str("配置写入已被更新请求替代"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {} 失败：{source}", path.display()),
        }
    }
}

impl Error for ConfigWriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidValue(_) => None,
            #[cfg(test)]
            Self::StaleConfigUpdate => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}
