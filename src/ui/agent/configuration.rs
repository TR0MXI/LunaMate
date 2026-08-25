//! 应用 Agent 配置快照，并以 generation 和 revision 隔离重试与迟到结果。

use gpui::Context;
use gpui_tokio::Tokio;
use lunamate_agent::{Client, chat_limits, client_from_model, model_and_options_from_config};
use rust_i18n::t;

use super::AgentView;
use crate::config::AgentConfigSnapshot;

impl AgentView {
    #[cfg(test)]
    pub(in crate::ui) const fn agent_config_generation_for_test(&self) -> u64 {
        self.agent_config_generation
    }

    #[cfg(test)]
    pub(in crate::ui) fn begin_agent_config_refresh_for_test(
        &mut self,
        generation: u64,
    ) -> Option<u64> {
        self.begin_agent_config_refresh(generation)
    }

    #[cfg(test)]
    pub(in crate::ui) fn finish_agent_config_refresh_for_test(
        &mut self,
        revision: u64,
        generation: u64,
        applied: bool,
    ) -> bool {
        self.finish_agent_config_refresh(revision, generation, applied)
    }

    fn begin_agent_config_refresh(&mut self, generation: u64) -> Option<u64> {
        if generation <= self.agent_config_generation
            || self
                .agent_config_pending_generation
                .is_some_and(|pending| generation <= pending)
        {
            return None;
        }
        self.agent_config_pending_generation = Some(generation);
        self.refresh_revision = self.refresh_revision.wrapping_add(1).max(1);
        self.refresh_task = None;
        Some(self.refresh_revision)
    }

    fn finish_agent_config_refresh(
        &mut self,
        revision: u64,
        generation: u64,
        applied: bool,
    ) -> bool {
        if self.refresh_revision != revision
            || self.agent_config_pending_generation != Some(generation)
        {
            return false;
        }
        self.refresh_task = None;
        self.agent_config_pending_generation = None;
        if applied {
            self.agent_config_generation = generation;
        }
        true
    }

    /// 把宿主配置快照解析为直接 Agent 组件；核心本身不依赖该快照类型。
    pub fn refresh_settings(&mut self, snapshot: AgentConfigSnapshot, cx: &mut Context<Self>) {
        let generation = snapshot.generation();
        let Some(persona) = snapshot.personas().active().cloned() else {
            return;
        };
        let Some(revision) = self.begin_agent_config_refresh(generation) else {
            return;
        };
        self.agent.cancel_pending_voice();
        self.cancel_speech();
        let model = persona
            .model
            .as_deref()
            .and_then(|id| snapshot.settings().model(id))
            .or_else(|| snapshot.settings().selected())
            .cloned();
        let limits = chat_limits(&persona, snapshot.settings());
        let language = snapshot.language();
        let system_prompt = persona.system_prompt;
        let persona_id = persona.id;
        let agent = self.agent.clone();
        let memory = agent.memory();
        let build = Tokio::spawn(cx, async move {
            let client_model = model.clone();
            let client = tokio::task::spawn_blocking(move || {
                client_model
                    .as_ref()
                    .map_or_else(Client::default, client_from_model)
            })
            .await
            .map_err(|error| error.to_string())?;
            let (model_iden, options) = model
                .as_ref()
                .and_then(model_and_options_from_config)
                .map_or((None, None), |(model, options)| (Some(model), options));
            agent
                .apply_configuration(
                    generation,
                    client,
                    model_iden,
                    options,
                    system_prompt,
                    memory,
                    persona_id,
                    limits,
                    language,
                )
                .await
                .map_err(|error| error.to_string())
        });
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let result = build.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                let applied = result.as_ref().is_ok_and(|applied| *applied);
                if !this.finish_agent_config_refresh(revision, generation, applied) {
                    return;
                }
                if let Err(error) = result {
                    this.agent.set_status(Some(
                        t!("chat.persistence_unavailable", error = error).to_string(),
                    ));
                }
                cx.notify();
            });
        }));
    }
}
