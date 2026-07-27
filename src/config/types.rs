//! 定义配置模块对内外共享的领域值与持久化错误。

use std::{error::Error, fmt, io, num::NonZeroU16, path::PathBuf};

use rust_i18n::t;

pub(super) const UNLIMITED_FRAME_RATE_NAME: &str = "unlimited";
pub(super) const FOLLOW_DISPLAY_FRAME_RATE_NAME: &str = "display";
pub(super) const CUSTOM_FRAME_RATE_NAME: &str = "custom";
pub(super) const CUSTOM_FRAME_RATE_KEY: &str = "custom_frame_rate";
pub(crate) const CUSTOM_FRAME_RATE_MIN: u16 = 1;
pub(crate) const CUSTOM_FRAME_RATE_MAX: u16 = u16::MAX;
const UNLIMITED_FRAME_RATE_VALUE: u32 = 0;
const CUSTOM_FRAME_RATE_TAG: u32 = 1 << 16;
const FOLLOW_DISPLAY_FRAME_RATE_VALUE: u32 = 2 << 16;
const FRAME_RATE_PAYLOAD_MASK: u32 = 0xFFFF;
const MODEL_WINDOW_SIZE_AUTO: u16 = 0;
const MODEL_WINDOW_SIZE_COMPACT: u16 = 240;
const MODEL_WINDOW_SIZE_STANDARD: u16 = 300;
const MODEL_WINDOW_SIZE_LARGE: u16 = 360;
const MODEL_WINDOW_SIZE_EXTRA_LARGE: u16 = 420;
const LOGGING_DEFAULT_MAX_SIZE_MB: u32 = 10;
const LOGGING_DEFAULT_KEEP_FILES: u32 = 10;
pub(crate) const LOGGING_MIN_FILE_SIZE_MB: u32 = 1;
pub(crate) const LOGGING_MAX_FILE_SIZE_MB: u32 = 1_024;
pub(crate) const LOGGING_MIN_KEEP_FILES: u32 = 1;
pub(crate) const LOGGING_MAX_KEEP_FILES: u32 = 100;

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

/// 表示内置、自定义、显示器同步或无帧率上限模式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FrameRate {
    /// 低功耗内置档位。
    #[default]
    Fps30,
    /// 平衡流畅度与负载的内置档位。
    Fps60,
    /// 面向高刷新率设备的内置档位。
    Fps120,
    /// 由显示系统的帧回调或 FIFO presentation 驱动。
    FollowDisplay,
    /// 用户指定的正整数目标帧率；只受 `u16` 技术表示范围约束。
    Custom(NonZeroU16),
    /// 不增加人工帧间隔，由实际渲染耗时决定帧率。
    Unlimited,
}

impl FrameRate {
    /// 创建保留自定义档位身份的正整数帧率。
    pub(crate) fn custom(fps: u16) -> Result<Self, FrameRateError> {
        NonZeroU16::new(fps)
            .map(Self::Custom)
            .ok_or(FrameRateError { fps })
    }

    /// 返回软件限帧模式的每秒目标渲染帧数。
    pub(crate) fn limit(self) -> Option<u16> {
        match self {
            Self::Fps30 => Some(30),
            Self::Fps60 => Some(60),
            Self::Fps120 => Some(120),
            Self::Custom(fps) => Some(fps.get()),
            Self::FollowDisplay | Self::Unlimited => None,
        }
    }

    /// 返回是否由显示系统而不是软件定时器决定下一帧时刻。
    pub(crate) fn follows_display(self) -> bool {
        matches!(self, Self::FollowDisplay)
    }

    /// 返回是否允许在持续超预算时自动降低到半帧或四分之一帧。
    pub(crate) fn allows_frame_rate_degradation(self) -> bool {
        matches!(self, Self::Fps30 | Self::Fps60 | Self::Fps120)
    }

    /// 返回 GPU presentation 是否必须使用无撕裂 FIFO 模式。
    pub(crate) fn uses_vsync(self) -> bool {
        matches!(
            self,
            Self::Fps30 | Self::Fps60 | Self::Fps120 | Self::FollowDisplay
        )
    }

    /// 返回适合界面状态提示的简短名称。
    pub(crate) fn display_name(self) -> String {
        match self {
            Self::Fps30 => "30 FPS".to_owned(),
            Self::Fps60 => "60 FPS".to_owned(),
            Self::Fps120 => "120 FPS".to_owned(),
            Self::FollowDisplay => t!("system.follow_display").to_string(),
            Self::Custom(fps) => format!("{} FPS", fps.get()),
            Self::Unlimited => t!("system.unlimited").to_string(),
        }
    }

