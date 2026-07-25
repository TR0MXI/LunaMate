//! 统一管理应用配置领域、持久化快照与 revision 提交。
//!
//! 高频标量通过原子变量读取，模型选择通过 [`ArcSwap`] 发布不可变快照，窗口位置使用短临界区缓存。
//! 配置文件只在启动和显式保存时访问；渲染路径不读取磁盘。

mod appearance;
mod document;
mod llm;
mod types;

#[cfg(test)]
mod tests;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
    },
};

use arc_swap::ArcSwap;
use parking_lot::Mutex;
use rust_i18n::t;
use tokio::sync::watch;
use toml_edit::{DocumentMut, Value};

pub(crate) use appearance::{AppLanguage, AppearanceSettings, CustomThemeSettings, ThemePreset};
use document::{
    default_config_path, document_for_update, ensure_table_like, read_config_file, remove_key,
    set_item_value, validate_relative_path, write_appearance, write_config_file,
    write_logging_settings, write_window_position,
};
pub(crate) use llm::{LLM_PROVIDERS, LlmModelConfig, LlmProvider, LlmSettings, SharedLlmSettings};
use llm::{parse_llm_settings, write_llm_settings};
use types::{
    CUSTOM_FRAME_RATE_KEY, CUSTOM_FRAME_RATE_NAME, FOLLOW_DISPLAY_FRAME_RATE_NAME,
    UNLIMITED_FRAME_RATE_NAME,
};
pub(crate) use types::{
    CUSTOM_FRAME_RATE_MAX, CUSTOM_FRAME_RATE_MIN, ConfigWindow, ConfigWriteError, FrameRate,
    LOGGING_MAX_FILE_SIZE_MB, LOGGING_MAX_KEEP_FILES, LOGGING_MIN_FILE_SIZE_MB,
    LOGGING_MIN_KEEP_FILES, LogLevel, LoggingSettings, ModelWindowSize, WindowPosition,
};
/// 全局应用配置；首次访问时从用户配置目录加载，并兼容已有工作目录配置。
pub(crate) static CONFIG: LazyLock<LunaConfig> = LazyLock::new(LunaConfig::load);

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
    allow_agent_screenshot: bool,
    logging: LoggingSettings,
    appearance: AppearanceSettings,
    snapshot: ConfigSnapshot,
    window_positions: WindowPositions,
    llm: LlmSettings,
}

impl Default for LoadedConfig {
    fn default() -> Self {
        Self {
            frame_rate: FrameRate::default(),
            model_window_size: ModelWindowSize::default(),
            remember_window_positions: true,
            eye_tracking: true,
            show_fps: false,
            allow_agent_screenshot: false,
            logging: LoggingSettings::default(),
            appearance: AppearanceSettings::default(),
            snapshot: ConfigSnapshot::default(),
            window_positions: WindowPositions::default(),
            llm: LlmSettings::default(),
        }
    }
}

/// 保存 LunaMate 的全部运行时配置，并提供无锁读取与受控持久化。
pub(crate) struct LunaConfig {
    path: PathBuf,
    frame_rate: AtomicU32,
    model_window_size: AtomicU16,
    remember_window_positions: AtomicBool,
    eye_tracking: AtomicBool,
    show_fps: AtomicBool,
    allow_agent_screenshot: AtomicBool,
    requested_allow_agent_screenshot: AtomicBool,
    agent_screenshot_permission_retry_required: AtomicBool,
    applied_allow_agent_screenshot_revision: AtomicU64,
    agent_screenshot_permission_revision_sender: watch::Sender<u64>,
    logging: ArcSwap<LoggingSettings>,
    appearance: ArcSwap<AppearanceSettings>,
    snapshot: ArcSwap<ConfigSnapshot>,
    window_positions: Mutex<WindowPositions>,
    llm: ArcSwap<LlmSettings>,
    llm_request_revision: AtomicU64,
    model_request_revision: AtomicU64,
    frame_rate_request_revision: AtomicU64,
    model_window_size_request_revision: AtomicU64,
    remember_positions_request_revision: AtomicU64,
    eye_tracking_request_revision: AtomicU64,
    show_fps_request_revision: AtomicU64,
    allow_agent_screenshot_request_revision: AtomicU64,
    logging_request_revision: AtomicU64,
    appearance_request_revision: AtomicU64,
    reset_positions_request_revision: AtomicU64,
    write_nonce: AtomicU64,
    revision_lock: Mutex<()>,
    write_lock: Mutex<()>,
    window_position_write_lock: Mutex<()>,
    startup_warning: Option<String>,
}

