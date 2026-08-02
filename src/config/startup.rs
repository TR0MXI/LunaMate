//! 负责启动配置装配、Agent 跨域终检与诊断聚合。

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64},
    },
};

use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use tokio::sync::watch;

use super::{
    AgentConfigSnapshot, AppLanguage, AppearanceSettings, ConfigSnapshot, FrameRate, LlmSettings,
    LoadedConfig, LoggingSettings, LunaConfig, ModelResourceSettings, ModelWindowSize,
    PersonaSettings, ShortcutSettings, VoiceSettings, document::read_config_file,
};

impl Default for LoadedConfig {
    fn default() -> Self {
        Self {
            frame_rate: FrameRate::default(),
            model_window_size: ModelWindowSize::default(),
            remember_window_positions: true,
            eye_tracking: true,
            show_fps: false,
            use_native_tray_menu: false,
            allow_agent_screenshot: false,
            allow_agent_outfit_change: true,
            logging: LoggingSettings::default(),
            appearance: AppearanceSettings::default(),
            snapshot: ConfigSnapshot::default(),
            window_positions: super::WindowPositions::default(),
            llm: LlmSettings::default(),
            persona: PersonaSettings::default_for(AppLanguage::default()),
            shortcuts: ShortcutSettings::default(),
            voice: VoiceSettings::default(),
            model_resources: ModelResourceSettings::default(),
        }
    }
}

impl LunaConfig {
    pub(super) fn load_from_optional_path(path: Option<PathBuf>) -> Self {
        let (mut loaded, mut startup_warning) = match path.as_deref() {
            Some(path) => read_config_file(path),
            None => (
                LoadedConfig::default(),
                Some(
                    "无法确定平台用户配置目录，已使用不可持久化的默认配置；当前会话中的配置修改无法保存"
                        .to_owned(),
                ),
            ),
        };
        let (agent_screenshot_permission_revision_sender, _) = watch::channel(0);
        let agent_config = finalize_loaded_agent_config(&mut loaded, &mut startup_warning);
        Self {
            path,
            frame_rate: AtomicU32::new(loaded.frame_rate.atomic_value()),
            model_window_size: AtomicU16::new(loaded.model_window_size.atomic_value()),
            remember_window_positions: AtomicBool::new(loaded.remember_window_positions),
            eye_tracking: AtomicBool::new(loaded.eye_tracking),
            show_fps: AtomicBool::new(loaded.show_fps),
            use_native_tray_menu: AtomicBool::new(loaded.use_native_tray_menu),
            allow_agent_screenshot: AtomicBool::new(loaded.allow_agent_screenshot),
            allow_agent_outfit_change: AtomicBool::new(loaded.allow_agent_outfit_change),
            requested_allow_agent_screenshot: AtomicBool::new(loaded.allow_agent_screenshot),
            agent_screenshot_permission_retry_required: AtomicBool::new(false),
            applied_allow_agent_screenshot_revision: AtomicU64::new(0),
            agent_screenshot_permission_revision_sender,
            agent_screenshot_execution_gate: RwLock::new(()),
            logging: ArcSwap::from_pointee(loaded.logging),
            appearance: ArcSwap::from_pointee(loaded.appearance),
            snapshot: ArcSwap::from_pointee(loaded.snapshot),
            window_positions: Mutex::new(loaded.window_positions),
            llm: ArcSwap::from_pointee(loaded.llm),
            persona: ArcSwap::from_pointee(loaded.persona),
            shortcuts: ArcSwap::from_pointee(loaded.shortcuts),
            voice: ArcSwap::from_pointee(loaded.voice),
            model_resources: ArcSwap::from_pointee(loaded.model_resources),
            agent_config: ArcSwap::from_pointee(agent_config),
            llm_request_revision: AtomicU64::new(0),
            persona_request_revision: AtomicU64::new(0),
            shortcut_request_revision: AtomicU64::new(0),
            voice_request_revision: AtomicU64::new(0),
            model_request_revision: AtomicU64::new(0),
            model_resources_request_revision: AtomicU64::new(0),
            frame_rate_request_revision: AtomicU64::new(0),
            model_window_size_request_revision: AtomicU64::new(0),
            remember_positions_request_revision: AtomicU64::new(0),
            eye_tracking_request_revision: AtomicU64::new(0),
            show_fps_request_revision: AtomicU64::new(0),
            use_native_tray_menu_request_revision: AtomicU64::new(0),
            allow_agent_screenshot_request_revision: AtomicU64::new(0),
            allow_agent_outfit_change_request_revision: AtomicU64::new(0),
            logging_request_revision: AtomicU64::new(0),
            appearance_request_revision: AtomicU64::new(0),
            reset_positions_request_revision: AtomicU64::new(0),
            write_nonce: AtomicU64::new(0),
            revision_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
            window_position_write_lock: Mutex::new(()),
            #[cfg(test)]
            prepare_commit_barrier_for_test: Mutex::new(None),
            #[cfg(test)]
            prepare_failure_for_test: AtomicBool::new(false),
            #[cfg(test)]
            parent_sync_barrier_for_test: Mutex::new(None),
            #[cfg(all(test, unix))]
            parent_sync_failure_for_test: AtomicBool::new(false),
            startup_warning,
        }
    }
}

pub(super) fn finalize_loaded_agent_config(
    loaded: &mut LoadedConfig,
    startup_warning: &mut Option<String>,
) -> AgentConfigSnapshot {
    let language = loaded.appearance.language;
    match AgentConfigSnapshot::try_new(
        1,
        Arc::new(loaded.llm.clone()),
        Arc::new(loaded.persona.clone()),
        language,
    ) {
        Ok(snapshot) => {
            loaded.llm = snapshot.settings().as_ref().clone();
            loaded.persona = snapshot.personas().as_ref().clone();
            snapshot
        }
        Err(error) => {
            append_startup_warning(
                startup_warning,
                format!("Agent 配置解析结果不一致，Provider 与人格配置已整体回退默认值：{error}"),
                language,
            );
            let settings = Arc::new(LlmSettings::default());
            let personas = Arc::new(PersonaSettings::default_for(language));
            // 此分支只依赖代码内置默认值，配置输入已在上方完整丢弃。
            let snapshot = match AgentConfigSnapshot::try_new(1, settings, personas, language) {
                Ok(snapshot) => snapshot,
                Err(error) => panic!("内置默认 Agent 配置违反代码不变量：{error}"),
            };
            loaded.llm = snapshot.settings().as_ref().clone();
            loaded.persona = snapshot.personas().as_ref().clone();
            snapshot
        }
    }
}

fn append_startup_warning(
    startup_warning: &mut Option<String>,
    warning: String,
    language: AppLanguage,
) {
    match startup_warning {
        Some(current) => {
            current.push_str(
                rust_i18n::t!("common.status_separator", locale = language.id()).as_ref(),
            );
            current.push_str(&warning);
        }
        None => *startup_warning = Some(warning),
    }
}
