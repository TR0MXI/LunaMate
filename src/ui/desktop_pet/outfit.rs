//! 向 Agent 发布当前服装能力，并应用经过 revision 校验的换装请求。

use gpui::Context;
use lunamate_agent::tools::AgentOutfitRequest;

use super::{DesktopPetView, ModelLoadState};
use crate::{config::CONFIG, model::ModelCommand, ui::AgentOutfitAction};

impl DesktopPetView {
    const fn model_load_command_was_issued(
        generation_before_load: u64,
        generation_after_load: u64,
    ) -> bool {
        generation_after_load != generation_before_load
    }

    #[cfg(test)]
    pub(in crate::ui) const fn model_load_command_was_issued_for_test(
        generation_before_load: u64,
        generation_after_load: u64,
    ) -> bool {
        Self::model_load_command_was_issued(generation_before_load, generation_after_load)
    }

    pub(super) fn clear_agent_outfits(&self, cx: &mut Context<Self>) {
        self.chat.update(cx, |chat, _| {
            chat.set_available_outfits(Vec::new());
        });
    }

    pub(super) fn sync_agent_outfits(&self, cx: &mut Context<Self>) {
        let outfits = if matches!(self.model_state, ModelLoadState::Ready { .. }) {
            self.config.read(cx).available_agent_outfits()
        } else {
            Vec::new()
        };
        self.chat.update(cx, |chat, _| {
            chat.set_available_outfits(outfits);
        });
    }

    pub(super) fn apply_agent_outfit_request(
        &mut self,
        request: &AgentOutfitRequest,
        cx: &mut Context<Self>,
    ) {
        if !self.desktop_pet_visible
            || !CONFIG.allow_agent_outfit_change()
            || !self.chat.read(cx).outfit_request_is_current(request)
        {
            request.complete(false);
            return;
        }
        let Some(action) = self
            .config
            .read(cx)
            .resolve_agent_outfit(request.outfit_id())
        else {
            request.complete(false);
            return;
        };
        let runtime_action_accepted = match action {
            AgentOutfitAction::Unchanged => true,
            action @ AgentOutfitAction::LoadVariant(_) => {
                let generation_before_load = self.model_generation;
                match self
                    .config
                    .update(cx, |config, cx| config.commit_agent_outfit(action, cx))
                {
                    Ok(Some(model_path)) => {
                        self.load_model(Some(model_path), cx);
                        Self::model_load_command_was_issued(
                            generation_before_load,
                            self.model_generation,
                        )
                    }
                    Ok(None) | Err(_) => false,
                }
            }
            AgentOutfitAction::PreviewExpression(name) => {
                let sent = self.model_commands.as_ref().is_some_and(|sender| {
                    sender
                        .try_send(ModelCommand::PreviewExpression(name.clone()))
                        .is_ok()
                });
                if sent {
                    self.wake_model();
                    self.config
                        .update(cx, |config, cx| {
                            config
                                .commit_agent_outfit(AgentOutfitAction::PreviewExpression(name), cx)
                        })
                        .is_ok()
                } else {
                    false
                }
            }
            AgentOutfitAction::ResetExpression => {
                let sent = self
                    .model_commands
                    .as_ref()
                    .is_some_and(|sender| sender.try_send(ModelCommand::ResetExpression).is_ok());
                if sent {
                    self.wake_model();
                    self.config
                        .update(cx, |config, cx| {
                            config.commit_agent_outfit(AgentOutfitAction::ResetExpression, cx)
                        })
                        .is_ok()
                } else {
                    false
                }
            }
        };
        // 工具结果只确认即时运行时动作已受理；异步写盘失败会通过模型事件回滚。
        request.complete(runtime_action_accepted);
    }
}
