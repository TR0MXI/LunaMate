//! 在无头 GPUI TestAppContext 中验证 Agent 人格设置编辑器的草稿与危险操作确认流程。
//!
//! 记忆的实际读写需要嵌入式数据库；这里使用不可用的记忆句柄，只覆盖草稿状态、
//! 供应商绑定映射与"删除必须先确认"的约束。真实删除路径在数据库层单独验证。

use gpui::{
    Entity, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, prelude::*, px,
};
use lunamate_agent::AgentMemory;
use lunamate_agent::config::{
    LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings, ModelKind, ModelProvider,
    PersonaConfig, PersonaContextLimits, PersonaSettings,
};
use lunamate_agent::{
    ChatRole,
    memory::{ContextMessage, ContextUsage, LiveContextUsage},
};
use std::{path::PathBuf, sync::Arc};

use crate::ui::settings::{
    MemoryScope, PersonaSettingsDraft, PersonaSettingsView, next_persona_id_for_test,
    provider_option_index_for_test, tts_model_option_index_for_test,
};

fn provider(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: format!("Provider {id}"),
        kind: ModelKind::ChatCompletions,
        provider: ModelProvider::Genai(LlmProvider::Ollama),
        model: "qwen3:8b".to_owned(),
        endpoint: Some("http://localhost:11434/".to_owned()),
        api_key: None,
        app_id: None,
        voice: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    }
}

fn tts_model(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: format!("TTS {id}"),
        kind: ModelKind::SpeechSynthesis,
        provider: ModelProvider::Genai(LlmProvider::OpenAI),
        model: "gpt-4o-mini-tts".to_owned(),
        endpoint: None,
        api_key: Some("test-key".to_owned()),
        app_id: None,
        voice: Some("alloy".to_owned()),
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    }
}

fn persona(id: &str, bound: Option<&str>) -> PersonaConfig {
    let mut persona = PersonaConfig::new(id, format!("人格 {id}"));
    persona.model = bound.map(str::to_owned);
    persona
}

#[test]
fn tts_binding_rows_ignore_chat_models_and_keep_zero_as_disabled() {
    let providers = Arc::new(LlmSettings {
        models: vec![provider("chat"), tts_model("voice-a"), tts_model("voice-b")],
        selected_model: Some("chat".to_owned()),
        selected_transcription_model: None,
    });

    assert_eq!(tts_model_option_index_for_test(&providers, None), 0);
    assert_eq!(
        tts_model_option_index_for_test(&providers, Some("voice-a")),
        1
    );
    assert_eq!(
        tts_model_option_index_for_test(&providers, Some("voice-b")),
        2
    );
    assert_eq!(tts_model_option_index_for_test(&providers, Some("chat")), 0);
}

fn mount(
    cx: &mut TestAppContext,
    providers: LlmSettings,
    personas: PersonaSettings,
) -> (Entity<PersonaSettingsView>, &mut VisualTestContext) {
    mount_with_models(cx, providers, personas, Vec::new())
}

fn mount_with_models(
    cx: &mut TestAppContext,
    providers: LlmSettings,
    personas: PersonaSettings,
    models: Vec<(String, PathBuf)>,
) -> (Entity<PersonaSettingsView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let memory = AgentMemory::unavailable();
    let draft = PersonaSettingsDraft::from_settings_for_test(personas);
    // 数据库不可用的句柄让统计立即失败，避免测试依赖测试线程之外的唤醒。
    cx.add_window_view(|window, cx| {
        PersonaSettingsView::new_for_test(draft, memory, Arc::new(providers), models, window, cx)
    })
}

#[test]
fn new_persona_ids_skip_identifiers_already_in_use() {
    assert_eq!(
        next_persona_id_for_test(&PersonaSettings {
            personas: Vec::new(),
            selected: None,
            pending_deletions: Vec::new(),
        }),
        "persona-1"
    );

    let settings = PersonaSettings {
        personas: vec![persona("persona-1", None), persona("persona-3", None)],
        selected: None,
        pending_deletions: vec!["persona-2".to_owned()],
    };
    assert_eq!(next_persona_id_for_test(&settings), "persona-4");
}

