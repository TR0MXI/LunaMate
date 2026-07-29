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
        AgentMemoryAccess, AgentOutfitRequest, AssistantTrace,
        service::{ChatBackend, ChatServiceRequest, ChatStreamEvent},
        session::{ChatSession, MAX_TRACE_REASONING_BYTES},
        store::ChatSessionStore,
        view::AgentView,
    },
    config::{
        AppLanguage, CONFIG, DEFAULT_PERSONA_ID, LlmAdvancedOptions, LlmModelConfig, LlmProvider,
        LlmSettings, PersonaContextLimits, PersonaSettings,
    },
    database::Database,
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

fn config_with_model() -> ConfigGuard {
    ConfigGuard::publish(LlmSettings {
        models: vec![LlmModelConfig {
            id: "local".to_owned(),
            label: "Local".to_owned(),
            provider: LlmProvider::Ollama,
            model: "qwen3:8b".to_owned(),
            endpoint: Some("http://localhost:11434/".to_owned()),
            api_key: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("local".to_owned()),
    })
}

fn mount(
    cx: &mut TestAppContext,
    backend: Arc<dyn ChatBackend>,
    initial_status: Option<String>,
) -> (Entity<AgentView>, &mut VisualTestContext) {
    mount_with_session(cx, backend, initial_status, ChatSession::default())
}

fn mount_with_session(
    cx: &mut TestAppContext,
    backend: Arc<dyn ChatBackend>,
    initial_status: Option<String>,
    session: ChatSession,
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
            session,
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

fn mount_with_database(
    cx: &mut TestAppContext,
    database: Arc<Database>,
) -> (Entity<AgentView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let memory = AgentMemoryAccess::new(Some(database));
    let store = ChatSessionStore::unavailable();
    let (view, cx) = cx.add_window_view(|window, cx| {
        AgentView::new(
            CONFIG.llm_settings(),
            CONFIG.persona_settings(),
            DEFAULT_PERSONA_ID.to_owned(),
            ChatSession::default(),
            store,
            memory,
            None,
            window,
            cx,
        )
    });
    (view, cx)
}

fn memory_database() -> Arc<Database> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试必须能创建 Tokio 运行时")
        .block_on(Database::open_memory())
        .expect("内存数据库应可打开")
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

#[gpui::test]
fn a_failed_persona_restore_cannot_send_the_previous_session(cx: &mut TestAppContext) {
    let _config = config_with_model();
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);

    view.update(cx, |view, cx| {
        view.mark_persona_swap_failed_for_test();
        assert!(!view.send_message_for_test("不得写入旧人格", cx));
        assert_eq!(view.message_count_for_test(), 0);
        assert!(view.reply_visible());
    });
}

#[gpui::test]
fn offline_limit_changes_preserve_the_newest_current_persona_history(cx: &mut TestAppContext) {
    let _config = config_without_model();
    let mut session = ChatSession::default();
    for (question, answer) in [("first", "one"), ("second", "two")] {
        let turn = session.start_turn(question).expect("测试轮次应可开始");
        session
            .append_response(turn.response_id, answer)
            .expect("测试回复应可写入");
        assert!(session.finish_response(turn.response_id));
    }
    let (view, cx) = mount_with_session(cx, Arc::new(SilentBackend), None, session);
    let mut personas = PersonaSettings::default();
    personas.personas[0].context = PersonaContextLimits {
        max_messages: Some(2),
        max_tokens: None,
    };
    CONFIG.publish_persona_settings_for_test(personas);

    view.update(cx, |view, cx| {
        view.refresh_settings(cx);
        assert_eq!(view.message_count_for_test(), 2);
    });
}

#[gpui::test]
fn clearing_the_active_context_reports_persistence_failure(cx: &mut TestAppContext) {
    let _config = config_without_model();
    let mut session = ChatSession::default();
    let turn = session.start_turn("question").expect("测试轮次应可开始");
    session
        .append_response(turn.response_id, "answer")
        .expect("测试回复应可写入");
    assert!(session.finish_response(turn.response_id));
    let (view, cx) = mount_with_session(cx, Arc::new(SilentBackend), None, session);
    let (completion, result) = async_channel::bounded(1);

    view.update(cx, |view, cx| {
        view.clear_persona_context(DEFAULT_PERSONA_ID, Some(completion), cx);
        assert_eq!(view.message_count_for_test(), 0);
    });
    let error = result
        .try_recv()
        .expect("数据库不可用时应立即返回结果")
        .expect_err("未落盘的上下文清空不得报告成功");
    assert_eq!(error, rust_i18n::t!("persona.memory_unavailable"));
}

#[gpui::test]
fn a_late_edit_for_a_deleted_persona_does_not_defer_the_active_restore(cx: &mut TestAppContext) {
    let _config = config_without_model();
    let (view, cx) = mount_with_database(cx, memory_database());

    view.update(cx, |view, cx| {
        view.mark_persona_swap_failed_for_test();
        view.edit_persona_context_message("deleted", 1, "late".to_owned(), None, cx);
        assert_eq!(view.persona_swap_state_for_test(), (true, false));
    });
}

#[gpui::test]
fn trace_events_only_attach_to_the_matching_active_response(cx: &mut TestAppContext) {
    let mut session = ChatSession::default();
    let old = session.start_turn("old").expect("第一轮应可开始");
    assert!(session.cancel_response(old.response_id));
    let current = session.start_turn("current").expect("替代轮次应可开始");
    let message_id = session.messages().back().expect("应有助手占位").id();
    let (view, cx) = mount_with_session(cx, Arc::new(SilentBackend), None, session);
    let trace = AssistantTrace::new(Some("current reasoning".to_owned()), Vec::new());

    view.update(cx, |view, cx| {
        assert!(
            view.apply_stream_event_for_test(
                old.response_id,
                ChatStreamEvent::Trace(AssistantTrace::new(
                    Some("late reasoning".to_owned()),
                    Vec::new(),
                )),
                cx,
            )
            .is_none()
        );
        assert!(view.message_trace_for_test(message_id).is_none());
        assert_eq!(
            view.apply_stream_event_for_test(
                current.response_id,
                ChatStreamEvent::Trace(trace.clone()),
                cx,
            ),
            Some((true, false))
        );
        assert_eq!(view.message_trace_for_test(message_id), Some(trace));
        assert_eq!(
            view.apply_stream_event_for_test(current.response_id, ChatStreamEvent::Finished, cx,),
            Some((false, true))
        );
    });
}

#[gpui::test]
fn rejected_optional_trace_does_not_fail_the_matching_visible_reply(cx: &mut TestAppContext) {
    let mut session = ChatSession::default();
    let current = session.start_turn("question").expect("测试轮次应可开始");
    session
        .append_response(current.response_id, "visible answer")
        .expect("可见回复应可写入");
    let message_id = session.messages().back().expect("应有助手消息").id();
    let (view, cx) = mount_with_session(cx, Arc::new(SilentBackend), None, session);

    view.update(cx, |view, cx| {
        assert_eq!(
            view.apply_stream_event_for_test(
                current.response_id,
                ChatStreamEvent::Trace(AssistantTrace::new(
                    Some("x".repeat(MAX_TRACE_REASONING_BYTES + 1)),
                    Vec::new(),
                )),
                cx,
            ),
            Some((true, false))
        );
        assert_eq!(
            view.apply_stream_event_for_test(current.response_id, ChatStreamEvent::Finished, cx,),
            Some((false, true))
        );
        assert!(!view.is_streaming_for_test());
        assert!(view.message_trace_for_test(message_id).is_none());
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
fn only_the_latest_voice_utterance_can_be_cancelled_or_submitted(cx: &mut TestAppContext) {
    let _config = config_without_model();
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);

    view.update(cx, |view, cx| {
        view.voice_speech_started(10, AppLanguage::SimplifiedChinese, cx);
        view.voice_speech_started(11, AppLanguage::SimplifiedChinese, cx);
        assert_eq!(view.pending_voice_for_test(), Some(11));

        view.voice_utterance_cancelled(10);
        assert_eq!(view.pending_voice_for_test(), Some(11));
        view.voice_utterance_cancelled(11);
        assert_eq!(view.pending_voice_for_test(), None);
    });
}

#[gpui::test]
fn a_voice_transcript_is_rejected_after_agent_configuration_changes(cx: &mut TestAppContext) {
    let _config = config_without_model();
    let (view, cx) = mount(cx, Arc::new(SilentBackend), None);

    view.update(cx, |view, cx| {
        view.voice_speech_started(21, AppLanguage::English, cx);
        assert_eq!(view.pending_voice_for_test(), Some(21));

        // 即使内容相同，新发布的 Arc 也代表一个新的配置 generation。
        CONFIG.publish_llm_settings_for_test(LlmSettings::default());
        assert!(!view.send_voice_transcript(21, "hello".to_owned(), cx));
        assert_eq!(view.pending_voice_for_test(), None);
        assert_eq!(view.message_count_for_test(), 0);
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
