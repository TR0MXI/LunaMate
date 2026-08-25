//! 统一管理应用配置领域、持久化快照与 revision 提交。
//!
//! 高频标量通过原子变量读取，模型选择通过 [`ArcSwap`] 发布不可变快照，窗口位置使用短临界区缓存。
//! 配置文件只在启动和显式保存时访问；渲染路径不读取磁盘。

mod access;
mod agent;
mod appearance;
mod atomic_file;
mod commit;
mod document;
mod llm;
mod model;
mod persistence;
mod persona;
mod revision;
mod shortcut;
mod startup;
mod types;
mod voice;
mod window;

#[cfg(test)]
mod tests;

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64},
};

use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
#[cfg(test)]
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::watch;

pub use agent::AgentConfigSnapshot;
pub use appearance::{AppearanceSettings, CustomThemeSettings, ThemePreset};
use document::{
    ensure_table_like, remove_key, set_item_value, table_like_section, validate_relative_path,
    write_appearance, write_logging_settings, write_window_position,
};
use llm::{parse_llm_settings, write_llm_settings};
pub use lunamate_agent::config::{
    AppLanguage, LlmSettings, PersonaSettings, SharedLlmSettings, SharedPersonaSettings,
};
#[cfg(test)]
pub use lunamate_agent::config::{
    CONTEXT_MESSAGES_MIN, CONTEXT_TOKENS_MAX, DEFAULT_CONTEXT_MESSAGES, DEFAULT_CONTEXT_TOKENS,
    DEFAULT_PERSONA_ID, LLM_PROVIDERS, LlmAdvancedOptions, LlmModelConfig, LlmProvider,
    MAX_OUTPUT_TOKENS_MAX, MODEL_CONTEXT_TOKENS_MAX, ModelKind, ModelProvider, PersonaConfig,
    PersonaContextLimits, REASONING_EFFORT_LEVELS, ReasoningEffort, TEMPERATURE_MAX,
    llm_provider_from_id, llm_provider_id,
};
pub use model::{
    ModelExpressionCategory, ModelResourceKey, ModelResourceKind, ModelResourceSettings,
    SharedModelResourceSettings,
};
use model::{parse_model_resource_settings, write_model_resource_settings};
use persona::{clear_invalid_model_bindings, parse_persona_settings, write_persona_settings};
pub use shortcut::{KeyboardShortcut, ShortcutAction, ShortcutSettings};
use shortcut::{parse_shortcut_settings, write_shortcut_settings};
#[cfg(test)]
use startup::finalize_loaded_agent_config;
use types::{
    CUSTOM_FRAME_RATE_KEY, CUSTOM_FRAME_RATE_NAME, FOLLOW_DISPLAY_FRAME_RATE_NAME,
    UNLIMITED_FRAME_RATE_NAME,
};
pub use types::{
    CUSTOM_FRAME_RATE_MAX, CUSTOM_FRAME_RATE_MIN, ConfigWindow, ConfigWriteError, FrameRate,
    LOGGING_MAX_FILE_SIZE_MB, LOGGING_MAX_KEEP_FILES, LOGGING_MIN_FILE_SIZE_MB,
    LOGGING_MIN_KEEP_FILES, LogLevel, LoggingSettings, ModelWindowSize, WindowPosition,
};
pub use voice::{
    SharedVoiceRuntimeSettings, SharedVoiceSettings, VoiceMode, VoiceRuntimeSettings,
    VoiceSettings, VoiceTranscriptionBackend,
};
use voice::{parse_voice_settings, write_voice_settings};

#[derive(Clone, Debug, Default)]
struct ConfigSnapshot {
    selected_model: Option<PathBuf>,
}

/// 保存两个窗口的最新逻辑位置；只在 bounds 事件和持久化边界短暂加锁。
#[derive(Clone, Copy, Debug, Default)]
struct WindowPositions {
    desktop_pet: Option<WindowPosition>,
    settings: Option<WindowPosition>,
}

impl WindowPositions {
    fn window_position(&self, window: ConfigWindow) -> Option<WindowPosition> {
        match window {
            ConfigWindow::DesktopPet => self.desktop_pet,
            ConfigWindow::Settings => self.settings,
        }
    }

    fn set_window_position(&mut self, window: ConfigWindow, position: Option<WindowPosition>) {
        match window {
            ConfigWindow::DesktopPet => self.desktop_pet = position,
            ConfigWindow::Settings => self.settings = position,
        }
    }
}

#[derive(Clone, Debug)]
struct LoadedConfig {
    frame_rate: FrameRate,
    model_window_size: ModelWindowSize,
    remember_window_positions: bool,
    eye_tracking: bool,
    show_fps: bool,
    use_native_tray_menu: bool,
    allow_agent_screenshot: bool,
    allow_agent_outfit_change: bool,
    logging: LoggingSettings,
    appearance: AppearanceSettings,
    snapshot: ConfigSnapshot,
    window_positions: WindowPositions,
    llm: LlmSettings,
    persona: PersonaSettings,
    shortcuts: ShortcutSettings,
    voice: VoiceSettings,
    model_resources: ModelResourceSettings,
}

