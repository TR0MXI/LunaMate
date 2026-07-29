//! 验证人格配置的解析、校验与写回往返。

use std::path::Path;

use toml_edit::DocumentMut;

use crate::config::{
    CONTEXT_MESSAGES_MIN, CONTEXT_TOKENS_MAX, DEFAULT_CONTEXT_MESSAGES, DEFAULT_CONTEXT_TOKENS,
    DEFAULT_PERSONA_ID, PersonaConfig, PersonaContextLimits, PersonaSettings,
    parse_persona_settings, write_persona_settings,
};

fn document(source: &str) -> DocumentMut {
    source.parse::<DocumentMut>().expect("测试配置应当可以解析")
}

#[test]
fn an_empty_configuration_still_yields_one_persona() {
    let settings = parse_persona_settings(&DocumentMut::new(), &mut Vec::new());

    assert_eq!(settings.personas.len(), 1);
    assert_eq!(settings.selected.as_deref(), Some(DEFAULT_PERSONA_ID));
    assert!(
        settings
            .active()
            .expect("默认人格必须存在")
            .system_prompt
            .is_empty()
    );
}

#[test]
fn legacy_system_prompt_migrates_into_the_default_persona() {
    let mut source = document(
        r#"
[llm]
system_prompt = "沿用旧人格"
"#,
    );

    let settings = parse_persona_settings(&source, &mut Vec::new());

    assert_eq!(
        settings
            .active()
            .map(|persona| persona.system_prompt.as_str()),
        Some("沿用旧人格")
    );
    write_persona_settings(&mut source, &settings);
    assert!(
        source
            .get("llm")
            .and_then(|llm| llm.get("system_prompt"))
            .is_none(),
        "写回人格后必须移除旧提示词来源"
    );
}

#[test]
fn stored_personas_round_trip_through_the_document() {
    let mut source = document(
        r#"
[persona]
selected = "moon"

[[persona.list]]
id = "moon"
name = "露娜"
system_prompt = "保持简洁"
input_prompt = "用户说：{input}"
model = "cloud"
live2d_model = "luna/luna.model3.json"
max_context_messages = 16
max_context_tokens = 8256
future_option = "keep"

[[persona.list]]
id = "study"
name = "学习助手"
"#,
    );
    let mut warnings = Vec::new();

    let settings = parse_persona_settings(&source, &mut warnings);

    assert!(warnings.is_empty(), "警告：{warnings:?}");
    assert_eq!(settings.personas.len(), 2);
    assert_eq!(settings.selected.as_deref(), Some("moon"));
    let moon = settings.active().expect("选中人格必须存在");
    assert_eq!(moon.model.as_deref(), Some("cloud"));
    assert_eq!(moon.input_prompt, "用户说：{input}");
    assert_eq!(
        moon.live2d_model.as_deref(),
        Some(Path::new("luna/luna.model3.json"))
    );
    assert_eq!(
        moon.context,
        PersonaContextLimits {
            max_messages: Some(16),
            max_tokens: Some(8_256),
        }
    );
    // 未设置上限的人格使用默认值，而不是把 0 当成"无限制"。
    assert_eq!(
        settings.personas[1].context,
        PersonaContextLimits::default()
    );

    write_persona_settings(&mut source, &settings);
    let mut rewritten = Vec::new();
    assert_eq!(parse_persona_settings(&source, &mut rewritten), settings);
    assert!(rewritten.is_empty());
    let saved = source.to_string();
    assert!(saved.contains("future_option = \"keep\""));
    assert!(saved.contains("max_context_tokens = 8256"));
    assert!(saved.contains("input_prompt = \"用户说：{input}\""));
    assert!(saved.contains("live2d_model = \"luna/luna.model3.json\""));
}

#[test]
fn pending_deletions_round_trip_and_reject_active_or_unsafe_ids() {
    let mut source = document(
        r#"
[persona]
selected = "moon"
pending_deletions = ["removed", "bad id", "moon", "removed", 7]

[[persona.list]]
id = "moon"
name = "露娜"
"#,
    );
    let mut warnings = Vec::new();

    let settings = parse_persona_settings(&source, &mut warnings);

    assert_eq!(settings.pending_deletions, ["removed"]);
    assert_eq!(warnings.len(), 3);
    write_persona_settings(&mut source, &settings);
    assert_eq!(parse_persona_settings(&source, &mut Vec::new()), settings);

    let mut conflicting = settings;
    conflicting.pending_deletions.push("moon".to_owned());
    assert!(conflicting.normalized().is_err());
}

