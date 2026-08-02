use crate::{
    Agent, AgentMemory, Client, chat_limits,
    config::{AppLanguage, LlmSettings, PersonaConfig, PersonaContextLimits},
    media::prepare_dynamic_image,
    memory::AssistantTrace,
    session::*,
};

use super::{LANGUAGE, reasoning_trace};

#[test]
fn unavailable_persistence_keeps_the_active_personas_context_limits() {
    let mut persona = PersonaConfig::new("active", "Active");
    persona.context = PersonaContextLimits {
        max_messages: Some(2),
        max_tokens: Some(256),
    };
    let limits = chat_limits(&persona, &LlmSettings::default());
    let agent = Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        "active",
        limits,
        AppLanguage::Japanese,
        Some("offline".to_owned()),
    );
    let usage = agent
        .memory()
        .live_context_usage()
        .get("active")
        .expect("活动人格应发布实时上下文用量");

    assert_eq!(agent.snapshot().language(), AppLanguage::Japanese);
    assert_eq!(agent.snapshot().active_persona(), "active");
    assert_eq!(usage.max_messages, 2);
    assert_eq!(usage.max_tokens, 256);
    assert!(!agent.memory().is_available());
    assert_eq!(agent.snapshot().status(), Some("offline"));
}

#[test]
fn context_excludes_streaming_placeholder() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("用户消息应当可发送");

    assert_eq!(
        started.context,
        vec![ChatContextMessage {
            source_message_id: Some(session.messages()[0].id()),
            role: ChatRole::User,
            content: "hello".to_owned(),
            image: None,
        }]
    );
    assert_eq!(session.messages().len(), 2);
}

#[test]
fn image_only_prompt_uses_the_turn_language() {
    let image = prepare_dynamic_image(image::DynamicImage::new_rgb8(2, 2), "sample.jpg".to_owned())
        .expect("测试图片应当可以规范化");
    let cases = [
        (AppLanguage::SimplifiedChinese, "请查看这张图片。"),
        (AppLanguage::TraditionalChinese, "請查看這張圖片。"),
        (AppLanguage::English, "Please inspect this image."),
        (AppLanguage::Japanese, "この画像を確認してください。"),
    ];

    for (language, expected) in cases {
        let mut session = ChatSession::default();
        let started = session
            .start_turn_with_image("", Some(image.clone()), language)
            .expect("纯图片消息应当可以发送");
        assert_eq!(started.context[0].content, expected);
    }
}

#[test]
fn stale_cancel_cannot_cancel_replacement_request() {
    let mut session = ChatSession::default();
    let old = session.start_turn("old").expect("第一轮应当可开始");
    assert!(session.cancel_response(old.response_id));
    let current = session.start_turn("new").expect("取消后应当可开始新一轮");

    assert!(!session.cancel_response(old.response_id));
    session
        .append_response(current.response_id, "answer")
        .expect("当前请求应当保持有效");
}

#[test]
fn trace_only_attaches_to_the_matching_active_response() {
    let mut session = ChatSession::default();
    let old = session.start_turn("old").expect("第一轮应当可开始");
    assert!(session.cancel_response(old.response_id));
    let current = session.start_turn("new").expect("替代轮次应当可开始");

    assert!(matches!(
        session.attach_response_trace(old.response_id, reasoning_trace("late")),
        Err(ChatError::StaleResponse)
    ));
    assert!(session.messages()[1].trace().is_none());
    assert!(
        session
            .attach_response_trace(current.response_id, reasoning_trace("current"))
            .expect("当前响应应可附加详情")
    );
    assert_eq!(
        session.messages()[3]
            .trace()
            .and_then(AssistantTrace::reasoning),
        Some("current")
    );
}

#[test]
fn oversized_optional_trace_does_not_prevent_visible_reply_completion() {
    let mut session = ChatSession::default();
    let started = session.start_turn("question").expect("测试轮次应可开始");
    session
        .append_response(started.response_id, "visible answer")
        .expect("可见回复应可写入");

    assert!(matches!(
        session.attach_response_trace(
            started.response_id,
            reasoning_trace("x".repeat(MAX_TRACE_REASONING_BYTES + 1)),
        ),
        Err(ChatError::MessageTooLarge)
    ));
    assert!(session.finish_response(started.response_id));
    assert_eq!(session.messages()[1].content(), "visible answer");
    assert_eq!(session.messages()[1].state(), &ChatMessageState::Complete);
    assert!(session.messages()[1].trace().is_none());
}

#[test]
fn failed_turn_is_not_replayed_in_next_context() {
    let mut session = ChatSession::default();
    let failed = session.start_turn("failed").expect("失败轮次应当可开始");
    assert!(session.fail_response(failed.response_id, "offline".to_owned()));
    let next = session.start_turn("next").expect("失败后应当可继续");

    assert_eq!(next.context.len(), 1);
    assert_eq!(next.context[0].content, "next");
}