/// 保存 LunaMate 的全部运行时配置，并提供无锁读取与受控持久化。
pub struct LunaConfig {
    path: Option<PathBuf>,
    frame_rate: AtomicU32,
    model_window_size: AtomicU16,
    remember_window_positions: AtomicBool,
    eye_tracking: AtomicBool,
    show_fps: AtomicBool,
    use_native_tray_menu: AtomicBool,
    allow_agent_screenshot: AtomicBool,
    allow_agent_outfit_change: AtomicBool,
    requested_allow_agent_screenshot: AtomicBool,
    agent_screenshot_permission_retry_required: AtomicBool,
    applied_allow_agent_screenshot_revision: AtomicU64,
    agent_screenshot_permission_revision_sender: watch::Sender<u64>,
    agent_screenshot_execution_gate: RwLock<()>,
    logging: ArcSwap<LoggingSettings>,
    appearance: ArcSwap<AppearanceSettings>,
    snapshot: ArcSwap<ConfigSnapshot>,
    window_positions: Mutex<WindowPositions>,
    shortcuts: ArcSwap<ShortcutSettings>,
    voice: ArcSwap<VoiceSettings>,
    model_resources: ArcSwap<ModelResourceSettings>,
    agent_config: ArcSwap<AgentConfigSnapshot>,
    llm_request_revision: AtomicU64,
    persona_request_revision: AtomicU64,
    shortcut_request_revision: AtomicU64,
    voice_request_revision: AtomicU64,
    model_request_revision: AtomicU64,
    model_resources_request_revision: AtomicU64,
    frame_rate_request_revision: AtomicU64,
    model_window_size_request_revision: AtomicU64,
    remember_positions_request_revision: AtomicU64,
    eye_tracking_request_revision: AtomicU64,
    show_fps_request_revision: AtomicU64,
    use_native_tray_menu_request_revision: AtomicU64,
    allow_agent_screenshot_request_revision: AtomicU64,
    allow_agent_outfit_change_request_revision: AtomicU64,
    logging_request_revision: AtomicU64,
    appearance_request_revision: AtomicU64,
    reset_positions_request_revision: AtomicU64,
    write_nonce: AtomicU64,
    revision_lock: Mutex<()>,
    write_lock: Mutex<()>,
    window_position_write_lock: Mutex<()>,
    #[cfg(test)]
    prepare_commit_barrier_for_test: Mutex<Option<Arc<std::sync::Barrier>>>,
    #[cfg(test)]
    prepare_failure_for_test: AtomicBool,
    #[cfg(test)]
    parent_sync_barrier_for_test: Mutex<Option<Arc<std::sync::Barrier>>>,
    #[cfg(all(test, unix))]
    parent_sync_failure_for_test: AtomicBool,
    startup_warning: Option<String>,
}

impl LunaConfig {
    /// 从默认配置路径加载；任何读取或解析错误都会回退为完整默认值。
    pub fn load() -> Self {
        Self::load_from_optional_path(document::default_config_path())
    }

    #[cfg(test)]
    fn load_from(path: PathBuf) -> Self {
        Self::load_from_optional_path(Some(path))
    }

    #[cfg(test)]
    fn set_prepare_commit_barrier_for_test(&self, barrier: Arc<std::sync::Barrier>) {
        *self.prepare_commit_barrier_for_test.lock() = Some(barrier);
    }

    #[cfg(test)]
    fn pause_after_prepare_for_test(&self) {
        let barrier = self.prepare_commit_barrier_for_test.lock().take();
        if let Some(barrier) = barrier {
            barrier.wait();
            barrier.wait();
        }
    }

    #[cfg(test)]
    fn fail_next_prepare_for_test(&self) {
        self.prepare_failure_for_test.store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn set_parent_sync_barrier_for_test(&self, barrier: Arc<std::sync::Barrier>) {
        *self.parent_sync_barrier_for_test.lock() = Some(barrier);
    }

    #[cfg(all(test, unix))]
    fn fail_next_parent_sync_for_test(&self) {
        self.parent_sync_failure_for_test
            .store(true, Ordering::Release);
    }

    /// 更新帧率原子值，并准确修改 TOML 中对应键。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    #[cfg(test)]
    pub fn set_frame_rate(&self, frame_rate: FrameRate) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_frame_rate_revision();
        self.set_frame_rate_at_revision(frame_rate, revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }

    /// 更新完整日志配置并持久化。
    ///
    /// # Errors
    ///
    /// 配置值不合法，或配置目录和文件无法读写时返回错误。
    #[cfg(test)]
    pub fn set_logging_settings(&self, settings: LoggingSettings) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_logging_settings_revision();
        self.set_logging_settings_at_revision(settings, revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }

    /// 更新窗口位置保存开关，并准确修改 TOML 中对应键。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    #[cfg(test)]
    pub fn set_remember_window_positions(&self, remember: bool) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_remember_positions_revision();
        self.set_remember_window_positions_at_revision(remember, revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }

    /// 清除内存和配置文件中保存的全部窗口位置。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    #[cfg(test)]
    pub fn reset_window_positions(&self) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_reset_positions_revision();
        self.reset_window_positions_at_revision(revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }
}
