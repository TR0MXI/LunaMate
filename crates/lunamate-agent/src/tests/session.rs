use crate::{
    Agent, AgentMemory, Client, chat_limits,
    config::{
        AppLanguage, LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings, ModelKind,
        ModelProvider, PersonaConfig, PersonaContextLimits,
    },
    media::prepare_dynamic_image,
    memory::{AssistantTrace, ToolExecutionTrace},
    session::*,
};

const LANGUAGE: AppLanguage = AppLanguage::English;

fn reasoning_trace(reasoning: impl Into<String>) -> AssistantTrace {
    AssistantTrace::new(Some(reasoning.into()), Vec::new())
}

fn tool_trace(reasoning: &str) -> AssistantTrace {
    AssistantTrace::new(
        Some(reasoning.to_owned()),
        vec![ToolExecutionTrace::new(
            "local_tool".to_owned(),
            serde_json::json!({"input": reasoning}),
            serde_json::json!({"status": "ok"}),
        )],
    )
}

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
fn editable_context_exposes_assistant_trace_without_changing_tokens_or_provider_context() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("用户消息应当可发送");
    session
        .append_response(started.response_id, "answer")
        .expect("测试回复应可写入");
    let tokens_before_trace = session.usage().tokens;
    assert!(
        session
            .attach_response_trace(started.response_id, tool_trace("reasoning"))
            .expect("匹配响应应可附加详情")
    );
    assert_eq!(session.usage().tokens, tokens_before_trace);
    assert!(session.finish_response(started.response_id));

    let editable = session.editable_messages();
    assert!(editable[0].trace.is_none(), "用户消息不得携带助手详情");
    let trace = editable[1]
        .trace
        .as_ref()
        .expect("助手详情应暴露给设置 DTO");
    assert_eq!(trace.reasoning(), Some("reasoning"));
    assert_eq!(trace.tool_executions()[0].name(), "local_tool");

    let next = session.start_turn("next").expect("下一轮应可开始");
    assert_eq!(
        next.context
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        [
            (ChatRole::User, "hello"),
            (ChatRole::Assistant, "answer"),
            (ChatRole::User, "next"),
        ]
    );
}

#[test]
fn image_content_stays_in_memory_but_not_in_snapshot() {
    let image = prepare_dynamic_image(
        image::DynamicImage::new_rgb8(8, 6),
        "private-sample.jpg".to_owned(),
    )
    .expect("测试图片应当可以规范化");
    let mut session = ChatSession::default();
    let started = session
        .start_turn_with_image("", Some(image), LANGUAGE)
        .expect("纯图片消息应当可以发送");

    assert_eq!(
        started.context[0].content,
        rust_i18n::t!("chat.image_only_prompt", locale = LANGUAGE.id())
    );
    assert!(
        started.context[0]
            .image
            .as_ref()
            .and_then(|image| image.bytes())
            .is_some()
    );
    let tokens_with_pixels = session.usage().tokens;
    let encoded = serde_json::to_vec(&session.snapshot(1)).expect("会话快照应当可以序列化");
    assert!(encoded.len() < 1_024, "图片字节不得进入会话快照");
    let document: serde_json::Value =
        serde_json::from_slice(&encoded).expect("会话快照 JSON 应当有效");
    assert_eq!(document["messages"][0]["image"], serde_json::json!(true));
    let encoded_text = std::str::from_utf8(&encoded).expect("JSON 应当是 UTF-8");
    assert!(!encoded_text.contains("private-sample"));
    assert!(!encoded_text.contains("width"));
    assert!(!encoded_text.contains("height"));
    assert!(!encoded_text.contains("bytes"));
    let snapshot = serde_json::from_slice(&encoded).expect("会话快照应当可以反序列化");
    let restored = ChatSession::from_snapshot(snapshot, ChatLimits::default())
        .expect("不含图片像素的快照应当可以恢复");
    let restored_image = restored.messages()[0]
        .image()
        .expect("图片存在标记应当保留");
    assert_eq!(restored_image.name(), "image.jpg");
    assert_eq!((restored_image.width(), restored_image.height()), (1, 1));
    assert!(restored_image.bytes().is_none());
    assert_eq!(
        tokens_with_pixels.saturating_sub(restored.usage().tokens),
        1_024,
        "重启后只有图片元数据，存储用量不能继续占用真实图片预算"
    );
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
fn restored_image_placeholder_counts_against_the_request_budget() {
    let image = prepare_dynamic_image(image::DynamicImage::new_rgb8(8, 6), "sample.jpg".to_owned())
        .expect("测试图片应当可以规范化");
    let mut session = ChatSession::default();
    let first = session
        .start_turn_with_image("", Some(image), LANGUAGE)
        .expect("纯图片消息应可发送");
    session
        .append_response(first.response_id, "answer")
        .expect("测试回复应可写入");
    assert!(session.finish_response(first.response_id));
    let base_tokens = session
        .messages()
        .iter()
        .map(|message| context_message_tokens(message.content(), 4))
        .sum::<usize>()
        .saturating_add(context_message_tokens("next", 4));
    let limits = ChatLimits {
        max_request_tokens: base_tokens,
        ..ChatLimits::default()
    };
    let snapshot =
        serde_json::from_slice(&serde_json::to_vec(&session.snapshot(1)).expect("快照应可编码"))
            .expect("快照应可解码");
    let mut restored = ChatSession::from_snapshot(snapshot, limits).expect("快照应可恢复");

    let next = restored.start_turn("next").expect("下一轮应可开始");

    assert_eq!(next.context.len(), 1);
    assert_eq!(next.context[0].content, "next");
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
fn history_trims_complete_turns_without_evicting_active_response() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 4,
        max_tokens: 14,
        max_request_tokens: 14,
    })
    .expect("测试限制必须有效");
    let first = session.start_turn("first").expect("第一轮应当可开始");
    session
        .append_response(first.response_id, "one")
        .expect("第一轮回复应当可写入");
    assert!(session.finish_response(first.response_id));

    let second = session.start_turn("second").expect("第二轮应当可开始");
    session
        .append_response(second.response_id, "two")
        .expect("活动回复应当通过淘汰旧轮次获得空间");

    assert_eq!(session.messages().len(), 2);
    assert_eq!(session.messages()[0].content(), "second");
    assert_eq!(session.messages()[1].content(), "two");
}

