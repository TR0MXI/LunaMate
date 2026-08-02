use crate::{
    chat_limits,
    config::{
        LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings, ModelKind, ModelProvider,
        PersonaConfig, PersonaContextLimits,
    },
    memory::AssistantTrace,
    session::*,
};

use super::reasoning_trace;

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
            voice: None,
            voice_type: None,
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
            voice: None,
            voice_type: None,
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