    pub(super) fn atomic_value(self) -> u32 {
        match self {
            Self::Fps30 => 30,
            Self::Fps60 => 60,
            Self::Fps120 => 120,
            Self::FollowDisplay => FOLLOW_DISPLAY_FRAME_RATE_VALUE,
            Self::Custom(fps) => CUSTOM_FRAME_RATE_TAG | u32::from(fps.get()),
            Self::Unlimited => UNLIMITED_FRAME_RATE_VALUE,
        }
    }

    pub(super) fn from_atomic_value(value: u32) -> Self {
        match value {
            UNLIMITED_FRAME_RATE_VALUE => Self::Unlimited,
            30 => Self::Fps30,
            60 => Self::Fps60,
            120 => Self::Fps120,
            FOLLOW_DISPLAY_FRAME_RATE_VALUE => Self::FollowDisplay,
            value if value & !FRAME_RATE_PAYLOAD_MASK == CUSTOM_FRAME_RATE_TAG => {
                let payload = value & FRAME_RATE_PAYLOAD_MASK;
                u16::try_from(payload)
                    .ok()
                    .and_then(NonZeroU16::new)
                    .map(Self::Custom)
                    .unwrap_or_default()
            }
            _ => Self::default(),
        }
    }
}

/// 控制日志宏送入 flexi_logger 的最低严重等级。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum LogLevel {
    /// 只记录错误。
    Error,
    /// 记录警告和错误。
    Warn,
    /// 记录常规运行信息。
    #[default]
    Info,
    /// 记录调试信息。
    Debug,
    /// 记录最详细的跟踪信息。
    Trace,
}

impl LogLevel {
    /// 返回配置文件中的稳定标识和 flexi_logger 可接受的过滤字符串。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    pub(super) fn from_id(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

/// 描述日志过滤和文件轮转策略；文件目录、异步写入和每日轮转周期由运行时固定。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LoggingSettings {
    /// 当前日志过滤等级。
    pub(crate) level: LogLevel,
    /// 是否启用按日期或文件大小轮转。
    pub(crate) rotation: bool,
    /// 轮转后的文件是否压缩为 gzip。
    pub(crate) compression: bool,
    /// 文件超过多少 MiB 时触发轮转。
    pub(crate) max_size_mb: u32,
    /// 最多保留多少个轮转文件。
    pub(crate) keep_files: u32,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            rotation: true,
            compression: true,
            max_size_mb: LOGGING_DEFAULT_MAX_SIZE_MB,
            keep_files: LOGGING_DEFAULT_KEEP_FILES,
        }
    }
}

impl LoggingSettings {
    /// 校验来自配置文件或 UI 的数值，避免把异常参数传给日志后台线程。
    pub(crate) fn normalized(self) -> Result<Self, String> {
        if !(LOGGING_MIN_FILE_SIZE_MB..=LOGGING_MAX_FILE_SIZE_MB).contains(&self.max_size_mb) {
            return Err(format!(
                "日志轮转大小必须在 {LOGGING_MIN_FILE_SIZE_MB} 到 {LOGGING_MAX_FILE_SIZE_MB} MiB 之间"
            ));
        }
        if !(LOGGING_MIN_KEEP_FILES..=LOGGING_MAX_KEEP_FILES).contains(&self.keep_files) {
            return Err(format!(
                "日志保留数量必须在 {LOGGING_MIN_KEEP_FILES} 到 {LOGGING_MAX_KEEP_FILES} 之间"
            ));
        }
        Ok(self)
    }

    /// 返回 flexi_logger 使用的字节轮转阈值。
    pub(crate) fn max_size_bytes(self) -> u64 {
        u64::from(self.max_size_mb) * 1024 * 1024
    }
}

impl TryFrom<u16> for FrameRate {
    type Error = FrameRateError;

    fn try_from(fps: u16) -> Result<Self, Self::Error> {
        match fps {
            0 => Err(FrameRateError { fps }),
            30 => Ok(Self::Fps30),
            60 => Ok(Self::Fps60),
            120 => Ok(Self::Fps120),
            _ => Self::custom(fps),
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
        write!(formatter, "自定义帧率必须是正整数，当前为 {} FPS", self.fps)
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

impl ConfigWriteError {
    /// 返回适合日志聚合的稳定分类，不暴露配置路径或用户输入。
    pub(crate) const fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::InvalidValue(_) => "invalid_value",
            #[cfg(test)]
            Self::StaleConfigUpdate => "stale_update",
            Self::Io { .. } => "io",
        }
    }
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
