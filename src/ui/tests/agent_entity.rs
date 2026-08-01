//! 在无头 GPUI 中验证 AgentView 只桥接核心快照、输入和本地效果。

use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use lunamate_agent::{
    Agent, AgentMemory, ChatLimits, Client,
    config::{AppLanguage, DEFAULT_PERSONA_ID},
    tools::{AgentOutfitRequest, OutfitOption},
};

use crate::ui::agent::AgentView;

const INITIAL_GENERATION: u64 = 1;

fn agent(status: Option<String>) -> Arc<Agent> {
    Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        DEFAULT_PERSONA_ID,
        ChatLimits::default(),
        AppLanguage::SimplifiedChinese,
        status,
    )
}

fn mount(
    cx: &mut TestAppContext,
    status: Option<String>,
) -> (Entity<AgentView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let agent = agent(status);
    cx.add_window_view(move |window, cx| AgentView::new(agent, INITIAL_GENERATION, window, cx))
}

#[gpui::test]
fn persistence_warning_is_visible_until_fade_is_started(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, Some("数据库不可用".to_owned()));
    view.update(cx, |view, cx| {
        view.start_initial_reply_fade(cx);
        assert!(view.reply_visible());
        assert_eq!(view.reply_text_for_test().as_deref(), Some("数据库不可用"));
        assert_eq!(view.message_count_for_test(), 0);
    });
}

#[gpui::test]
fn view_without_status_starts_with_hidden_reply(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, None);
    view.update(cx, |view, _cx| {
        assert!(!view.reply_visible());
        assert_eq!(view.reply_text_for_test(), None);
        assert!(!view.is_streaming_for_test());
        assert_eq!(view.active_persona_for_test(), DEFAULT_PERSONA_ID);
    });
}

#[gpui::test]
fn outfit_requests_are_checked_against_current_revision(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, None);
    view.update(cx, |view, _cx| {
        view.set_available_outfits(vec![
            OutfitOption::new("default", "默认"),
            OutfitOption::new("detective", "侦探"),
        ]);
        let (current, _result) = AgentOutfitRequest::channel("detective".to_owned(), 1);
        let (stale, _result) = AgentOutfitRequest::channel("detective".to_owned(), 0);
        assert!(view.outfit_request_is_current(&current));
        assert!(!view.outfit_request_is_current(&stale));
    });
}

#[gpui::test]
fn cancelled_voice_utterance_does_not_clear_a_newer_one(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, None);
    view.update(cx, |view, cx| {
        view.voice_speech_started(7, AppLanguage::SimplifiedChinese, cx);
        view.voice_speech_started(8, AppLanguage::SimplifiedChinese, cx);
        view.voice_utterance_cancelled(7);
        assert_eq!(view.pending_voice_for_test(), Some(8));
        view.voice_utterance_cancelled(8);
        assert_eq!(view.pending_voice_for_test(), None);
    });
}

#[gpui::test]
fn stopping_voice_interaction_clears_the_pending_utterance(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, None);
    view.update(cx, |view, cx| {
        view.voice_speech_started(9, AppLanguage::SimplifiedChinese, cx);
        assert_eq!(view.pending_voice_for_test(), Some(9));

        view.stop_voice_interaction(cx);

        assert_eq!(view.pending_voice_for_test(), None);
    });
}