impl LunaConfig {
    /// 从默认配置路径加载；任何读取或解析错误都会回退为完整默认值。
    fn load() -> Self {
        Self::load_from(default_config_path())
    }

    fn load_from(path: PathBuf) -> Self {
        let (loaded, startup_warning) = read_config_file(&path);
        let (agent_screenshot_permission_revision_sender, _) = watch::channel(0);
        if let Some(warning) = &startup_warning {
            log::warn!("{}", t!("log.startup_config_warning", warning = warning));
        }

        Self {
            path,
            frame_rate: AtomicU32::new(loaded.frame_rate.atomic_value()),
            model_window_size: AtomicU16::new(loaded.model_window_size.atomic_value()),
            remember_window_positions: AtomicBool::new(loaded.remember_window_positions),
            eye_tracking: AtomicBool::new(loaded.eye_tracking),
            show_fps: AtomicBool::new(loaded.show_fps),
            allow_agent_screenshot: AtomicBool::new(loaded.allow_agent_screenshot),
            requested_allow_agent_screenshot: AtomicBool::new(loaded.allow_agent_screenshot),
            agent_screenshot_permission_retry_required: AtomicBool::new(false),
            applied_allow_agent_screenshot_revision: AtomicU64::new(0),
            agent_screenshot_permission_revision_sender,
            logging: ArcSwap::from_pointee(loaded.logging),
            appearance: ArcSwap::from_pointee(loaded.appearance),
            snapshot: ArcSwap::from_pointee(loaded.snapshot),
            window_positions: Mutex::new(loaded.window_positions),
            llm: ArcSwap::from_pointee(loaded.llm),
            llm_request_revision: AtomicU64::new(0),
            model_request_revision: AtomicU64::new(0),
            frame_rate_request_revision: AtomicU64::new(0),
            model_window_size_request_revision: AtomicU64::new(0),
            remember_positions_request_revision: AtomicU64::new(0),
            eye_tracking_request_revision: AtomicU64::new(0),
            show_fps_request_revision: AtomicU64::new(0),
            allow_agent_screenshot_request_revision: AtomicU64::new(0),
            logging_request_revision: AtomicU64::new(0),
            appearance_request_revision: AtomicU64::new(0),
            reset_positions_request_revision: AtomicU64::new(0),
            write_nonce: AtomicU64::new(0),
            revision_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
            window_position_write_lock: Mutex::new(()),
            startup_warning,
        }
    }

    /// 返回启动配置诊断；该消息不会阻止用户继续修改并修复配置。
    pub(crate) fn startup_warning(&self) -> Option<&str> {
        self.startup_warning.as_deref()
    }

    /// 返回当前渲染帧率；该读取不会获取锁。
    pub(crate) fn frame_rate(&self) -> FrameRate {
        FrameRate::from_atomic_value(self.frame_rate.load(Ordering::Relaxed))
    }

    /// 返回桌宠主窗口的尺寸档位；该读取不会获取锁。
    pub(crate) fn model_window_size(&self) -> ModelWindowSize {
        ModelWindowSize::from_atomic_value(self.model_window_size.load(Ordering::Relaxed))
    }

    /// 返回是否在退出时保存并在下次启动时恢复窗口位置。
    pub(crate) fn remember_window_positions(&self) -> bool {
        self.remember_window_positions.load(Ordering::Relaxed)
    }