#[test]
fn live_context_revisions_are_scoped_to_the_active_persona() {
    let live = LiveContextUsage::default();
    assert_eq!(live.revision_for("moon"), None);

    live.publish("moon", ContextUsage::default(), Vec::new());
    let moon_revision = live
        .revision_for("moon")
        .expect("刚发布的人格应有实时 revision");
    assert!(moon_revision > 0);
    assert_eq!(live.revision_for("study"), None);

    live.publish("study", ContextUsage::default(), Vec::new());
    assert_eq!(live.revision_for("moon"), None);
    assert!(
        live.revision_for("study")
            .is_some_and(|revision| revision > moon_revision)
    );
}

#[test]
fn bound_provider_maps_to_the_selector_row_and_back() {
    let providers = std::sync::Arc::new(LlmSettings {
        models: vec![provider("a"), provider("b")],
        selected_model: Some("a".to_owned()),
        selected_transcription_model: None,
    });

    // 第一项固定表示"跟随全局默认"，因此供应商条目从 1 开始。
    assert_eq!(provider_option_index_for_test(&providers, None), 0);
    assert_eq!(provider_option_index_for_test(&providers, Some("a")), 1);
    assert_eq!(provider_option_index_for_test(&providers, Some("b")), 2);
    // 绑定的供应商被删除后回退到"跟随全局默认"，而不是指向错误的条目。
    assert_eq!(provider_option_index_for_test(&providers, Some("gone")), 0);
}

#[gpui::test]
fn the_persisted_selection_becomes_the_edited_persona(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None), persona("c", None)],
            selected: Some("b".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(cx, |view, _cx| {
        assert_eq!(view.persona_ids_for_test(), ["a", "b", "c"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
    });
}

#[gpui::test]
fn adding_a_persona_selects_it_and_allocates_an_unused_id(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("persona-1", None)],
            selected: Some("persona-1".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.add_persona_for_test(window, cx);

        assert_eq!(view.persona_ids_for_test(), ["persona-1", "persona-2"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
        assert_eq!(view.selected_persona_for_test(), Some("persona-2"));
    });
}

#[gpui::test]
fn switching_personas_keeps_each_bound_provider(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings {
            models: vec![provider("a"), provider("b")],
            selected_model: Some("a".to_owned()),
            selected_transcription_model: None,
        },
        PersonaSettings {
            personas: vec![persona("bound", Some("b")), persona("inherit", None)],
            selected: Some("bound".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.select_persona_for_test(1, window, cx);
        view.select_persona_for_test(0, window, cx);

        // 往返切换不得把显式绑定丢成"跟随全局默认"，也不得给未绑定人格补上绑定。
        assert_eq!(view.bound_provider_for_test(0).as_deref(), Some("b"));
        assert_eq!(view.bound_provider_for_test(1), None);
    });
}

#[gpui::test]
fn switching_personas_preserves_a_missing_live2d_binding(cx: &mut TestAppContext) {
    let mut bound = persona("bound", None);
    bound.live2d_model = Some(PathBuf::from("missing/missing.model3.json"));
    let (view, cx) = mount_with_models(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![bound, persona("inherit", None)],
            selected: Some("bound".to_owned()),
            pending_deletions: Vec::new(),
        },
        vec![(
            "可用模型".to_owned(),
            PathBuf::from("available/model.model3.json"),
        )],
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.select_persona_for_test(1, window, cx);
        view.select_persona_for_test(0, window, cx);
        assert_eq!(
            view.bound_live2d_for_test(0),
            Some(PathBuf::from("missing/missing.model3.json"))
        );
        assert_eq!(view.bound_live2d_for_test(1), None);
    });
}

#[gpui::test]
fn persona_editor_renders_five_tabs_and_row_delete_buttons(cx: &mut TestAppContext) {
    let (_view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None)],
            selected: Some("a".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.run_until_parked();

    let tabs = cx.debug_bounds("persona-tabs").expect("人格页签行应已渲染");
    for (index, selector) in [
        "persona-tab-0",
        "persona-tab-1",
        "persona-tab-2",
        "persona-tab-3",
        "persona-tab-4",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "第 {index} 个人格页签应已渲染"
        );
    }
    let first_tab = cx
        .debug_bounds("persona-tab-0")
        .expect("第一个人格页签应已渲染");
    let last_tab = cx
        .debug_bounds("persona-tab-4")
        .expect("最后一个人格页签应已渲染");
    assert_eq!(first_tab.origin.x, tabs.origin.x);
    assert_eq!(
        last_tab.origin.x + last_tab.size.width,
        tabs.origin.x + tabs.size.width,
        "人格页签应占满整行，右侧不得留空"
    );
    assert!(cx.debug_bounds("persona-delete-0").is_some());
    assert!(cx.debug_bounds("persona-delete-1").is_some());
}