#[test]
fn updating_limits_keeps_the_newest_fitting_history() {
    let mut session = ChatSession::default();
    for (question, answer) in [("first", "one"), ("second", "two")] {
        let turn = session.start_turn(question).expect("测试轮次应可开始");
        session
            .append_response(turn.response_id, answer)
            .expect("测试回复应可写入");
        assert!(session.finish_response(turn.response_id));
    }

    session
        .update_limits(ChatLimits {
            max_messages: 2,
            ..ChatLimits::default()
        })
        .expect("有效的新限制应可应用");

    assert_eq!(session.messages().len(), 2);
    assert_eq!(session.messages()[0].content(), "second");
    assert_eq!(session.messages()[1].content(), "two");
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

#[test]
fn restoring_streaming_response_marks_it_interrupted() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("测试轮次应当可开始");
    session
        .append_response(started.response_id, "partial")
        .expect("测试增量应当可写入");
    let snapshot = session.snapshot(7);

    let restored = ChatSession::from_snapshot(snapshot, ChatLimits::default())
        .expect("当前版本快照应当可恢复");
    assert_eq!(restored.messages().len(), 2);
    assert_eq!(
        restored.messages()[1].state(),
        &ChatMessageState::Interrupted
    );
    assert_eq!(restored.active_response_id(), None);
}

#[test]
fn assistant_trace_round_trips_with_the_same_message() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("测试轮次应当可开始");
    session
        .append_response(started.response_id, "answer")
        .expect("测试回复应可写入");
    let expected = tool_trace("inspect");
    assert!(
        session
            .attach_response_trace(started.response_id, expected.clone())
            .expect("助手详情应可附加")
    );
    assert!(session.finish_response(started.response_id));

    let encoded = serde_json::to_vec(&session.snapshot(8)).expect("含详情快照应可编码");
    let restored = ChatSession::from_snapshot(
        serde_json::from_slice(&encoded).expect("含详情快照应可解码"),
        ChatLimits::default(),
    )
    .expect("含详情快照应可恢复");
    assert_eq!(restored.messages()[1].trace(), Some(&expected));
}

