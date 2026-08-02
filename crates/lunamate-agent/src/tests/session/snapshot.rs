use crate::{media::prepare_dynamic_image, session::*};

use super::{LANGUAGE, reasoning_trace, tool_trace};

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