#[gpui::test]
fn persona_list_starts_at_the_top_and_places_its_count_in_the_footer(cx: &mut TestAppContext) {
    let (_view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None)],
            selected: Some("a".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.run_until_parked();

    let sidebar = cx
        .debug_bounds("persona-sidebar")
        .expect("人格侧栏应已渲染");
    let list = cx.debug_bounds("persona-list").expect("人格列表应已渲染");
    let count = cx.debug_bounds("persona-count").expect("人格数量应已渲染");
    let add = cx
        .debug_bounds("persona-add")
        .expect("添加人格按钮应已渲染");

    assert_eq!(list.origin.y, sidebar.origin.y);
    assert!(count.origin.y >= list.origin.y + list.size.height);
    assert!(count.origin.x < add.origin.x);
    assert_eq!(
        count.origin.y + count.size.height / 2.0,
        add.origin.y + add.size.height / 2.0
    );
}

#[gpui::test]
fn context_limits_are_empty_inline_inputs_in_a_compact_stats_row(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(cx, |view, cx| {
        assert_eq!(view.context_limit_inputs_for_test(cx), ["", ""]);
        assert_eq!(
            view.context_limits_for_test(cx),
            PersonaContextLimits::default()
        );
        view.show_context_for_test(cx);
    });
    cx.run_until_parked();

    let stats = cx
        .debug_bounds("context-stats")
        .expect("上下文统计行应已渲染");
    let messages = cx
        .debug_bounds("context-stat-messages")
        .expect("消息统计应已渲染");
    let tokens = cx
        .debug_bounds("context-stat-tokens")
        .expect("Token 统计应已渲染");
    let message_limit = cx
        .debug_bounds("context-limit-messages")
        .expect("消息上限输入应位于统计行");
    let token_limit = cx
        .debug_bounds("context-limit-tokens")
        .expect("Token 上限输入应位于统计行");
    let messages_view = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文消息列表应已渲染");

    assert_eq!(stats.size.height, px(58.0));
    assert!(messages.origin.x < message_limit.origin.x);
    assert!(message_limit.origin.x + message_limit.size.width <= tokens.origin.x);
    assert!(
        tokens.origin.x >= stats.origin.x + stats.size.width / 3.0,
        "Token 统计应展开到统计行中部，避免右侧集中留白"
    );
    assert!(tokens.origin.x < token_limit.origin.x);
    assert!(token_limit.origin.x + token_limit.size.width <= stats.origin.x + stats.size.width);
    assert_eq!(
        messages_view.origin.y,
        stats.origin.y + stats.size.height + px(12.0),
        "消息列表与统计行之间不应残留旧上限控制区"
    );
}

#[gpui::test]
fn persona_editor_accepts_more_than_thirty_two_entries(cx: &mut TestAppContext) {
    let personas = (0..40)
        .map(|index| persona(&format!("p-{index}"), None))
        .collect::<Vec<_>>();
    let (view, _cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas,
            selected: Some("p-0".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(_cx, |view, _| {
        assert_eq!(view.persona_ids_for_test().len(), 40);
    });
}

#[gpui::test]
fn deleting_a_persona_requires_confirmation(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None)],
            selected: Some("a".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(cx, |view, cx| {
        view.request_delete_persona_for_test(cx);

        // 请求本身不得改动草稿；只有确认后才允许删除。
        assert_eq!(
            view.pending_confirm_for_test(),
            Some(("a".to_owned(), None))
        );
        assert_eq!(
            view.confirm_message_for_test(),
            Some(
                rust_i18n::t!("persona.confirm_delete_persona_message", persona = "人格 a")
                    .to_string()
            )
        );
        assert_eq!(view.persona_ids_for_test(), ["a", "b"]);

        view.cancel_confirm_for_test(cx);
        assert_eq!(view.pending_confirm_for_test(), None);
        assert_eq!(view.persona_ids_for_test(), ["a", "b"]);
    });
}

#[gpui::test]
fn deleting_a_persona_reserves_its_id_until_memory_cleanup_finishes(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("persona-1", None), persona("other", None)],
            selected: Some("persona-1".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.delete_persona_for_test("persona-1", window, cx);
        assert_eq!(view.persona_ids_for_test(), ["other"]);
        assert_eq!(view.pending_deletions_for_test(), ["persona-1"]);

        view.add_persona_for_test(window, cx);
        assert_eq!(view.persona_ids_for_test(), ["other", "persona-2"]);
    });
}