    /// 返回是否根据鼠标位置驱动模型的视线参数。
    pub(crate) fn eye_tracking(&self) -> bool {
        self.eye_tracking.load(Ordering::Relaxed)
    }

    /// 返回是否在桌宠窗口显示运行时帧率。
    pub(crate) fn show_fps(&self) -> bool {
        self.show_fps.load(Ordering::Relaxed)
    }

    /// 返回当前是否存在已持久化且未被更新请求撤销的 Agent 截屏授权。
    pub(crate) fn allow_agent_screenshot(&self) -> bool {
        self.agent_screenshot_permission_revision().is_some()
    }

    /// 返回设置界面最近一次请求的截屏授权状态；该值不代表权限已经持久化生效。
    pub(crate) fn requested_allow_agent_screenshot(&self) -> bool {
        self.requested_allow_agent_screenshot
            .load(Ordering::Acquire)
    }

    /// 返回是否仍需把 fail-closed 的关闭状态重试写入磁盘。
    pub(crate) fn agent_screenshot_permission_retry_required(&self) -> bool {
        self.agent_screenshot_permission_retry_required
            .load(Ordering::Acquire)
    }

    /// 订阅截屏授权请求 revision；新订阅者会立即看到当前 revision。
    pub(crate) fn subscribe_agent_screenshot_permission_revision(&self) -> watch::Receiver<u64> {
        self.agent_screenshot_permission_revision_sender.subscribe()
    }

    /// 返回当前有效截屏授权的 revision，供异步工具执行后复核。
    pub(crate) fn agent_screenshot_permission_revision(&self) -> Option<u64> {
        loop {
            let requested_revision = self
                .allow_agent_screenshot_request_revision
                .load(Ordering::Acquire);
            let applied_revision = self
                .applied_allow_agent_screenshot_revision
                .load(Ordering::Acquire);
            let allowed = self.allow_agent_screenshot.load(Ordering::Acquire);
            let requested = self
                .requested_allow_agent_screenshot
                .load(Ordering::Acquire);
            if requested_revision
                == self
                    .allow_agent_screenshot_request_revision
                    .load(Ordering::Acquire)
            {
                return (allowed && requested && applied_revision == requested_revision)
                    .then_some(requested_revision);
            }
        }
    }

    /// 检查一次已注册工具使用的授权是否仍未被用户撤销。
    pub(crate) fn agent_screenshot_permission_is_current(&self, revision: u64) -> bool {
        self.agent_screenshot_permission_revision() == Some(revision)
    }

    /// 返回当前日志过滤与轮转配置快照。
    pub(crate) fn logging_settings(&self) -> Arc<LoggingSettings> {
        self.logging.load_full()
    }

    /// 返回当前模型清单的相对路径快照。
    pub(crate) fn selected_model(&self) -> Option<PathBuf> {
        self.snapshot.load().selected_model.clone()
    }

    /// 返回一次性发布的界面语言和主题配置快照。
    pub(crate) fn appearance(&self) -> Arc<AppearanceSettings> {
        self.appearance.load_full()
    }

    /// 返回一次性发布的语言模型与系统提示词快照。
    pub(crate) fn llm_settings(&self) -> SharedLlmSettings {
        self.llm.load_full()
    }

    /// 返回指定窗口最近一次观察到的位置。
    pub(crate) fn window_position(&self, window: ConfigWindow) -> Option<WindowPosition> {
        self.window_positions.lock().window_position(window)
    }

    /// 更新帧率原子值，并准确修改 TOML 中对应键。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    #[cfg(test)]
    pub(crate) fn set_frame_rate(&self, frame_rate: FrameRate) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_frame_rate_revision();
        self.set_frame_rate_at_revision(frame_rate, revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }

    /// 为帧率写入分配单调 revision。
    pub(crate) fn reserve_frame_rate_revision(&self) -> u64 {
        self.reserve_request_revision(&self.frame_rate_request_revision)
    }

