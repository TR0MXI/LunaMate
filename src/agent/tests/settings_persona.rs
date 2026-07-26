//! 在无头 GPUI TestAppContext 中验证人格设置编辑器的草稿与危险操作确认流程。
//!
//! 记忆的实际读写需要嵌入式数据库；这里使用不可用的记忆句柄，只覆盖草稿状态、
//! 供应商绑定映射与"删除必须先确认"的约束。真实删除路径在数据库层单独验证。

use gpui::{Entity, TestAppContext, VisualTestContext, prelude::*};

use super::ConfigGuard;
use crate::{
    agent::{
        AgentMemoryAccess, MemoryScope,
        settings::{
            PersonaSettingsDraft, PersonaSettingsView, next_persona_id_for_test,
            provider_option_index_for_test,
        },
    },
    config::{
        LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings, PersonaConfig,
        PersonaSettings,
    },
};

fn provider(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: format!("Provider {id}"),
        provider: LlmProvider::Ollama,
        model: "qwen3:8b".to_owned(),
        endpoint: Some("http://localhost:11434/".to_owned()),
        api_key: None,
        advanced: LlmAdvancedOptions::default(),
    }
}

fn persona(id: &str, bound: Option<&str>) -> PersonaConfig {
    let mut persona = PersonaConfig::new(id, format!("人格 {id}"));
    persona.model = bound.map(str::to_owned);
    persona
}

fn mount(cx: &mut TestAppContext) -> (Entity<PersonaSettingsView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let draft = PersonaSettingsDraft::current();
    // 数据库不可用的句柄让统计立即失败，避免测试依赖测试线程之外的唤醒。
    let memory = AgentMemoryAccess::default();
    cx.add_window_view(|window, cx| PersonaSettingsView::new(draft, memory, window, cx))
}

#[test]
fn new_persona_ids_skip_identifiers_already_in_use() {
    assert_eq!(
        next_persona_id_for_test(&PersonaSettings {
            personas: Vec::new(),
            selected: None,
        }),
        "persona-1"
    );

    let settings = PersonaSettings {
        personas: vec![persona("persona-1", None), persona("persona-3", None)],
        selected: None,
    };
    assert_eq!(next_persona_id_for_test(&settings), "persona-2");
}

#[test]
fn bound_provider_maps_to_the_selector_row_and_back() {
    let providers = std::sync::Arc::new(LlmSettings {
        models: vec![provider("a"), provider("b")],
        selected_model: Some("a".to_owned()),
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
    let _config = ConfigGuard::publish_all(
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None), persona("c", None)],
            selected: Some("b".to_owned()),
        },
    );
    let (view, cx) = mount(cx);

    view.update(cx, |view, _cx| {
        assert_eq!(view.persona_ids_for_test(), ["a", "b", "c"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
    });
}

#[gpui::test]
fn adding_a_persona_selects_it_and_allocates_an_unused_id(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish_all(
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("persona-1", None)],
            selected: Some("persona-1".to_owned()),
        },
    );
    let (view, cx) = mount(cx);

    cx.update_window_entity(&view, |view, window, cx| {
        view.add_persona_for_test(window, cx);

        assert_eq!(view.persona_ids_for_test(), ["persona-1", "persona-2"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
        assert_eq!(view.selected_persona_for_test(), Some("persona-2"));
    });
}

#[gpui::test]
fn switching_personas_keeps_each_bound_provider(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish_all(
        LlmSettings {
            models: vec![provider("a"), provider("b")],
            selected_model: Some("a".to_owned()),
        },
        PersonaSettings {
            personas: vec![persona("bound", Some("b")), persona("inherit", None)],
            selected: Some("bound".to_owned()),
        },
    );
    let (view, cx) = mount(cx);

    cx.update_window_entity(&view, |view, window, cx| {
        view.select_persona_for_test(1, window, cx);
        view.select_persona_for_test(0, window, cx);

        // 往返切换不得把显式绑定丢成"跟随全局默认"，也不得给未绑定人格补上绑定。
        assert_eq!(view.bound_provider_for_test(0).as_deref(), Some("b"));
        assert_eq!(view.bound_provider_for_test(1), None);
    });
}

#[gpui::test]
fn deleting_a_persona_requires_confirmation(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish_all(
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None)],
            selected: Some("a".to_owned()),
        },
    );
    let (view, cx) = mount(cx);

    view.update(cx, |view, cx| {
        view.request_delete_persona_for_test(cx);

        // 请求本身不得改动草稿；只有确认后才允许删除。
        assert_eq!(
            view.pending_confirm_for_test(),
            Some(("a".to_owned(), None))
        );
        assert_eq!(view.persona_ids_for_test(), ["a", "b"]);

        view.cancel_confirm_for_test(cx);
        assert_eq!(view.pending_confirm_for_test(), None);
        assert_eq!(view.persona_ids_for_test(), ["a", "b"]);
    });
}

#[gpui::test]
fn the_last_persona_cannot_be_deleted(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish_all(
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("only", None)],
            selected: Some("only".to_owned()),
        },
    );
    let (view, cx) = mount(cx);

    view.update(cx, |view, cx| {
        view.request_delete_persona_for_test(cx);

        // 人格 ID 同时是记忆归属键，删空会让已有记忆失去可管理入口。
        assert_eq!(view.pending_confirm_for_test(), None);
        assert_eq!(view.persona_ids_for_test(), ["only"]);
    });
}

#[gpui::test]
fn clearing_each_memory_tier_requires_confirmation(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish_all(
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
        },
    );
    let (view, cx) = mount(cx);

    for scope in [
        MemoryScope::Context,
        MemoryScope::Medium,
        MemoryScope::Long,
        MemoryScope::All,
    ] {
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
