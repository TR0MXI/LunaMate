//! 验证人格配置的解析、校验、旧版提示词迁移与写回往返。

use toml_edit::DocumentMut;

use crate::config::{
    CONTEXT_KIB_MAX, CONTEXT_MESSAGES_MIN, DEFAULT_CONTEXT_KIB, DEFAULT_CONTEXT_MESSAGES,
    DEFAULT_PERSONA_ID, MAX_PERSONAS, PersonaConfig, PersonaContextLimits, PersonaSettings,
    parse_persona_settings, write_persona_settings,
};

fn document(source: &str) -> DocumentMut {
    source.parse::<DocumentMut>().expect("测试配置应当可以解析")
}

#[test]
fn a_configuration_without_personas_migrates_the_legacy_system_prompt() {
    let document = document(
        r#"
[llm]
system_prompt = """你是 LunaMate。
回答保持简洁。"""
"#,
    );
    let mut warnings = Vec::new();

    let settings = parse_persona_settings(&document, &mut warnings);

    assert!(warnings.is_empty());
    assert_eq!(settings.personas.len(), 1);
    let persona = settings.active().expect("迁移后必须有一个可用人格");
    assert_eq!(persona.id, DEFAULT_PERSONA_ID);
    assert!(persona.system_prompt.contains("回答保持简洁"));
    // 未绑定供应商的人格回退到全局默认选择。
    assert_eq!(persona.model, None);
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
fn stored_personas_round_trip_through_the_document() {
    let mut source = document(
        r#"
[persona]
selected = "moon"

[[persona.list]]
id = "moon"
name = "露娜"
system_prompt = "保持简洁"
model = "cloud"
max_context_messages = 16
max_context_kib = 8
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
    assert_eq!(
        moon.context,
        PersonaContextLimits {
            max_messages: Some(16),
            max_kib: Some(8),
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
    // 提示词已迁移到人格条目，旧键必须在写回时移除以避免两个来源互相矛盾。
    assert!(!saved.contains("system_prompt = \"保持简洁\"\n\n[llm]"));
}

#[test]
fn writing_personas_removes_the_legacy_global_prompt() {
    let mut source = document("[llm]\nsystem_prompt = \"旧提示词\"\n");
    let settings = parse_persona_settings(&source, &mut Vec::new());

    write_persona_settings(&mut source, &settings);

    // 提示词内容迁移到了人格条目，而 `llm` 表下的旧键必须被移除，避免同一份配置
    // 出现两个互相矛盾的来源。
    assert!(
        source
            .get("llm")
            .and_then(|llm| llm.get("system_prompt"))
            .is_none(),
        "保存内容：{source}"
    );
    let saved = source.to_string();
    assert!(saved.contains("[persona]"), "保存内容：{saved}");
    assert!(saved.contains("[[persona.list]]"));
    assert_eq!(
        parse_persona_settings(&source, &mut Vec::new())
            .active()
            .map(|persona| persona.system_prompt.clone()),
        Some("旧提示词".to_owned())
    );
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
        }
        .normalized()
        .is_err()
    );

    let mut too_many = PersonaSettings {
        personas: (0..=MAX_PERSONAS)
            .map(|index| PersonaConfig::new(format!("p-{index}"), format!("人格 {index}")))
            .collect(),
        selected: None,
    };
    assert!(too_many.clone().normalized().is_err());
    too_many.personas.pop();
    assert!(too_many.normalized().is_ok());

    let with_limits = |context: PersonaContextLimits| {
        let mut persona = PersonaConfig::new("moon", "露娜");
        persona.context = context;
        PersonaSettings {
            personas: vec![persona],
            selected: None,
        }
    };
    assert!(
        with_limits(PersonaContextLimits {
            max_messages: Some(CONTEXT_MESSAGES_MIN - 1),
            max_kib: None,
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_limits(PersonaContextLimits {
            max_messages: None,
            max_kib: Some(CONTEXT_KIB_MAX + 1),
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_limits(PersonaContextLimits {
            max_messages: Some(CONTEXT_MESSAGES_MIN),
            max_kib: Some(CONTEXT_KIB_MAX),
        })
        .normalized()
        .is_ok()
    );
}

#[test]
fn unset_context_limits_resolve_to_the_documented_defaults() {
    let context = PersonaContextLimits::default();

    assert_eq!(context.effective_messages(), DEFAULT_CONTEXT_MESSAGES);
    assert_eq!(
        context.effective_bytes(),
        usize::try_from(DEFAULT_CONTEXT_KIB).expect("默认上限必须可以转换") * 1024
    );

    let explicit = PersonaContextLimits {
        max_messages: Some(8),
        max_kib: Some(2),
    };
    assert_eq!(explicit.effective_messages(), 8);
    assert_eq!(explicit.effective_bytes(), 2 * 1024);
}

#[test]
fn a_selection_pointing_at_a_removed_persona_is_rejected_on_publish() {
    let settings = PersonaSettings {
        personas: vec![PersonaConfig::new("moon", "露娜")],
        selected: Some("removed".to_owned()),
    };

    assert!(settings.normalized().is_err());
}