    /// 仅提交仍然最新的帧率写入。
    pub(crate) fn set_frame_rate_at_revision(
        &self,
        frame_rate: FrameRate,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.frame_rate_request_revision,
            revision,
            || {
                self.frame_rate
                    .store(frame_rate.atomic_value(), Ordering::Relaxed);
            },
            |document| {
                ensure_table_like(&mut document["render"]);
                if !matches!(frame_rate, FrameRate::Custom(_)) {
                    remove_key(document, "render", CUSTOM_FRAME_RATE_KEY);
                }
                let value = match frame_rate {
                    FrameRate::Fps30 => Value::from(30_i64),
                    FrameRate::Fps60 => Value::from(60_i64),
                    FrameRate::Fps120 => Value::from(120_i64),
                    FrameRate::FollowDisplay => Value::from(FOLLOW_DISPLAY_FRAME_RATE_NAME),
                    FrameRate::Custom(fps) => {
                        set_item_value(
                            &mut document["render"][CUSTOM_FRAME_RATE_KEY],
                            Value::from(i64::from(fps.get())),
                        );
                        Value::from(CUSTOM_FRAME_RATE_NAME)
                    }
                    FrameRate::Unlimited => Value::from(UNLIMITED_FRAME_RATE_NAME),
                };
                set_item_value(&mut document["render"]["frame_rate"], value);
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 为帧率显示开关写入分配单调 revision。
    pub(crate) fn reserve_show_fps_revision(&self) -> u64 {
        self.reserve_request_revision(&self.show_fps_request_revision)
    }

    /// 仅提交仍然最新的帧率显示开关写入。
    pub(crate) fn set_show_fps_at_revision(
        &self,
        show: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.show_fps_request_revision,
            revision,
            || self.show_fps.store(show, Ordering::Relaxed),
            |document| {
                ensure_table_like(&mut document["debug"]);
                set_item_value(&mut document["debug"]["show_fps"], Value::from(show));
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 为 Agent 截屏授权写入分配单调 revision。
    pub(crate) fn reserve_allow_agent_screenshot_revision(&self, allowed: bool) -> u64 {
        let _guard = self.revision_lock.lock();
        let revision = reserve_revision(&self.allow_agent_screenshot_request_revision);
        self.agent_screenshot_permission_revision_sender
            .send_replace(revision);
        self.requested_allow_agent_screenshot
            .store(allowed, Ordering::Release);
        self.agent_screenshot_permission_retry_required
            .store(false, Ordering::Release);
        revision
    }

    /// 仅提交仍然最新的 Agent 截屏授权；磁盘成功前不会开放权限。
    pub(crate) fn set_allow_agent_screenshot_at_revision(
        &self,
        allowed: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let _guard = self.write_lock.lock();
        if !revision_is_current(&self.allow_agent_screenshot_request_revision, revision) {
            return Ok(None);
        }
        if self
            .applied_allow_agent_screenshot_revision
            .load(Ordering::Acquire)
            == revision
            && self.allow_agent_screenshot.load(Ordering::Acquire) == allowed
            && self
                .requested_allow_agent_screenshot
                .load(Ordering::Acquire)
                == allowed
            && !self
                .agent_screenshot_permission_retry_required
                .load(Ordering::Acquire)
        {
            return Ok(Some(()));
        }

        let mut candidate_revision = revision;
        let mut candidate_allowed = allowed;
        loop {
            if let Err(error) = self.edit_document_locked(|document| {
                ensure_table_like(&mut document["tools"]);
                set_item_value(
                    &mut document["tools"]["allow_agent_screenshot"],
                    Value::from(candidate_allowed),
                );
            }) {
                let _revision_guard = self.revision_lock.lock();
                if revision_is_current(
                    &self.allow_agent_screenshot_request_revision,
                    candidate_revision,
                ) && self
                    .requested_allow_agent_screenshot
                    .load(Ordering::Acquire)
                    == candidate_allowed
                {
                    // 持久化结果不确定时不从旧磁盘值重新开放隐私权限。
                    self.allow_agent_screenshot.store(false, Ordering::Release);
                    self.requested_allow_agent_screenshot
                        .store(false, Ordering::Release);
                    self.agent_screenshot_permission_retry_required
                        .store(!candidate_allowed, Ordering::Release);
                }
                return Err(error);
            }

            let _revision_guard = self.revision_lock.lock();
            let current_revision = self
                .allow_agent_screenshot_request_revision
                .load(Ordering::Acquire);
            let current_allowed = self
                .requested_allow_agent_screenshot
                .load(Ordering::Acquire);
            if current_revision == candidate_revision && current_allowed == candidate_allowed {
                self.allow_agent_screenshot
                    .store(candidate_allowed, Ordering::Release);
                self.applied_allow_agent_screenshot_revision
                    .store(candidate_revision, Ordering::Release);
                self.agent_screenshot_permission_retry_required
                    .store(false, Ordering::Release);
                return Ok((candidate_revision == revision).then_some(()));
            }
            candidate_revision = current_revision;
            candidate_allowed = current_allowed;
        }
    }

    /// 更新完整日志配置并持久化。
    ///
    /// # Errors
    ///
    /// 配置值不合法，或配置目录和文件无法读写时返回错误。
    #[cfg(test)]
    pub(crate) fn set_logging_settings(
        &self,
        settings: LoggingSettings,
    ) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_logging_settings_revision();
        self.set_logging_settings_at_revision(settings, revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }

    /// 为完整日志配置写入分配单调 revision。
    pub(crate) fn reserve_logging_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.logging_request_revision)
    }

    /// 仅持久化并发布仍是最新请求的日志配置。
    pub(crate) fn set_logging_settings_at_revision(
        &self,
        settings: LoggingSettings,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let settings = settings
            .normalized()
            .map_err(ConfigWriteError::InvalidValue)?;
        let published = Arc::new(settings);
        let applied = self.edit_config_at_revision(
            &self.logging_request_revision,
            revision,
            move || self.logging.store(published),
            move |document| write_logging_settings(document, &settings),
        )?;
        Ok(applied.then_some(()))
    }

    /// 为桌宠主窗口尺寸写入分配单调 revision。
    pub(crate) fn reserve_model_window_size_revision(&self) -> u64 {
        self.reserve_request_revision(&self.model_window_size_request_revision)
    }

    /// 仅提交仍然最新的桌宠主窗口尺寸写入。
    pub(crate) fn set_model_window_size_at_revision(
        &self,
        size: ModelWindowSize,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.model_window_size_request_revision,
            revision,
            || {
                self.model_window_size
                    .store(size.atomic_value(), Ordering::Relaxed);
            },
            |document| {
                ensure_table_like(&mut document["window"]);
                set_item_value(
                    &mut document["window"]["model_size"],
                    Value::from(size.id()),
                );
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 为眼部跟随开关写入分配单调 revision。
    pub(crate) fn reserve_eye_tracking_revision(&self) -> u64 {
        self.reserve_request_revision(&self.eye_tracking_request_revision)
    }

    /// 仅提交仍然最新的眼部跟随开关写入。
    pub(crate) fn set_eye_tracking_at_revision(
        &self,
        enabled: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.eye_tracking_request_revision,
            revision,
            || self.eye_tracking.store(enabled, Ordering::Relaxed),
            |document| {
                ensure_table_like(&mut document["interaction"]);
                set_item_value(
                    &mut document["interaction"]["eye_tracking"],
                    Value::from(enabled),
                );
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 为外观配置写入分配单调 revision。
    pub(crate) fn reserve_appearance_revision(&self) -> u64 {
        self.reserve_request_revision(&self.appearance_request_revision)
    }

    /// 校验、持久化并一次性发布最新的外观配置。
    pub(crate) fn set_appearance_at_revision(
        &self,
        settings: AppearanceSettings,
        revision: u64,
    ) -> Result<Option<Arc<AppearanceSettings>>, ConfigWriteError> {
        let settings = Arc::new(
            settings
                .normalized()
                .map_err(ConfigWriteError::InvalidValue)?,
        );
        let _guard = self.write_lock.lock();
        if !revision_is_current(&self.appearance_request_revision, revision) {
            return Ok(None);
        }
        self.edit_document_locked(|document| write_appearance(document, &settings))?;
        // reservation 与发布共享短锁，确保复核通过后不会插入更新的 revision。
        let _revision_guard = self.revision_lock.lock();
        if !revision_is_current(&self.appearance_request_revision, revision) {
            return Ok(None);
        }
        self.appearance.store(settings.clone());
        Ok(Some(settings))
    }

    /// 更新窗口位置保存开关，并准确修改 TOML 中对应键。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    #[cfg(test)]
    pub(crate) fn set_remember_window_positions(
        &self,
        remember: bool,
    ) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_remember_positions_revision();
        self.set_remember_window_positions_at_revision(remember, revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }

    /// 为窗口位置记忆开关写入分配单调 revision。
    pub(crate) fn reserve_remember_positions_revision(&self) -> u64 {
        self.reserve_request_revision(&self.remember_positions_request_revision)
    }

    /// 仅提交仍然最新的窗口位置记忆开关写入。
    pub(crate) fn set_remember_window_positions_at_revision(
        &self,
        remember: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let _position_guard = self.window_position_write_lock.lock();
        let applied = self.edit_config_at_revision(
            &self.remember_positions_request_revision,
            revision,
            || {
                self.remember_window_positions
                    .store(remember, Ordering::Relaxed);
            },
            |document| {
                ensure_table_like(&mut document["window"]);
                set_item_value(
                    &mut document["window"]["remember_position"],
                    Value::from(remember),
                );
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 为模型选择写入分配单调 revision。
    pub(crate) fn reserve_model_revision(&self) -> u64 {
        self.reserve_request_revision(&self.model_request_revision)
    }

    /// 仅提交仍然最新的模型选择写入。
    pub(crate) fn set_selected_model_at_revision(
        &self,
        relative_path: Option<&Path>,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let selected_model = match relative_path {
            Some(path) => Some(validate_relative_path(path)?),
            None => None,
        };
        let applied = self.edit_config_at_revision(
            &self.model_request_revision,
            revision,
            || {
                self.snapshot.rcu(|current| {
                    let mut next = ConfigSnapshot::clone(current);
                    next.selected_model.clone_from(&selected_model);
                    Arc::new(next)
                });
            },
            |document| match selected_model.as_ref() {
                Some(path) => {
                    ensure_table_like(&mut document["model"]);
                    set_item_value(
                        &mut document["model"]["selected"],
                        Value::from(path.to_string_lossy().into_owned()),
                    );
                }
                None => remove_key(document, "model", "selected"),
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 为一份由 Agent 设置编辑器提交的草稿分配单调 revision。
    pub(crate) fn reserve_llm_settings_revision(&self) -> u64 {
        self.reserve_request_revision(&self.llm_request_revision)
    }

    /// 仅当该草稿仍是最新请求时才写入并发布；旧后台任务会被无害丢弃。
    ///
    /// # Errors
    ///
    /// 模型字段不合法，或配置文件无法持久化时返回错误。
    pub(crate) fn set_llm_settings_at_revision(
        &self,
        settings: LlmSettings,
        revision: u64,
    ) -> Result<Option<SharedLlmSettings>, ConfigWriteError> {
        let settings = Arc::new(settings.normalized()?);
        let _guard = self.write_lock.lock();
        if self.llm_request_revision.load(Ordering::Relaxed) != revision {
            return Ok(None);
        }
        self.edit_document_locked(|document| write_llm_settings(document, &settings))?;
        // reservation 与发布共享短锁，确保复核通过后不会插入更新的 revision。
        let _revision_guard = self.revision_lock.lock();
        if self.llm_request_revision.load(Ordering::Relaxed) != revision {
            return Ok(None);
        }
        self.llm.store(settings.clone());
        Ok(Some(settings))
    }

    /// 只更新内存中的窗口位置快照；拖动期间不会访问磁盘。
    pub(crate) fn cache_window_position(&self, window: ConfigWindow, position: WindowPosition) {
        let mut positions = self.window_positions.lock();
        if positions.window_position(window) != Some(position) {
            positions.set_window_position(window, Some(position));
        }
    }

    /// 将已缓存的两个窗口位置集中写入配置文件。
    ///
    /// 关闭位置保存时该方法不访问磁盘。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    pub(crate) fn persist_window_positions(&self) -> Result<(), ConfigWriteError> {
        let _position_guard = self.window_position_write_lock.lock();
        if !self.remember_window_positions() {
            return Ok(());
        }
        let positions = *self.window_positions.lock();
        self.edit_document(|document| {
            write_window_position(document, ConfigWindow::DesktopPet, positions.desktop_pet);
            write_window_position(document, ConfigWindow::Settings, positions.settings);
        })
    }

    /// 清除内存和配置文件中保存的全部窗口位置。
    ///
    /// # Errors
    ///
    /// 配置目录或文件无法读取、创建或写入时返回错误。
    #[cfg(test)]
    pub(crate) fn reset_window_positions(&self) -> Result<(), ConfigWriteError> {
        let revision = self.reserve_reset_positions_revision();
        self.reset_window_positions_at_revision(revision)?
            .ok_or(ConfigWriteError::StaleConfigUpdate)
    }

    /// 为窗口位置重置写入分配单调 revision。
    pub(crate) fn reserve_reset_positions_revision(&self) -> u64 {
        self.reserve_request_revision(&self.reset_positions_request_revision)
    }

    /// 仅提交仍然最新的窗口位置重置。
    pub(crate) fn reset_window_positions_at_revision(
        &self,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let _position_guard = self.window_position_write_lock.lock();
        let applied = self.edit_config_at_revision(
            &self.reset_positions_request_revision,
            revision,
            || {
                *self.window_positions.lock() = WindowPositions::default();
            },
            |document| {
                remove_key(document, "window", ConfigWindow::DesktopPet.table_name());
                remove_key(document, "window", ConfigWindow::Settings.table_name());
            },
        )?;
        Ok(applied.then_some(()))
    }

    fn edit_document(&self, edit: impl FnOnce(&mut DocumentMut)) -> Result<(), ConfigWriteError> {
        let _guard = self.write_lock.lock();
        self.edit_document_locked(edit)
    }

    fn reserve_request_revision(&self, counter: &AtomicU64) -> u64 {
        let _guard = self.revision_lock.lock();
        reserve_revision(counter)
    }

    fn edit_document_locked(
        &self,
        edit: impl FnOnce(&mut DocumentMut),
    ) -> Result<(), ConfigWriteError> {
        let mut document = document_for_update(&self.path)?;
        edit(&mut document);

        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| ConfigWriteError::Io {
                operation: "创建配置目录",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let nonce = reserve_revision(&self.write_nonce);
        write_config_file(&self.path, &document, nonce)
    }

    fn edit_config_at_revision(
        &self,
        counter: &AtomicU64,
        revision: u64,
        publish: impl FnOnce(),
        edit: impl FnOnce(&mut DocumentMut),
    ) -> Result<bool, ConfigWriteError> {
        let _guard = self.write_lock.lock();
        if !revision_is_current(counter, revision) {
            return Ok(false);
        }
        self.edit_document_locked(edit)?;
        // 磁盘写入期间允许新请求递增 revision；最终复核与发布必须保持原子顺序。
        let _revision_guard = self.revision_lock.lock();
        if !revision_is_current(counter, revision) {
            return Ok(false);
        }
        publish();
        Ok(true)
    }
}

fn reserve_revision(counter: &AtomicU64) -> u64 {
    counter
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
        .max(1)
}

fn revision_is_current(counter: &AtomicU64, revision: u64) -> bool {
    counter.load(Ordering::Relaxed) == revision
}