#[gpui::test]
fn deleting_one_context_message_describes_only_that_message(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(cx, |view, cx| {
        view.request_delete_context_confirmation_for_test(7, cx);
        assert_eq!(
            view.confirm_message_for_test(),
            Some(
                rust_i18n::t!(
                    "persona.confirm_delete_context_messages",
                    persona = "人格 moon",
                    count = 1
                )
                .to_string()
            )
        );
    });
}

#[gpui::test]
fn a_non_empty_context_builds_auto_growing_message_editors(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.load_context_messages_for_test(
            vec![ContextMessage {
                id: 1,
                role: ChatRole::User,
                content: "第一行\n第二行".to_owned(),
                tokens: 8,
                fixed_tokens: 4,
                trace: None,
            }],
            window,
            cx,
        );
        assert_eq!(view.context_editor_count_for_test(), 1);
        assert_eq!(view.context_message_ids_for_test(), [1]);
    });
}

#[gpui::test]
fn selected_context_messages_share_one_confirmation(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.load_context_messages_for_test(
            vec![
                ContextMessage {
                    id: 1,
                    role: ChatRole::User,
                    content: "问题一".to_owned(),
                    tokens: 7,
                    fixed_tokens: 4,
                    trace: None,
                },
                ContextMessage {
                    id: 2,
                    role: ChatRole::Assistant,
                    content: "回答一".to_owned(),
                    tokens: 7,
                    fixed_tokens: 4,
                    trace: None,
                },
                ContextMessage {
                    id: 3,
                    role: ChatRole::User,
                    content: "问题二".to_owned(),
                    tokens: 7,
                    fixed_tokens: 4,
                    trace: None,
                },
            ],
            window,
            cx,
        );
        view.toggle_context_message_selected_for_test(1, cx);
        view.toggle_context_message_selected_for_test(3, cx);
        assert_eq!(view.selected_context_messages_for_test(), 2);
        view.copy_selected_context_messages_for_test(cx);

        view.request_delete_selected_context_messages_for_test(cx);
        assert_eq!(
            view.confirm_message_for_test(),
            Some(
                rust_i18n::t!(
                    "persona.confirm_delete_context_messages",
                    persona = "人格 moon",
                    count = 2
                )
                .to_string()
            )
        );
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("问题一\n问题二".to_owned())
    );
}