#[test]
fn snapshot_requires_every_field_in_the_current_message_and_trace_format() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("测试轮次应当可开始");
    assert!(
        session
            .attach_response_trace(started.response_id, reasoning_trace("reasoning"))
            .expect("助手详情应可附加")
    );
    assert!(session.finish_response(started.response_id));
    let current = serde_json::to_value(session.snapshot(1)).expect("当前快照应可编码");

    for (message_index, field) in [(0, "image"), (0, "trace")] {
        let mut missing = current.clone();
        missing["messages"][message_index]
            .as_object_mut()
            .expect("消息应为 JSON 对象")
            .remove(field);
        assert!(serde_json::from_value::<ChatSessionSnapshot>(missing).is_err());
    }
    for field in ["reasoning", "tool_executions"] {
        let mut missing = current.clone();
        missing["messages"][1]["trace"]
            .as_object_mut()
            .expect("助手详情应为 JSON 对象")
            .remove(field);
        assert!(serde_json::from_value::<ChatSessionSnapshot>(missing).is_err());
    }
}

#[test]
fn untrusted_snapshot_rejects_user_invalid_and_oversized_trace_metadata() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("测试轮次应当可开始");
    assert!(session.finish_response(started.response_id));
    let base = serde_json::to_value(session.snapshot(1)).expect("测试快照应可转换为 JSON");

    let mut user_trace = base.clone();
    user_trace["messages"][0]["trace"] =
        serde_json::json!({"reasoning": "forbidden", "tool_executions": []});
    let snapshot = serde_json::from_value(user_trace).expect("详情结构本身应可反序列化");
    assert!(matches!(
        ChatSession::from_snapshot(snapshot, ChatLimits::default()),
        Err(ChatError::InvalidSnapshot)
    ));

    let mut oversized_reasoning = base.clone();
    oversized_reasoning["messages"][1]["trace"] = serde_json::json!({
        "reasoning": "x".repeat(MAX_TRACE_REASONING_BYTES + 1),
        "tool_executions": []
    });
    let snapshot = serde_json::from_value(oversized_reasoning).expect("超限详情结构应可反序列化");
    assert!(matches!(
        ChatSession::from_snapshot(snapshot, ChatLimits::default()),
        Err(ChatError::InvalidSnapshot)
    ));

    let mut too_many_tools = base;
    too_many_tools["messages"][1]["trace"] = serde_json::json!({
        "reasoning": null,
        "tool_executions": (0..=MAX_MESSAGE_TOOL_EXECUTIONS)
            .map(|index| serde_json::json!({
                "name": format!("tool_{index}"),
                "arguments": {},
                "result": {"status": "ok"}
            }))
            .collect::<Vec<_>>()
    });
    let snapshot = serde_json::from_value(too_many_tools).expect("超限工具列表应可反序列化");
    assert!(matches!(
        ChatSession::from_snapshot(snapshot, ChatLimits::default()),
        Err(ChatError::InvalidSnapshot)
    ));
}

#[test]
fn untrusted_snapshot_rejects_aggregate_trace_byte_and_count_overflow() {
    let limits = ChatLimits {
        max_messages: 130,
        max_tokens: usize::MAX,
        max_request_tokens: usize::MAX,
    };
    let mut session = ChatSession::new(limits).expect("测试限制应有效");
    for index in 0..65 {
        let turn = session
            .start_turn(format!("q{index}"))
            .expect("测试轮次应可开始");
        session
            .append_response(turn.response_id, "a")
            .expect("测试回复应可写入");
        assert!(session.finish_response(turn.response_id));
    }
    let base = serde_json::to_value(session.snapshot(1)).expect("测试快照应可转换为 JSON");

    let mut too_many = base.clone();
    for message in too_many["messages"]
        .as_array_mut()
        .expect("消息列表应为 JSON 数组")
        .iter_mut()
        .skip(1)
        .step_by(2)
    {
        message["trace"] = serde_json::json!({"reasoning": "r", "tool_executions": []});
    }
    let snapshot = serde_json::from_value(too_many).expect("聚合超限快照应可反序列化");
    assert!(matches!(
        ChatSession::from_snapshot(snapshot, limits),
        Err(ChatError::InvalidSnapshot)
    ));

    let mut too_large = base;
    for message in too_large["messages"]
        .as_array_mut()
        .expect("消息列表应为 JSON 数组")
        .iter_mut()
        .skip(1)
        .step_by(2)
        .take(18)
    {
        message["trace"] = serde_json::json!({
            "reasoning": "r".repeat(60 * 1024),
            "tool_executions": []
        });
    }
    let snapshot = serde_json::from_value(too_large).expect("聚合字节超限快照应可反序列化");
    assert!(matches!(
        ChatSession::from_snapshot(snapshot, limits),
        Err(ChatError::InvalidSnapshot)
    ));
}

