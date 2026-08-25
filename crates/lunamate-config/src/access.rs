//! 提供配置运行时快照的只读访问。

use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

use parking_lot::RwLockReadGuard;
use tokio::sync::watch;

use super::{
    AgentConfigSnapshot, AppearanceSettings, ConfigWindow, FrameRate, LoggingSettings, LunaConfig,
    ModelWindowSize, SharedLlmSettings, SharedModelResourceSettings, SharedPersonaSettings,
    SharedVoiceSettings, ShortcutSettings, WindowPosition,
};

impl LunaConfig {
    /// 返回启动配置诊断；该消息不会阻止用户继续修改并修复配置。
    pub fn startup_warning(&self) -> Option<&str> {
        self.startup_warning.as_deref()
    }

    /// 在日志过滤等级生效后记录不含路径、凭据或自由文本的启动配置摘要。
    pub fn log_startup_summary(&self) {
        if self.startup_warning.is_some() {
            log::warn!("event=startup_config_fallback warning=true");
        }
        let agent = self.agent_config.load();
        log::info!(
            "event=config_loaded warning={} providers={} personas={} model_selected={} shortcuts={} voice_mode={}",
            self.startup_warning.is_some(),
            agent.settings().models.len(),
            agent.personas().personas.len(),
            agent.settings().selected_model.is_some(),
            self.shortcuts.load().configured_count(),
            self.voice.load().mode.id()
        );
    }

    /// 返回当前渲染帧率；该读取不会获取锁。
    pub fn frame_rate(&self) -> FrameRate {
        FrameRate::from_atomic_value(self.frame_rate.load(Ordering::Relaxed))
    }

    /// 返回桌宠主窗口的尺寸档位；该读取不会获取锁。
    pub fn model_window_size(&self) -> ModelWindowSize {
        ModelWindowSize::from_atomic_value(self.model_window_size.load(Ordering::Relaxed))
    }

    /// 返回是否在退出时保存并在下次启动时恢复窗口位置。
    pub fn remember_window_positions(&self) -> bool {
        self.remember_window_positions.load(Ordering::Relaxed)
    }

    /// 返回是否根据鼠标位置驱动模型的视线参数。
    pub fn eye_tracking(&self) -> bool {
        self.eye_tracking.load(Ordering::Relaxed)
    }

    /// 返回是否在桌宠窗口显示运行时帧率。
    pub fn show_fps(&self) -> bool {
        self.show_fps.load(Ordering::Relaxed)
    }

    /// 返回是否强制使用系统提供的托盘右键菜单。
    pub fn use_native_tray_menu(&self) -> bool {
        self.use_native_tray_menu.load(Ordering::Relaxed)
    }

    /// 返回当前是否存在已持久化且未被更新请求撤销的 Agent 截屏授权。
    pub fn allow_agent_screenshot(&self) -> bool {
        self.agent_screenshot_permission_revision().is_some()
    }

    /// 返回是否允许 Agent 为当前 Live2D 模型注册并执行换装工具。
    pub fn allow_agent_outfit_change(&self) -> bool {
        self.allow_agent_outfit_change.load(Ordering::Relaxed)
    }

    /// 返回设置界面最近一次请求的截屏授权状态；该值不代表权限已经持久化生效。
    pub fn requested_allow_agent_screenshot(&self) -> bool {
        self.requested_allow_agent_screenshot
            .load(Ordering::Acquire)
    }

    /// 返回是否仍需把 fail-closed 的关闭状态重试写入磁盘。
    pub fn agent_screenshot_permission_retry_required(&self) -> bool {
        self.agent_screenshot_permission_retry_required
            .load(Ordering::Acquire)
    }

    /// 订阅截屏授权请求 revision；新订阅者会立即看到当前 revision。
    pub fn subscribe_agent_screenshot_permission_revision(&self) -> watch::Receiver<u64> {
        self.agent_screenshot_permission_revision_sender.subscribe()
    }

    /// 返回当前有效截屏授权的 revision，供异步工具执行后复核。
    pub fn agent_screenshot_permission_revision(&self) -> Option<u64> {
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
    pub fn agent_screenshot_permission_is_current(&self, revision: u64) -> bool {
        self.agent_screenshot_permission_revision() == Some(revision)
    }

    /// 在授权 revision 仍有效时取得一次截图任务启动租约。
    pub fn begin_agent_screenshot_capture(&self, revision: u64) -> Option<RwLockReadGuard<'_, ()>> {
        let guard = self.agent_screenshot_execution_gate.read();
        self.agent_screenshot_permission_is_current(revision)
            .then_some(guard)
    }

    /// 返回当前日志过滤与轮转配置快照。
    pub fn logging_settings(&self) -> Arc<LoggingSettings> {
        self.logging.load_full()
    }

    /// 返回当前模型清单的相对路径快照。
    pub fn selected_model(&self) -> Option<PathBuf> {
        self.snapshot.load().selected_model.clone()
    }

    /// 返回模型动作、表情与服装的显示名和分类覆盖快照。
    pub fn model_resource_settings(&self) -> SharedModelResourceSettings {
        self.model_resources.load_full()
    }

    /// 返回一次性发布的界面语言和主题配置快照。
    pub fn appearance(&self) -> Arc<AppearanceSettings> {
        self.appearance.load_full()
    }

    /// 返回一次性发布的供应商目录快照。
    pub fn llm_settings(&self) -> SharedLlmSettings {
        self.agent_config.load_full().settings().clone()
    }

    /// 返回一次性发布的人格目录与当前人格快照。
    pub fn persona_settings(&self) -> SharedPersonaSettings {
        self.agent_config.load_full().personas().clone()
    }

    /// 返回一次原子发布的 Agent 配置与 generation。
    pub fn agent_config_snapshot(&self) -> AgentConfigSnapshot {
        self.agent_config.load_full().as_ref().clone()
    }

    /// 返回四个应用动作的一次性快捷键配置快照。
    pub fn shortcut_settings(&self) -> Arc<ShortcutSettings> {
        self.shortcuts.load_full()
    }

    /// 返回一次性发布的本地语音配置快照。
    pub fn voice_settings(&self) -> SharedVoiceSettings {
        self.voice.load_full()
    }

    /// 返回指定窗口最近一次观察到的位置。
    pub fn window_position(&self, window: ConfigWindow) -> Option<WindowPosition> {
        self.window_positions.lock().window_position(window)
    }
}
