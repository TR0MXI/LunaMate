use crate::{memory::AssistantTrace, session::*};

use super::{reasoning_trace, tool_trace};

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
fn discarding_an_active_response_removes_its_entire_turn() {
    let mut session = ChatSession::default();
    let completed = session
        .start_turn("kept question")
        .expect("保留轮次应可开始");
    session
        .append_response(completed.response_id, "kept answer")
        .expect("保留回复应可写入");
    assert!(session.finish_response(completed.response_id));
    let active = session
        .start_turn("discarded question")
        .expect("待丢弃轮次应可开始");
    session
        .append_response(active.response_id, "partial answer")
        .expect("部分回复应可写入");

    assert!(session.discard_response_turn(active.response_id));
    assert_eq!(session.active_response_id(), None);
    assert_eq!(session.messages().len(), 2);
    assert!(
        session
            .messages()
            .iter()
            .all(|message| !message.content().contains("discarded"))
    );
    assert!(matches!(
        session.append_response(active.response_id, "late"),
        Err(ChatError::StaleResponse)
    ));
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