#[gpui::test]
fn context_bubbles_use_whole_row_selection_without_inline_controls(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            vec![
                ContextMessage {
                    id: 1,
                    role: ChatRole::User,
                    content: "一行短消息".to_owned(),
                    tokens: 10,
                    fixed_tokens: 4,
                    trace: None,
                },
                ContextMessage {
                    id: 2,
                    role: ChatRole::Assistant,
                    content: "第一行\n第二行\n第三行".to_owned(),
                    tokens: 16,
                    fixed_tokens: 4,
                    trace: None,
                },
            ],
            window,
            cx,
        );
        assert_eq!(view.context_message_ids_for_test(), [1, 2]);
    });
    cx.run_until_parked();

    let card = cx
        .debug_bounds("context-card-1")
        .expect("第一条上下文卡片应已渲染");
    assert!(cx.debug_bounds("context-select-1").is_none());
    assert!(cx.debug_bounds("context-delete-1").is_none());

    let center = point(
        card.origin.x + card.size.width / 2.0,
        card.origin.y + card.size.height / 2.0,
    );
    cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 1);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("delete-selected-context-messages")
            .is_some(),
        "选择消息后应显示仅删除选择项的入口"
    );

    cx.simulate_mouse_down(center, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    let menu = cx
        .debug_bounds("context-action-menu")
        .expect("右键图标操作栏应已渲染");
    let edit = cx
        .debug_bounds("context-action-edit")
        .expect("编辑图标应已渲染");
    let copy = cx
        .debug_bounds("context-action-copy")
        .expect("复制图标应已渲染");
    let delete = cx
        .debug_bounds("context-action-delete")
        .expect("删除图标应已渲染");
    assert_eq!(edit.origin.y, copy.origin.y);
    assert_eq!(copy.origin.y, delete.origin.y);
    assert!(edit.origin.x < copy.origin.x && copy.origin.x < delete.origin.x);
    assert!(delete.origin.x + delete.size.width <= menu.origin.x + menu.size.width);

    let copy_center = point(
        copy.origin.x + copy.size.width / 2.0,
        copy.origin.y + copy.size.height / 2.0,
    );
    cx.simulate_mouse_down(copy_center, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(copy_center, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("一行短消息".to_owned())
    );
    assert!(cx.debug_bounds("context-action-menu").is_none());
}

#[gpui::test]
fn dragging_from_a_message_selects_a_contiguous_message_range(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=3)
                .map(|id| ContextMessage {
                    id,
                    role: if id % 2 == 0 {
                        ChatRole::Assistant
                    } else {
                        ChatRole::User
                    },
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let first = cx
        .debug_bounds("context-card-1")
        .expect("第一条消息应已渲染");
    let third = cx
        .debug_bounds("context-card-3")
        .expect("第三条消息应已渲染");
    let start = point(
        first.origin.x + first.size.width / 2.0,
        first.origin.y + first.size.height / 2.0,
    );
    let cursor = point(
        third.origin.x + third.size.width / 2.0,
        third.origin.y + third.size.height / 2.0,
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    assert!(
        cx.debug_bounds("context-selection-box").is_some(),
        "连续框选时应显示选择区域"
    );
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 3);
    });
    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert!(!view.context_selection_active_for_test());
        assert!(!view.context_selection_auto_scroll_scheduled_for_test());
    });
}

#[gpui::test]
fn dragging_from_viewport_padding_selects_messages(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=3)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let scroll = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let third = cx
        .debug_bounds("context-card-3")
        .expect("第三条消息应已渲染");
    let start = point(scroll.origin.x + px(4.0), third.origin.y - px(80.0));
    let cursor = point(
        third.origin.x + third.size.width / 2.0,
        third.origin.y + third.size.height / 2.0,
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 3);
    });
    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
}

#[gpui::test]
fn scrolling_during_marquee_selection_keeps_the_box_and_drag_alive(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=40)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let viewport = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let last = cx
        .debug_bounds("context-card-40")
        .expect("最后一条消息应已渲染");
    let start = point(
        last.origin.x + last.size.width / 2.0,
        last.origin.y + last.size.height / 2.0,
    );
    let cursor = point(start.x - px(120.0), viewport.origin.y + px(80.0));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    let selection_before = cx
        .debug_bounds("context-selection-box")
        .expect("滚动前应显示框选区域");
    let offset_before = view.update(cx, |view, _| {
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
        (
            view.context_scroll_for_test().0,
            view.selected_context_messages_for_test(),
        )
    });

    cx.simulate_event(ScrollWheelEvent {
        position: cursor,
        delta: ScrollDelta::Pixels(point(px(0.0), px(180.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    let selection_after_scroll = cx
        .debug_bounds("context-selection-box")
        .expect("滚动后框选区域不应消失");
    assert_eq!(selection_after_scroll.origin, selection_before.origin);
    assert_eq!(
        selection_after_scroll.size.width,
        selection_before.size.width
    );
    assert!(selection_after_scroll.size.height > selection_before.size.height);
    view.update(cx, |view, _| {
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() >= offset_before.1);
        assert_ne!(view.context_scroll_for_test().0, offset_before.0);
    });

    // 模拟部分平台在滚动后发出的无按键状态过渡移动事件。
    cx.simulate_mouse_move(
        point(cursor.x + px(40.0), cursor.y + px(20.0)),
        None,
        Modifiers::none(),
    );
    assert_eq!(
        cx.debug_bounds("context-selection-box"),
        Some(selection_after_scroll)
    );
    view.update(cx, |view, _| {
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
    });

    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert!(!view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
    });
}

#[gpui::test]
fn marquee_selection_auto_scrolls_from_the_viewport_edge(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=40)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let viewport = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let last = cx
        .debug_bounds("context-card-40")
        .expect("最后一条消息应已渲染");
    let start = point(
        last.origin.x + last.size.width / 2.0,
        last.origin.y + last.size.height / 2.0,
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    let cursor = point(start.x, viewport.origin.y + px(8.0));
    let offset_before = view.update(cx, |view, _| view.context_scroll_for_test().0);
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, cx| {
        assert!(view.context_selection_auto_scroll_scheduled_for_test());
        assert!(view.advance_context_selection_auto_scroll_for_test(cx));
        assert_ne!(view.context_scroll_for_test().0, offset_before);
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
    });
    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert!(!view.context_selection_active_for_test());
        assert!(!view.context_selection_auto_scroll_scheduled_for_test());
    });
}