#[test]
fn pending_deletions_survive_a_missing_persona_list() {
    let source = document(
        r#"
[persona]
pending_deletions = ["removed"]
"#,
    );
    let mut warnings = Vec::new();

    let settings = parse_persona_settings(&source, &mut warnings);

    assert_eq!(settings.personas.len(), 1);
    assert_eq!(settings.pending_deletions, ["removed"]);
    assert_eq!(warnings.len(), 1);
}

#[test]
fn a_single_malformed_persona_does_not_discard_the_others() {
    let document = document(
        r#"
[persona]
selected = "good"

[[persona.list]]
id = "bad id"
name = "非法 ID"

[[persona.list]]
id = "no-name"

[[persona.list]]
id = "bad-limit"
name = "越界上限"
max_context_messages = 0

[[persona.list]]
id = "good"
name = "可用人格"
"#,
    );
    let mut warnings = Vec::new();

    let settings = parse_persona_settings(&document, &mut warnings);

    assert_eq!(settings.personas.len(), 1);
    assert_eq!(settings.personas[0].id, "good");
    assert_eq!(warnings.len(), 3);
}

#[test]
fn duplicate_ids_and_a_missing_selection_are_reported() {
    let document = document(
        r#"
[persona]
selected = "removed"

[[persona.list]]
id = "moon"
name = "第一个"

[[persona.list]]
id = "moon"
name = "重复 ID"
"#,
    );
    let mut warnings = Vec::new();

    let settings = parse_persona_settings(&document, &mut warnings);

    assert_eq!(settings.personas.len(), 1);
    assert_eq!(settings.selected, None);
    // 选择会退回第一条，而不是让当前人格变成空。
    assert_eq!(
        settings.active().map(|persona| persona.id.as_str()),
        Some("moon")
    );
    assert_eq!(warnings.len(), 2);
}

#[test]
fn normalization_rejects_an_empty_list_and_out_of_range_limits() {
    assert!(
        PersonaSettings {
            personas: Vec::new(),
            selected: None,
            pending_deletions: Vec::new(),
        }
        .normalized()
        .is_err()
    );

    let many = PersonaSettings {
        personas: (0..64)
            .map(|index| PersonaConfig::new(format!("p-{index}"), format!("人格 {index}")))
            .collect(),
        selected: None,
        pending_deletions: Vec::new(),
    };
    assert!(many.normalized().is_ok(), "人格目录不再设置固定数量上限");

    let with_limits = |context: PersonaContextLimits| {
        let mut persona = PersonaConfig::new("moon", "露娜");
        persona.context = context;
        PersonaSettings {
            personas: vec![persona],
            selected: None,
            pending_deletions: Vec::new(),
        }
    };
    assert!(
        with_limits(PersonaContextLimits {
            max_messages: Some(CONTEXT_MESSAGES_MIN - 1),
            max_tokens: None,
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_limits(PersonaContextLimits {
            max_messages: None,
            max_tokens: Some(CONTEXT_TOKENS_MAX + 1),
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_limits(PersonaContextLimits {
            max_messages: Some(CONTEXT_MESSAGES_MIN),
            max_tokens: Some(CONTEXT_TOKENS_MAX),
        })
        .normalized()
        .is_ok()
    );
}

#[test]
fn live2d_binding_must_stay_inside_the_model_directory() {
    let mut persona = PersonaConfig::new("moon", "露娜");
    persona.live2d_model = Some("../escape.model3.json".into());
    assert!(
        PersonaSettings {
            personas: vec![persona],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        }
        .normalized()
        .is_err()
    );
}

#[test]
fn unset_context_limits_resolve_to_the_documented_defaults() {
    let context = PersonaContextLimits::default();

    assert_eq!(context.effective_messages(), DEFAULT_CONTEXT_MESSAGES);
    assert_eq!(context.effective_tokens(), DEFAULT_CONTEXT_TOKENS);

    let explicit = PersonaContextLimits {
        max_messages: Some(8),
        max_tokens: Some(2_048),
    };
    assert_eq!(explicit.effective_messages(), 8);
    assert_eq!(explicit.effective_tokens(), 2_048);
}

#[test]
fn a_selection_pointing_at_a_removed_persona_is_rejected_on_publish() {
    let settings = PersonaSettings {
        personas: vec![PersonaConfig::new("moon", "露娜")],
        selected: Some("removed".to_owned()),
        pending_deletions: Vec::new(),
    };

    assert!(settings.normalized().is_err());
}
