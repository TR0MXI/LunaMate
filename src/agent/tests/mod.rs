//! 集中索引 Agent 领域测试，并共享全局配置的独占夹具。

mod media;
mod service;
mod session;
mod settings;
mod settings_entity;
mod settings_persona;
mod store;
mod view;
mod view_entity;

use std::sync::{Mutex, MutexGuard};

use crate::config::{CONFIG, LlmSettings, PersonaSettings};

/// `CONFIG` 是进程级全局状态。锁必须由整个 `agent` 测试树共用：每个测试文件各自持有
/// 一把锁只能隔离同文件内的用例，跨文件的并行测试仍会互相覆盖已发布的快照。
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// 在作用域内独占并恢复全局供应商与人格配置。
pub(super) struct ConfigGuard {
    _guard: MutexGuard<'static, ()>,
    previous_llm: LlmSettings,
    previous_persona: PersonaSettings,
}

impl ConfigGuard {
    /// 发布一份供应商配置，人格固定为默认人格。
    pub(super) fn publish(llm: LlmSettings) -> Self {
        Self::publish_all(llm, PersonaSettings::default())
    }

    /// 同时发布供应商与人格配置。
    pub(super) fn publish_all(llm: LlmSettings, persona: PersonaSettings) -> Self {
        // 中毒锁只说明某个用例断言失败，后续用例仍需按顺序独占配置。
        let guard = CONFIG_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous_llm = CONFIG.llm_settings().as_ref().clone();
        let previous_persona = CONFIG.persona_settings().as_ref().clone();
        CONFIG.publish_llm_settings_for_test(llm);
        CONFIG.publish_persona_settings_for_test(persona);
        Self {
            _guard: guard,
            previous_llm,
            previous_persona,
        }
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        CONFIG.publish_llm_settings_for_test(self.previous_llm.clone());
        CONFIG.publish_persona_settings_for_test(self.previous_persona.clone());
    }
}
