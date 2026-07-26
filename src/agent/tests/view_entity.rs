//! 在无头 GPUI TestAppContext 中验证 Agent 视图实体的状态流转。
//!
//! 网络层统一替换为 fake backend，测试只覆盖实体状态、会话写入与取消路径；
//! 真实 Provider 请求与窗口呈现不在此处验证。

use std::{pin::Pin, sync::Arc, time::Duration};

use futures::{channel::mpsc, future::Future};
use gpui::{Entity, TestAppContext, VisualTestContext, prelude::*};

use super::ConfigGuard;
use crate::{
    agent::{
        AgentMemoryAccess, AgentOutfitRequest,
        service::{ChatBackend, ChatServiceRequest, ChatStreamEvent},
        session::ChatSession,
        store::ChatSessionStore,
        view::AgentView,
    },
    config::{CONFIG, DEFAULT_PERSONA_ID, LlmSettings},
};

/// 永不产出事件的 fake backend；保证测试不会发起任何网络请求。
///
/// 流式事件本身不在此处驱动：`AgentView` 通过 `gpui_tokio` 把请求交给真实 Tokio
/// runtime，其唤醒来自测试线程之外，会被 GPUI 测试调度器判定为非确定性。
struct SilentBackend;

impl ChatBackend for SilentBackend {
    fn stream(
        &self,
        _request: ChatServiceRequest,
        _events: mpsc::Sender<ChatStreamEvent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(std::future::pending())
    }
}

/// 未配置任何供应商时的固定夹具。
fn config_without_model() -> ConfigGuard {
    ConfigGuard::publish(LlmSettings::default())
}

fn mount(
    cx: &mut TestAppContext,
    backend: Arc<dyn ChatBackend>,
    initial_status: Option<String>,
) -> (Entity<AgentView>, &mut VisualTestContext) {
    // 与生产启动路径一致：GPUI Component 与 Tokio runtime 必须在创建视图前完成初始化。
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = AgentView::new(
            CONFIG.llm_settings(),
            CONFIG.persona_settings(),
            DEFAULT_PERSONA_ID.to_owned(),
            ChatSession::default(),
            ChatSessionStore::unavailable(),
            AgentMemoryAccess::default(),
            initial_status,
            window,
            cx,
        );
        view.set_backend_for_test(backend);
        view
    });
    (view, cx)
}

#[gpui::test]
fn a_persistence_warning_is_visible_until_it_fades(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, Arc::new(SilentBackend), Some("数据库不可用".to_owned()));

    view.update(cx, |view, cx| {
        view.start_initial_reply_fade(cx);
        assert!(view.reply_visible());
        assert_eq!(view.reply_text_for_test().as_deref(), Some("数据库不可用"));
        assert_eq!(view.message_count_for_test(), 0);
    });
}

#[gpui::test]
fn a_view_without_an_initial_status_starts_with_a_hidden_reply(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);

    view.update(cx, |view, _cx| {
        assert!(!view.reply_visible());
        assert!(view.reply_text_for_test().is_none());
        assert!(!view.is_streaming_for_test());
    });
}

#[gpui::test]
fn submitting_without_a_configured_model_shows_a_status_instead_of_a_turn(cx: &mut TestAppContext) {
    let _config = config_without_model();
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);

    view.update(cx, |view, cx| {
        assert!(!view.send_message_for_test("你好", cx));
        assert_eq!(view.message_count_for_test(), 0);
        assert!(view.reply_visible());
        assert!(view.reply_text_for_test().is_some());
    });
}

// 以下流式生命周期不在此处验证：`AgentView` 通过 `gpui_tokio` 把 Provider 请求交给真实
// Tokio runtime，其唤醒来自测试线程之外，GPUI 测试调度器会判定为非确定性测试。
// 流式解析、超时、取消与终止事件已在 `agent::tests::service` 中以确定性方式覆盖。

#[gpui::test]
async fn shutdown_snapshots_persist_without_a_database(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);

    let shutdown = view.update(cx, |view, _cx| view.shutdown_snapshot());

    // 数据库不可用时启动阶段已提示过一次，退出写入应当安静跳过。
    assert_eq!(shutdown.persist().await, Ok(()));
}

#[gpui::test]
fn toggling_the_input_bar_updates_visibility(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);

    cx.update_window_entity(&view, |view, window, cx| {
        view.set_input_visible(true, window, cx);
        view.set_input_visible(false, window, cx);
        view.refresh_settings(cx);
    });
}

#[gpui::test]
fn replacing_the_outfit_snapshot_rejects_an_older_tool_request(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);
    let (request, _result) = AgentOutfitRequest::channel("侦探".to_owned(), 1);

    view.update(cx, |view, _cx| {
        view.set_available_outfits(vec!["默认服装".to_owned(), "侦探".to_owned()]);
        assert!(view.outfit_request_is_current(&request));

        view.set_available_outfits(vec!["默认服装".to_owned(), "女仆".to_owned()]);
        assert!(!view.outfit_request_is_current(&request));
    });
}

#[gpui::test]
fn a_reply_stays_visible_while_hovered(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx, Arc::new(SilentBackend), Some("提示".to_owned()));

    view.update(cx, |view, cx| {
        view.start_initial_reply_fade(cx);
        assert!(view.reply_visible());
    });

    // 悬停期间即使等待超过 linger 时长，回复也不应被淡出任务清除。
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
}
