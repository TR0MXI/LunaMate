use crate::agent::{media::prepare_dynamic_image, session::*};

#[test]
fn context_excludes_streaming_placeholder() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("用户消息应当可发送");

    assert_eq!(
        started.context,
        vec![ChatContextMessage {
            role: ChatRole::User,
            content: "hello".to_owned(),
            image: None,
        }]
    );
    assert_eq!(session.messages().len(), 2);
}

#[test]
fn image_content_stays_in_memory_but_not_in_snapshot() {
    let image = prepare_dynamic_image(image::DynamicImage::new_rgb8(8, 6), "sample.jpg".to_owned())
        .expect("测试图片应当可以规范化");
    let mut session = ChatSession::default();
    let started = session
        .start_turn_with_image("", Some(image))
        .expect("纯图片消息应当可以发送");

    assert_eq!(
        started.context[0].content,
        rust_i18n::t!("chat.image_only_prompt")
    );
    assert!(
        started.context[0]
            .image
            .as_ref()
            .and_then(|image| image.bytes())
            .is_some()
    );
    let encoded = serde_json::to_vec(&session.snapshot(1)).expect("会话快照应当可以序列化");
    assert!(encoded.len() < 1_024, "图片字节不得进入会话快照");
    let snapshot = serde_json::from_slice(&encoded).expect("会话快照应当可以反序列化");
    let restored = ChatSession::from_snapshot(snapshot, ChatLimits::default())
        .expect("不含图片像素的快照应当可以恢复");
    let restored_image = restored.messages()[0]
        .image()
        .expect("图片安全元数据应当保留");
    assert_eq!(restored_image.name(), "sample.jpg");
    assert!(restored_image.bytes().is_none());
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
fn history_trims_complete_turns_without_evicting_active_response() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 4,
        max_bytes: 14,
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
fn failed_turn_is_not_replayed_in_next_context() {
    let mut session = ChatSession::default();
    let failed = session.start_turn("failed").expect("失败轮次应当可开始");
    assert!(session.fail_response(failed.response_id, "offline".to_owned()));
    let next = session.start_turn("next").expect("失败后应当可继续");

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
fn oversized_active_turn_does_not_evict_previous_history() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 6,
        max_bytes: 12,
    })
    .expect("测试限制应当有效");
    let first = session.start_turn("a").expect("首轮应当可开始");
    session
        .append_response(first.response_id, "b")
        .expect("首轮回复应当可写入");
    session.finish_response(first.response_id);
    let second = session.start_turn("12345").expect("第二轮应当可开始");

    let error = session
        .append_response(second.response_id, "12345678")
        .expect_err("活动轮次自身超限时必须拒绝");
    assert_eq!(error, ChatError::MessageTooLarge);
    assert_eq!(session.messages().len(), 4);
    assert_eq!(session.messages()[0].content(), "a");
}

#[test]
fn user_message_must_leave_room_for_response() {
    let mut session = ChatSession::new(ChatLimits {
        max_messages: 2,
        max_bytes: 4,
    })
    .expect("测试限制应当有效");

    assert!(matches!(
        session.start_turn("1234"),
        Err(ChatError::MessageTooLarge)
    ));
}

#[test]
fn malformed_snapshot_is_rejected_instead_of_silently_dropping_messages() {
    let mut session = ChatSession::default();
    let started = session.start_turn("hello").expect("测试轮次应当可开始");
    session.finish_response(started.response_id);
    let mut snapshot = session.snapshot(1);
    snapshot.messages.pop();

    assert!(matches!(
        ChatSession::from_snapshot(snapshot, ChatLimits::default()),
        Err(ChatError::InvalidSnapshot)
    ));
}