#[test]
fn voice_interruption_is_marked_and_replayed_in_next_context() {
    let mut session = ChatSession::default();
    let interrupted = session.start_turn("first").expect("第一轮应当可开始");
    session
        .append_response(interrupted.response_id, "partial response")
        .expect("部分回复应当可写入");

    assert!(session.interrupt_response_by_voice(interrupted.response_id));
    let assistant = &session.messages()[1];
    assert_eq!(assistant.visible_content(), "partial response");
    assert_eq!(assistant.content(), "partial response");
    assert_eq!(assistant.state(), &ChatMessageState::InterruptedByVoice);
    let marker = voice_interruption_marker(LANGUAGE);
    let encoded = serde_json::to_string(&session.snapshot(1)).expect("快照应可编码");
    assert!(!encoded.contains(&marker), "本地化协议文本不得进入会话快照");

    let next = session
        .start_turn_with_image("second", None, LANGUAGE)
        .expect("打断后应当可继续");
    assert_eq!(next.context.len(), 3);
    assert_eq!(next.context[0].content, "first");
    assert!(next.context[1].content.ends_with(&marker));
    assert_eq!(next.context[2].content, "second");
}

#[test]
fn voice_interruption_preserves_message_content_and_utf8_boundaries() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 4,
        max_tokens: context_message_tokens("问题", 4) + context_message_tokens("回答回答", 4),
        max_request_tokens: usize::MAX,
    })
    .expect("测试限制应当有效");
    let interrupted = session.start_turn("问题").expect("第一轮应当可开始");
    session
        .append_response(interrupted.response_id, "回答回答")
        .expect("部分回复应当可写入");

    assert!(session.interrupt_response_by_voice(interrupted.response_id));
    let content = session.messages()[1].content();
    assert!(content.is_char_boundary(content.len()));
    assert_eq!(content, "回答回答");
    assert!(!content.contains(&voice_interruption_marker(LANGUAGE)));
    assert!(session.usage().tokens <= session.usage().max_tokens);
}

#[test]
fn next_request_keeps_interruption_semantics_after_history_trimming() {
    let marker = voice_interruption_marker(LANGUAGE);
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 4,
        max_tokens: 10,
        max_request_tokens: context_message_tokens(&marker, 4) + 8,
    })
    .expect("测试限制应当有效");
    let interrupted = session.start_turn("a").expect("第一轮应当可开始");
    session
        .append_response(interrupted.response_id, "b")
        .expect("部分回复应当可写入");
    assert!(session.interrupt_response_by_voice(interrupted.response_id));

    let next = session
        .start_turn_with_image("next", None, LANGUAGE)
        .expect("旧轮次应当被裁剪以容纳新消息");

    assert_eq!(next.context.len(), 1);
    assert!(next.context[0].content.starts_with(&marker));
    assert!(next.context[0].content.ends_with("next"));
    assert_eq!(session.messages()[0].content(), "next");
}

#[test]
fn no_room_voice_interruption_survives_snapshot_restore() {
    let marker = voice_interruption_marker(LANGUAGE);
    let limits = ChatLimits {
        max_messages: 4,
        max_tokens: 10,
        max_request_tokens: context_message_tokens(&marker, 4) + 8,
    };
    let mut session = ChatSession::new(limits).expect("测试限制应当有效");
    let interrupted = session.start_turn("a").expect("第一轮应当可开始");
    session
        .append_response(interrupted.response_id, "b")
        .expect("部分回复应当可写入");
    assert!(session.interrupt_response_by_voice(interrupted.response_id));
    assert_eq!(
        session.messages()[1].state(),
        &ChatMessageState::InterruptedByVoice
    );

    let snapshot = session.snapshot(1);
    let mut restored = ChatSession::from_snapshot(snapshot, limits).expect("快照应当可以恢复");
    let next = restored
        .start_turn_with_image("c", None, LANGUAGE)
        .expect("恢复后应当可继续对话");

    assert!(
        next.context
            .last()
            .is_some_and(|message| message.content.starts_with(&marker))
    );
}

#[test]
fn the_latest_voice_interruption_uses_its_own_state_marker() {
    let marker = voice_interruption_marker(LANGUAGE);
    let mut session = ChatSession::default();
    let first = session.start_turn("first").expect("首轮应可开始");
    session
        .append_response(first.response_id, "first answer")
        .expect("首轮回复应可写入");
    assert!(session.interrupt_response_by_voice(first.response_id));
    assert_eq!(session.messages()[1].content(), "first answer");

    let second = session
        .start_turn_with_image("second", None, LANGUAGE)
        .expect("第二轮应可开始");
    session
        .append_response(second.response_id, "b")
        .expect("第二轮部分回复应可写入");
    let current_tokens = session.usage().tokens;
    session
        .update_limits(ChatLimits {
            max_tokens: current_tokens,
            max_request_tokens: usize::MAX,
            ..ChatLimits::default()
        })
        .expect("测试预算应可应用");
    assert!(session.interrupt_response_by_voice(second.response_id));
    assert_eq!(session.messages()[3].content(), "b");
    let latest_assistant_id = session.messages()[3].id();

    session
        .update_limits(ChatLimits::default())
        .expect("恢复默认预算应成功");
    let third = session
        .start_turn_with_image("third", None, LANGUAGE)
        .expect("第三轮应可开始");
    assert!(
        third
            .context
            .iter()
            .find(|message| message.source_message_id == Some(latest_assistant_id))
            .is_some_and(|message| message.content.ends_with(&marker))
    );
    assert_eq!(
        third.context.last().map(|message| message.content.as_str()),
        Some("third")
    );
}