#[gpui::test]
fn context_keyboard_shortcuts_select_delete_and_cancel_edit(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=3)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let first = cx
        .debug_bounds("context-card-1")
        .expect("第一条消息应已渲染");
    let first_center = point(
        first.origin.x + first.size.width / 2.0,
        first.origin.y + first.size.height / 2.0,
    );
    cx.simulate_mouse_down(first_center, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(first_center, MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes("alt-a");
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 3);
    });
    cx.simulate_keystrokes("delete");
    view.update(cx, |view, cx| {
        assert!(view.pending_confirm_for_test().is_some());
        view.cancel_confirm_for_test(cx);
    });

    cx.update_window_entity(&view, |view, window, cx| {
        view.begin_context_message_edit_for_test(1, window, cx);
        view.set_context_message_content_for_test(1, "未提交修改", window, cx);
    });
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.context_message_content_for_test(1, cx).as_deref(),
            Some("消息 1")
        );
    });
}

#[gpui::test]
fn context_message_editor_uses_the_available_bubble_width(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            vec![ContextMessage {
                id: 1,
                role: ChatRole::Assistant,
                content: "需要编辑的长消息".to_owned(),
                tokens: 8,
                fixed_tokens: 4,
                trace: None,
            }],
            window,
            cx,
        );
    });
    cx.run_until_parked();
    cx.update_window_entity(&view, |view, window, cx| {
        view.begin_context_message_edit_for_test(1, window, cx);
    });
    cx.run_until_parked();

    let bubble = cx
        .debug_bounds("context-bubble-1")
        .expect("编辑气泡应已渲染");
    assert!(bubble.size.width >= px(300.0));
}

#[gpui::test]
fn context_message_list_fills_the_page_and_initially_scrolls_to_bottom(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=30)
                .map(|id| ContextMessage {
                    id,
                    role: if id % 2 == 0 {
                        ChatRole::Assistant
                    } else {
                        ChatRole::User
                    },
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    view.update(cx, |view, _| {
        let (offset, max_offset) = view.context_scroll_for_test();
        assert!(max_offset > px(0.0));
        assert_eq!(offset, -max_offset);
    });
    let scroll = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let page = cx
        .debug_bounds("persona-context-page")
        .expect("上下文页面应已渲染");
    assert_eq!(
        scroll.origin.y + scroll.size.height,
        page.origin.y + page.size.height - px(24.0),
        "消息滚动区应填满页面剩余高度"
    );
}

#[gpui::test]
fn the_last_persona_cannot_be_deleted(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("only", None)],
            selected: Some("only".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(cx, |view, cx| {
        view.request_delete_persona_for_test(cx);

        // 人格 ID 同时是记忆归属键，删空会让已有记忆失去可管理入口。
        assert_eq!(view.pending_confirm_for_test(), None);
        assert_eq!(view.persona_ids_for_test(), ["only"]);
    });
}

#[gpui::test]
fn clearing_context_and_all_memory_requires_confirmation(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    for scope in [MemoryScope::Context, MemoryScope::All] {
        view.update(cx, |view, cx| {
            view.request_clear_memory_for_test(scope, cx);
            assert_eq!(
                view.pending_confirm_for_test(),
                Some(("moon".to_owned(), Some(scope))),
                "{scope:?} 必须先进入二次确认"
            );
            view.cancel_confirm_for_test(cx);
            assert_eq!(view.pending_confirm_for_test(), None);
        });
    }
}
