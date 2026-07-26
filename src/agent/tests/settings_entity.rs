//! 在无头 GPUI TestAppContext 中验证 Provider 设置编辑器的草稿状态流转。
//!
//! 保存路径会通过全局 `CONFIG` 写入用户配置文件，因此这里只覆盖不触发写入的
//! 草稿编辑：模型增删、选择切换与窗口状态转移。

use gpui::{Entity, TestAppContext, VisualTestContext, prelude::*};

use crate::{
    agent::settings::{AgentSettingsDraft, AgentSettingsView},
    config::{CONFIG, LlmModelConfig, LlmProvider, LlmSettings},
};

/// `CONFIG` 是进程级全局状态，测试线程并发修改会互相覆盖已发布的模型快照。
static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ConfigGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: LlmSettings,
}

impl ConfigGuard {
    fn publish(settings: LlmSettings) -> Self {
        let guard = CONFIG_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = CONFIG.llm_settings().as_ref().clone();
        CONFIG.publish_llm_settings_for_test(settings);
        Self {
            _guard: guard,
            previous,
        }
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        CONFIG.publish_llm_settings_for_test(self.previous.clone());
    }
}

fn model(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: format!("Model {id}"),
        provider: LlmProvider::Ollama,
        model: "qwen3:8b".to_owned(),
        endpoint: Some("http://localhost:11434/".to_owned()),
        api_key: None,
    }
}

fn mount(cx: &mut TestAppContext) -> (Entity<AgentSettingsView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let draft = AgentSettingsDraft::current();
    cx.add_window_view(|window, cx| AgentSettingsView::new(draft, window, cx))
}

#[gpui::test]
fn an_empty_configuration_starts_without_an_editing_model(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings::default());
    let (view, cx) = mount(cx);

    view.update(cx, |view, _cx| {
        assert!(view.model_ids_for_test().is_empty());
        assert_eq!(view.editing_index_for_test(), None);
        assert_eq!(view.selected_model_for_test(), None);
    });
}

#[gpui::test]
fn the_persisted_selection_becomes_the_edited_model(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("a"), model("b"), model("c")],
        selected_model: Some("b".to_owned()),
        system_prompt: "persona".to_owned(),
    });
    let (view, cx) = mount(cx);

    view.update(cx, |view, _cx| {
        assert_eq!(view.model_ids_for_test(), ["a", "b", "c"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
    });
}

#[gpui::test]
fn a_missing_selection_falls_back_to_the_first_model(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("a"), model("b")],
        selected_model: Some("removed".to_owned()),
        system_prompt: String::new(),
    });
    let (view, cx) = mount(cx);

    view.update(cx, |view, _cx| {
        assert_eq!(view.editing_index_for_test(), Some(0));
    });
}

#[gpui::test]
fn adding_a_model_selects_it_and_allocates_an_unused_id(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("model-1")],
        selected_model: Some("model-1".to_owned()),
        system_prompt: String::new(),
    });
    let (view, cx) = mount(cx);

    cx.update_window_entity(&view, |view, window, cx| {
        view.add_model_for_test(window, cx);

        // 新条目跳过已占用的 model-1，并立即成为编辑目标。
        assert_eq!(view.model_ids_for_test(), ["model-1", "model-2"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
        assert_eq!(view.selected_model_for_test(), Some("model-2"));

        view.add_model_for_test(window, cx);
        assert_eq!(view.model_ids_for_test(), ["model-1", "model-2", "model-3"]);
        assert_eq!(view.editing_index_for_test(), Some(2));
    });
}

#[gpui::test]
fn deleting_a_model_moves_the_selection_to_a_remaining_entry(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("a"), model("b"), model("c")],
        selected_model: Some("b".to_owned()),
        system_prompt: String::new(),
    });
    let (view, cx) = mount(cx);

    cx.update_window_entity(&view, |view, window, cx| {
        view.delete_model_for_test(window, cx);

        assert_eq!(view.model_ids_for_test(), ["a", "c"]);
        // 删除中间项后编辑位置保持不变，指向原来的后一项。
        assert_eq!(view.editing_index_for_test(), Some(1));
        assert_eq!(view.selected_model_for_test(), Some("c"));
    });
}

#[gpui::test]
fn deleting_the_last_model_clamps_the_selection_to_the_new_end(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("a"), model("b")],
        selected_model: Some("b".to_owned()),
        system_prompt: String::new(),
    });
    let (view, cx) = mount(cx);

    cx.update_window_entity(&view, |view, window, cx| {
        view.delete_model_for_test(window, cx);

        assert_eq!(view.model_ids_for_test(), ["a"]);
        assert_eq!(view.editing_index_for_test(), Some(0));
        assert_eq!(view.selected_model_for_test(), Some("a"));
    });
}

#[gpui::test]
fn deleting_the_only_model_clears_the_editing_target(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("only")],
        selected_model: Some("only".to_owned()),
        system_prompt: String::new(),
    });
    let (view, cx) = mount(cx);

    cx.update_window_entity(&view, |view, window, cx| {
        view.delete_model_for_test(window, cx);

        assert!(view.model_ids_for_test().is_empty());
        assert_eq!(view.editing_index_for_test(), None);
        assert_eq!(view.selected_model_for_test(), None);

        // 空列表上重复删除必须是无害的空操作。
        view.delete_model_for_test(window, cx);
        assert!(view.model_ids_for_test().is_empty());
    });
}

#[gpui::test]
fn selecting_an_out_of_range_model_keeps_the_current_target(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("a"), model("b")],
        selected_model: Some("a".to_owned()),
        system_prompt: String::new(),
    });
    let (view, cx) = mount(cx);

    cx.update_window_entity(&view, |view, window, cx| {
        view.select_model_for_test(1, window, cx);
        assert_eq!(view.editing_index_for_test(), Some(1));
        assert_eq!(view.selected_model_for_test(), Some("b"));

        view.select_model_for_test(9, window, cx);
        assert_eq!(view.editing_index_for_test(), Some(1));
        assert_eq!(view.selected_model_for_test(), Some("b"));
    });
}

#[gpui::test]
fn window_state_is_transferable_across_a_settings_window_reopen(cx: &mut TestAppContext) {
    let _config = ConfigGuard::publish(LlmSettings {
        models: vec![model("a")],
        selected_model: Some("a".to_owned()),
        system_prompt: "persona".to_owned(),
    });
    let (view, cx) = mount(cx);

    let draft = cx.update_window_entity(&view, |view, window, cx| {
        view.add_model_for_test(window, cx);
        let (draft, pending) = view.take_window_state(cx);
        // 未触发保存时不应遗留写入任务。
        assert!(pending.is_empty());
        draft
    });

    // 用取回的草稿重建编辑器：新增条目必须保留，而不是回退到已发布配置。
    let (restored, cx) = cx.add_window_view(|window, cx| AgentSettingsView::new(draft, window, cx));
    restored.update(cx, |view, _cx| {
        assert_eq!(view.model_ids_for_test(), ["a", "model-1"]);
        assert_eq!(view.selected_model_for_test(), Some("model-1"));
    });
}