#[test]
fn history_trimming_discards_the_old_trace_without_moving_it() {
    let limits = ChatLimits {
        max_messages: 2,
        max_tokens: usize::MAX,
        max_request_tokens: usize::MAX,
    };
    let mut session = ChatSession::new(limits).expect("测试限制应有效");
    let first = session.start_turn("first").expect("第一轮应可开始");
    assert!(
        session
            .attach_response_trace(first.response_id, reasoning_trace("first trace"))
            .expect("第一条详情应可附加")
    );
    assert!(session.finish_response(first.response_id));

    let second = session.start_turn("second").expect("第二轮应淘汰旧历史");
    assert!(
        session
            .attach_response_trace(second.response_id, reasoning_trace("second trace"))
            .expect("第二条详情应可附加")
    );

    assert_eq!(session.messages().len(), 2);
    assert_eq!(session.messages()[0].content(), "second");
    assert_eq!(
        session.messages()[1]
            .trace()
            .and_then(AssistantTrace::reasoning),
        Some("second trace")
    );
}

#[test]
fn oversized_active_turn_does_not_evict_previous_history() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 6,
        max_tokens: 20,
        max_request_tokens: 20,
    })
    .expect("测试限制应当有效");
    let first = session.start_turn("a").expect("首轮应当可开始");
    session
        .append_response(first.response_id, "b")
        .expect("首轮回复应当可写入");
    session.finish_response(first.response_id);
    let second = session.start_turn("12345").expect("第二轮应当可开始");

    let error = session
        .append_response(
            second.response_id,
            "123456789012345678901234567890123456789012345678901234567890",
        )
        .expect_err("活动轮次自身超限时必须拒绝");
    assert_eq!(error, ChatError::MessageTooLarge);
    assert_eq!(session.messages().len(), 4);
    assert_eq!(session.messages()[0].content(), "a");
}

#[test]
fn user_message_must_leave_room_for_response() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 2,
        max_tokens: 8,
        max_request_tokens: 8,
    })
    .expect("测试限制应当有效");

    assert!(matches!(
        session.start_turn("1234"),
        Err(ChatError::MessageTooLarge)
    ));
}

#[test]
fn zero_token_budget_restores_an_empty_session_but_rejects_messages() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 2,
        max_tokens: 0,
        max_request_tokens: 0,
    })
    .expect("耗尽的模型窗口仍应允许创建空会话");

    assert!(matches!(
        session.start_turn("hello"),
        Err(ChatError::MessageTooLarge)
    ));
    assert!(session.messages().is_empty());
}

#[test]
fn an_oversized_new_message_does_not_trim_existing_history() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 4,
        max_tokens: 12,
        max_request_tokens: 12,
    })
    .expect("测试限制应当有效");
    let first = session.start_turn("a").expect("首轮应可开始");
    session
        .append_response(first.response_id, "b")
        .expect("首轮回复应可写入");
    assert!(session.finish_response(first.response_id));

    assert!(matches!(
        session.start_turn("a message that cannot fit in this context"),
        Err(ChatError::MessageTooLarge)
    ));
    assert_eq!(session.messages().len(), 2);
    assert_eq!(session.messages()[0].content(), "a");
    assert_eq!(session.messages()[1].content(), "b");
}

#[test]
fn a_snapshot_with_an_individually_deleted_message_remains_valid() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("测试轮次应当可开始");
    session.finish_response(started.response_id);
    let mut snapshot = session.snapshot(1);
    let remaining_id = snapshot.messages[1].id();
    snapshot.messages.remove(0);

    let restored = ChatSession::from_snapshot(snapshot, ChatLimits::default())
        .expect("单条删除后留下的消息仍应可以恢复");
    assert_eq!(restored.messages().len(), 1);
    assert_eq!(restored.messages()[0].id(), remaining_id);
}

#[test]
fn a_snapshot_with_reversed_roles_is_rejected() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("测试轮次应当可开始");
    assert!(session.finish_response(started.response_id));
    let mut snapshot = session.snapshot(1);
    snapshot.messages.swap(0, 1);

    assert!(matches!(
        ChatSession::from_snapshot(snapshot, ChatLimits::default()),
        Err(ChatError::InvalidSnapshot)
    ));
}

