//! 校验、持久化并发布各配置领域的最新请求。

use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};

use toml_edit::Value;

use super::document::{replace_config_file, sync_config_file_parent};
use super::{
    AgentConfigSnapshot, AppLanguage, AppearanceSettings, CUSTOM_FRAME_RATE_KEY,
    CUSTOM_FRAME_RATE_NAME, ConfigSnapshot, ConfigWriteError, FOLLOW_DISPLAY_FRAME_RATE_NAME,
    FrameRate, LlmSettings, LoggingSettings, LunaConfig, ModelResourceSettings, PersonaSettings,
    SharedLlmSettings, SharedModelResourceSettings, SharedPersonaSettings, SharedVoiceSettings,
    ShortcutSettings, UNLIMITED_FRAME_RATE_NAME, VoiceSettings, ensure_table_like,
    persistence::log_config_update, remove_key, revision::revision_is_current, set_item_value,
    validate_relative_path, write_appearance, write_llm_settings, write_logging_settings,
    write_model_resource_settings, write_persona_settings, write_shortcut_settings,
    write_voice_settings,
};

impl LunaConfig {
    /// 仅提交仍然最新的帧率写入。
    pub(crate) fn set_frame_rate_at_revision(
        &self,
        frame_rate: FrameRate,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.frame_rate_request_revision,
            revision,
            "render.frame_rate",
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

    /// 仅提交仍然最新的帧率显示开关写入。
    pub(crate) fn set_show_fps_at_revision(
        &self,
        show: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.show_fps_request_revision,
            revision,
            "debug.show_fps",
            || self.show_fps.store(show, Ordering::Relaxed),
            |document| {
                ensure_table_like(&mut document["debug"]);
                set_item_value(&mut document["debug"]["show_fps"], Value::from(show));
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 仅提交仍然最新的原生托盘右键菜单开关写入。
    pub(crate) fn set_use_native_tray_menu_at_revision(
        &self,
        enabled: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.use_native_tray_menu_request_revision,
            revision,
            "debug.use_native_tray_menu",
            || self.use_native_tray_menu.store(enabled, Ordering::Relaxed),
            |document| {
                ensure_table_like(&mut document["debug"]);
                set_item_value(
                    &mut document["debug"]["use_native_tray_menu"],
                    Value::from(enabled),
                );
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 仅提交仍然最新的 Agent 换装工具开关写入。
    pub(crate) fn set_allow_agent_outfit_change_at_revision(
        &self,
        allowed: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let applied = self.edit_config_at_revision(
            &self.allow_agent_outfit_change_request_revision,
            revision,
            "tools.allow_agent_outfit_change",
            || {
                self.allow_agent_outfit_change
                    .store(allowed, Ordering::Relaxed);
            },
            |document| {
                ensure_table_like(&mut document["tools"]);
                set_item_value(
                    &mut document["tools"]["allow_agent_outfit_change"],
                    Value::from(allowed),
                );
            },
        )?;
        Ok(applied.then_some(()))
    }

    /// 仅提交仍然最新的 Agent 截屏授权；磁盘成功前不会开放权限。
    pub(crate) fn set_allow_agent_screenshot_at_revision(
        &self,
        allowed: bool,
        revision: u64,
    ) -> Result<Option<()>, ConfigWriteError> {
        let result = (|| {
            let _guard = self.write_lock.lock();
            if !self.screenshot_request_is_current(revision, allowed) {
                return Ok(None);
            }
            let prepared = match self.prepare_document_locked(|document| {
                ensure_table_like(&mut document["tools"]);
                set_item_value(
                    &mut document["tools"]["allow_agent_screenshot"],
                    Value::from(allowed),
                );
            }) {
                Ok(prepared) => prepared,
                Err(error) => {
                    let _revision_guard = self.revision_lock.lock();
                    if self.screenshot_request_is_current(revision, allowed) {
                        self.fail_screenshot_persistence_locked(allowed);
                    }
                    return Err(error);
                }
            };

            #[cfg(test)]
            self.pause_after_prepare_for_test();
            let visible = {
                let _revision_guard = self.revision_lock.lock();
                if !self.screenshot_request_is_current(revision, allowed) {
                    return Ok(None);
                }
                // 授权的最终复核、rename 可见点与权限发布保持同一临界区。
                let visible = match replace_config_file(prepared) {
                    Ok(visible) => visible,
                    Err(error) => {
                        self.fail_screenshot_persistence_locked(allowed);
                        return Err(error);
                    }
                };
                self.allow_agent_screenshot
                    .store(allowed, Ordering::Release);
                self.applied_allow_agent_screenshot_revision
                    .store(revision, Ordering::Release);
                self.agent_screenshot_permission_retry_required
                    .store(false, Ordering::Release);
                visible
            };
            sync_config_file_parent(visible)?;
            Ok(Some(()))
        })();
        log_config_update(
            "tools.allow_agent_screenshot",
            revision,
            result.as_ref().map(|applied| applied.is_some()),
        );
        result
    }

    fn screenshot_request_is_current(&self, revision: u64, allowed: bool) -> bool {
        revision_is_current(&self.allow_agent_screenshot_request_revision, revision)
            && self
                .requested_allow_agent_screenshot
                .load(Ordering::Acquire)
                == allowed
    }

    /// 调用方持有 revision 锁，保证失败回退不会覆盖更新的授权请求。
    fn fail_screenshot_persistence_locked(&self, attempted_allowed: bool) {
        // 持久化结果不确定时不从旧磁盘值重新开放隐私权限。
        self.allow_agent_screenshot.store(false, Ordering::Release);
        self.requested_allow_agent_screenshot
            .store(false, Ordering::Release);
        self.agent_screenshot_permission_retry_required
            .store(!attempted_allowed, Ordering::Release);
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
            "logging",
            move || self.logging.store(published),
            move |document| write_logging_settings(document, &settings),
        )?;
        Ok(applied.then_some(()))
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
            "interaction.eye_tracking",
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

    /// 校验、持久化并一次性发布最新的外观配置。
    pub(crate) fn set_appearance_at_revision(
        &self,
        settings: AppearanceSettings,
        revision: u64,
    ) -> Result<Option<Arc<AppearanceSettings>>, ConfigWriteError> {
        let result = (|| {
            let settings = Arc::new(
                settings
                    .normalized()
                    .map_err(ConfigWriteError::InvalidValue)?,
            );
            let _guard = self.write_lock.lock();
            if !revision_is_current(&self.appearance_request_revision, revision) {
                return Ok(None);
            }
            let prepared =
                self.prepare_document_locked(|document| write_appearance(document, &settings))?;
            self.commit_prepared_config_at_revision(
                &self.appearance_request_revision,
                revision,
                prepared,
                || {
                    let language_changed = self.appearance.load().language != settings.language;
                    self.appearance.store(settings.clone());
                    if language_changed {
                        self.publish_agent_config(
                            self.llm.load_full(),
                            self.persona.load_full(),
                            settings.language,
                        );
                    }
                    settings
                },
            )
        })();
        log_config_update(
            "appearance",
            revision,
            result.as_ref().map(|applied| applied.is_some()),
        );
        result
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
            "model.selected",
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

    /// 原子持久化并发布仍是最新请求的模型资源覆盖。
    pub(crate) fn set_model_resource_settings_at_revision(
        &self,
        settings: ModelResourceSettings,
        revision: u64,
    ) -> Result<Option<SharedModelResourceSettings>, ConfigWriteError> {
        let settings = Arc::new(settings);
        let published = settings.clone();
        let persisted = settings.clone();
        let applied = self.edit_config_at_revision(
            &self.model_resources_request_revision,
            revision,
            "model.resources",
            move || self.model_resources.store(published),
            move |document| write_model_resource_settings(document, &persisted),
        )?;
        Ok(applied.then_some(settings))
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
        validation_language: AppLanguage,
    ) -> Result<Option<SharedLlmSettings>, ConfigWriteError> {
        let result = (|| {
            let settings = Arc::new(settings.normalized(validation_language)?);
            let _guard = self.write_lock.lock();
            if self.llm_request_revision.load(Ordering::Relaxed) != revision {
                return Ok(None);
            }
            let generation = self.agent_config.load().generation().wrapping_add(1).max(1);
            let candidate = AgentConfigSnapshot::try_new(
                generation,
                settings,
                self.persona.load_full(),
                self.appearance.load().language,
            )
            .map_err(ConfigWriteError::from)?;
            let settings = Arc::clone(candidate.settings());
            let prepared =
                self.prepare_document_locked(|document| write_llm_settings(document, &settings))?;
            self.commit_prepared_config_at_revision(
                &self.llm_request_revision,
                revision,
                prepared,
                || {
                    self.llm.store(settings.clone());
                    self.agent_config.store(Arc::new(candidate));
                    settings
                },
            )
        })();
        log_config_update(
            "agent.providers",
            revision,
            result.as_ref().map(|applied| applied.is_some()),
        );
        result
    }

    /// 仅当该草稿仍是最新请求时才写入并发布；旧后台任务会被无害丢弃。
    ///
    /// # Errors
    ///
    /// 人格字段不合法，或配置文件无法持久化时返回错误。
    pub(crate) fn set_persona_settings_at_revision(
        &self,
        settings: PersonaSettings,
        revision: u64,
        validation_language: AppLanguage,
    ) -> Result<Option<SharedPersonaSettings>, ConfigWriteError> {
        let result = (|| {
            let settings = Arc::new(settings.normalized(validation_language)?);
            let _guard = self.write_lock.lock();
            if self.persona_request_revision.load(Ordering::Relaxed) != revision {
                return Ok(None);
            }
            let generation = self.agent_config.load().generation().wrapping_add(1).max(1);
            let candidate = AgentConfigSnapshot::try_new(
                generation,
                self.llm.load_full(),
                settings,
                self.appearance.load().language,
            )
            .map_err(ConfigWriteError::from)?;
            let settings = Arc::clone(candidate.personas());
            let prepared = self
                .prepare_document_locked(|document| write_persona_settings(document, &settings))?;
            self.commit_prepared_config_at_revision(
                &self.persona_request_revision,
                revision,
                prepared,
                || {
                    self.persona.store(settings.clone());
                    self.agent_config.store(Arc::new(candidate));
                    settings
                },
            )
        })();
        log_config_update(
            "agent.personas",
            revision,
            result.as_ref().map(|applied| applied.is_some()),
        );
        result
    }

    /// 校验、持久化并一次性发布仍为最新请求的快捷键配置。
    pub(crate) fn set_shortcut_settings_at_revision(
        &self,
        settings: ShortcutSettings,
        revision: u64,
    ) -> Result<Option<Arc<ShortcutSettings>>, ConfigWriteError> {
        let result = (|| {
            let settings = Arc::new(settings.normalized()?);
            let _guard = self.write_lock.lock();
            if !revision_is_current(&self.shortcut_request_revision, revision) {
                return Ok(None);
            }
            let prepared = self
                .prepare_document_locked(|document| write_shortcut_settings(document, &settings))?;
            self.commit_prepared_config_at_revision(
                &self.shortcut_request_revision,
                revision,
                prepared,
                || {
                    self.shortcuts.store(settings.clone());
                    settings
                },
            )
        })();
        log_config_update(
            "shortcuts",
            revision,
            result.as_ref().map(|applied| applied.is_some()),
        );
        result
    }

    /// 校验、持久化并一次性发布仍是最新请求的语音配置。
    pub(crate) fn set_voice_settings_at_revision(
        &self,
        settings: VoiceSettings,
        revision: u64,
    ) -> Result<Option<SharedVoiceSettings>, ConfigWriteError> {
        let result = (|| {
            let settings = Arc::new(settings.normalized()?);
            let _guard = self.write_lock.lock();
            if !revision_is_current(&self.voice_request_revision, revision) {
                return Ok(None);
            }
            let prepared =
                self.prepare_document_locked(|document| write_voice_settings(document, &settings))?;
            self.commit_prepared_config_at_revision(
                &self.voice_request_revision,
                revision,
                prepared,
                || {
                    self.voice.store(settings.clone());
                    settings
                },
            )
        })();
        log_config_update(
            "voice",
            revision,
            result.as_ref().map(|applied| applied.is_some()),
        );
        result
    }

    /// 调用方已持有 revision 锁，所有配置域先规范化再作为一个不可变快照发布。
    fn publish_agent_config(
        &self,
        settings: SharedLlmSettings,
        personas: SharedPersonaSettings,
        language: AppLanguage,
    ) {
        let generation = self.agent_config.load().generation().wrapping_add(1).max(1);
        let snapshot = AgentConfigSnapshot::try_new(generation, settings, personas, language)
            .expect("根配置层只能发布已经通过领域校验的 Agent 配置");
        self.agent_config.store(Arc::new(snapshot));
    }
}