#[test]
fn individual_messages_can_be_edited_and_deleted_without_removing_their_turn() {
    let mut session = ChatSession::default();
    let started = session
        .start_turn("old question")
        .expect("测试轮次应可开始");
    session
        .append_response(started.response_id, "old answer")
        .expect("测试回复应可写入");
    assert!(
        session
            .attach_response_trace(started.response_id, reasoning_trace("old reasoning"))
            .expect("测试详情应可附加")
    );
    assert!(session.finish_response(started.response_id));
    let user_id = session.messages()[0].id();
    let assistant_id = session.messages()[1].id();
    let tokens_before = session.usage().tokens;

    session
        .edit_message(assistant_id, "new answer")
        .expect("完整消息应可编辑");
    assert_eq!(session.messages()[1].content(), "new answer");
    assert!(
        session.messages()[1].trace().is_none(),
        "编辑助手正文必须清除旧详情"
    );
    assert!(session.delete_message(user_id).expect("用户消息应可删除"));
    assert_eq!(session.messages().len(), 1);
    assert_eq!(session.messages()[0].id(), assistant_id);
    assert!(session.usage().tokens < tokens_before);

    let next = session.start_turn("next").expect("编辑后应可继续对话");
    // 删除用户消息后，孤立的 assistant 仍留在编辑器中，但不会作为非法首消息发给 Provider。
    assert_eq!(next.context.len(), 1);
    assert_eq!(next.context[0].content, "next");
}

#[test]
fn deleting_multiple_messages_is_atomic_while_a_selected_turn_is_active() {
    let mut session = ChatSession::default();
    let first = session
        .start_turn("first question")
        .expect("第一轮应可开始");
    session
        .append_response(first.response_id, "first answer")
        .expect("第一轮回复应可写入");
    assert!(session.finish_response(first.response_id));
    let first_user = session.messages()[0].id();

    let second = session
        .start_turn("second question")
        .expect("第二轮应可开始");
    let second_user = session.messages()[2].id();
    let before = session
        .messages()
        .iter()
        .map(ChatMessage::id)
        .collect::<Vec<_>>();

    assert!(matches!(
        session.delete_messages(&[first_user, second_user]),
        Err(ChatError::Busy)
    ));
    assert_eq!(
        session
            .messages()
            .iter()
            .map(ChatMessage::id)
            .collect::<Vec<_>>(),
        before
    );

    assert!(session.finish_response(second.response_id));
    assert_eq!(
        session
            .delete_messages(&[first_user, second_user])
            .expect("终态消息应可批量删除"),
        2
    );
    assert!(
        session
            .messages()
            .iter()
            .all(|message| ![first_user, second_user].contains(&message.id()))
    );
}

#[test]
fn deleting_messages_never_transfers_an_assistant_trace() {
    let mut session = ChatSession::default();
    for (question, answer, reasoning) in [
        ("question 1", "answer 1", "trace 1"),
        ("question 2", "answer 2", "trace 2"),
    ] {
        let turn = session.start_turn(question).expect("测试轮次应可开始");
        session
            .append_response(turn.response_id, answer)
            .expect("测试回复应可写入");
        assert!(
            session
                .attach_response_trace(turn.response_id, reasoning_trace(reasoning))
                .expect("测试详情应可附加")
        );
        assert!(session.finish_response(turn.response_id));
    }
    let first_user_id = session.messages()[0].id();
    let first_assistant_id = session.messages()[1].id();
    let second_assistant_id = session.messages()[3].id();

    assert!(
        session
            .delete_message(first_user_id)
            .expect("单独删除用户消息应成功")
    );
    assert_eq!(
        session
            .messages()
            .iter()
            .find(|message| message.id() == first_assistant_id)
            .and_then(ChatMessage::trace)
            .and_then(AssistantTrace::reasoning),
        Some("trace 1")
    );
    assert!(
        session
            .delete_message(first_assistant_id)
            .expect("单独删除助手消息应成功")
    );
    assert!(
        session
            .messages()
            .iter()
            .all(|message| message.id() != first_assistant_id)
    );
    assert_eq!(
        session
            .messages()
            .iter()
            .find(|message| message.id() == second_assistant_id)
            .and_then(ChatMessage::trace)
            .and_then(AssistantTrace::reasoning),
        Some("trace 2")
    );
}

#[test]
fn an_incomplete_historical_turn_is_not_sent_to_the_provider() {
    let mut session = ChatSession::default();
    let started = session
        .start_turn("old question")
        .expect("测试轮次应可开始");
    session
        .append_response(started.response_id, "old answer")
        .expect("测试回复应可写入");
    assert!(session.finish_response(started.response_id));
    let assistant_id = session.messages()[1].id();
    assert!(
        session
            .delete_message(assistant_id)
            .expect("助手消息应可单独删除")
    );

    let next = session.start_turn("next").expect("删除后应可继续对话");
    assert_eq!(next.context.len(), 1);
    assert_eq!(next.context[0].content, "next");
}

#[test]
fn token_estimation_accounts_for_ascii_words_cjk_and_emoji() {
    assert_eq!(estimate_text_tokens(""), 0);
    assert!(estimate_text_tokens("abcdefgh") >= 2);
    assert!(estimate_text_tokens("你好世界") >= 4);
    assert!(estimate_text_tokens("😀😀") >= 4);
}

#[test]
fn configured_model_window_reserves_output_and_system_prompt_tokens() {
    let settings = LlmSettings {
        models: vec![LlmModelConfig {
            id: "small".to_owned(),
            label: "Small".to_owned(),
            kind: ModelKind::ChatCompletions,
            provider: ModelProvider::Genai(LlmProvider::Ollama),
            model: "small-model".to_owned(),
            endpoint: None,
            api_key: None,
            app_id: None,
            voice: None,
            local_path: None,
            use_gpu: false,
            whisper_language: None,
            advanced: LlmAdvancedOptions {
                context_window_tokens: Some(1_000),
                max_output_tokens: Some(200),
                ..LlmAdvancedOptions::default()
            },
        }],
        selected_model: Some("small".to_owned()),
        selected_transcription_model: None,
    };
    let mut persona = PersonaConfig::new("test", "Test");
    persona.system_prompt = "system prompt".to_owned();
    persona.context = PersonaContextLimits {
        max_messages: Some(20),
        max_tokens: Some(900),
    };

    let limits = chat_limits(&persona, &settings);

    assert_eq!(limits.max_messages, 20);
    assert_eq!(
        limits.max_tokens,
        1_000 - estimate_text_tokens(&persona.system_prompt) - 512
    );
    assert_eq!(
        limits.max_request_tokens,
        1_000 - 200 - estimate_text_tokens(&persona.system_prompt) - 512
    );
}

#[test]
fn output_reserve_trims_the_next_request_without_truncating_the_current_reply() {
    let settings = LlmSettings {
        models: vec![LlmModelConfig {
            id: "long-output".to_owned(),
            label: "Long output".to_owned(),
            kind: ModelKind::ChatCompletions,
            provider: ModelProvider::Genai(LlmProvider::Ollama),
            model: "long-output-model".to_owned(),
            endpoint: None,
            api_key: None,
            app_id: None,
            voice: None,
            local_path: None,
            use_gpu: false,
            whisper_language: None,
            advanced: LlmAdvancedOptions {
                context_window_tokens: Some(10_000),
                max_output_tokens: Some(8_000),
                ..LlmAdvancedOptions::default()
            },
        }],
        selected_model: Some("long-output".to_owned()),
        selected_transcription_model: None,
    };
    let mut persona = PersonaConfig::new("test", "Test");
    persona.context = PersonaContextLimits {
        max_messages: Some(20),
        max_tokens: Some(10_000),
    };
    let limits = chat_limits(&persona, &settings);
    assert_eq!(limits.max_tokens, 10_000 - 512);
    assert_eq!(limits.max_request_tokens, 10_000 - 8_000 - 512);

    let mut session = ChatSession::new(limits).expect("拆分后的预算应有效");
    let first = session.start_turn("question").expect("首轮应可开始");
    session
        .append_response(first.response_id, &"a".repeat(6_000))
        .expect("输出预留不得被再次当作当前回复上限");
    assert!(session.finish_response(first.response_id));

    let next = session.start_turn("next").expect("下一轮应可开始");
    assert_eq!(next.context.len(), 1);
    assert_eq!(next.context[0].content, "next");
}
