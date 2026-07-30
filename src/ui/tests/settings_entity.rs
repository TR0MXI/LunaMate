//! 在无头 GPUI TestAppContext 中验证设置窗口实体的状态流转。
//!
//! 设置界面通过全局 `CONFIG` 写入用户配置文件，测试只覆盖不触发写入的路径：
//! 实体创建、窗口激活与释放、能力快照与空目录扫描。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{Entity, TestAppContext, VisualTestContext, prelude::*};
use lunamate_agent::{Agent, AgentMemory, ChatLimits, Client, tools::OutfitOption};

use crate::{
    config::{AppLanguage, CONFIG, ConfigWriteError, ModelExpressionCategory, ModelResourceKind},
    model::{ModelCatalog, ModelPreviewCapabilities, ModelPreviewExpression, ModelPreviewResource},
    ui::settings::{AgentOutfitAction, SettingsView, SettingsWindowView},
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-settings-entity-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试模型目录应当可以创建");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn unavailable_agent() -> Arc<Agent> {
    Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::default(),
        None,
    )
}

/// 后台扫描运行在 GPUI executor 上，这里在有限时间内驱动到稳定状态。
///
/// 上限只用于避免测试永久挂起；正常情况下几毫秒即可收敛，因此取值足够宽松，
/// 使并行测试与覆盖率插桩带来的调度抖动不会造成偶发失败。
#[track_caller]
fn wait_for(
    view: &Entity<SettingsView>,
    cx: &mut VisualTestContext,
    description: &str,
    mut predicate: impl FnMut(&SettingsView) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        cx.run_until_parked();
        if view.update(cx, |view, _cx| predicate(view)) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待超时：{description}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn mount(
    cx: &mut TestAppContext,
    catalog: ModelCatalog,
    status: Option<String>,
) -> (Entity<SettingsView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    cx.add_window_view(|_window, cx| SettingsView::new(catalog, unavailable_agent(), status, cx))
}

fn mount_settings_window(
    cx: &mut TestAppContext,
    catalog: ModelCatalog,
    status: Option<String>,
) -> (
    Entity<SettingsView>,
    Entity<SettingsWindowView>,
    &mut VisualTestContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let view =
        cx.update(|cx| cx.new(|cx| SettingsView::new(catalog, unavailable_agent(), status, cx)));
    let config = view.clone();
    let (window, cx) =
        cx.add_window_view(move |window, cx| SettingsWindowView::new(config, window, cx));
    (view, window, cx)
}

#[gpui::test]
fn a_startup_warning_is_shown_as_the_initial_status(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, Some("配置已重建".to_owned()));

    view.update(cx, |view, _cx| {
        assert_eq!(view.status_for_test(), Some("配置已重建"));
        assert_eq!(view.catalog_counts_for_test(), (0, 0));
        assert!(!view.window_is_active_for_test());
        assert!(!view.is_refreshing_for_test());
    });
}

#[gpui::test]
fn a_view_without_a_startup_warning_starts_with_no_status(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, _cx| {
        assert_eq!(view.status_for_test(), None);
        assert!(view.preview_capabilities_for_test().motions().is_empty());
    });
}

#[gpui::test]
fn activating_and_deactivating_the_window_manages_input_components(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    cx.update_window_entity(&view, |view, window, cx| {
        view.activate_window(window, cx);
        assert!(view.window_is_active_for_test());
    });

    view.update(cx, |view, cx| {
        view.deactivate_window(cx);
        assert!(!view.window_is_active_for_test());
        // 未修改任何设置时不应遗留写入任务。
        assert!(view.take_pending_write_tasks(cx).is_empty());
    });
}

#[gpui::test]
fn reactivating_the_window_restores_the_provider_and_persona_drafts(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    // 关闭并重开设置窗口必须复用草稿，而不是重建后丢失未保存的编辑。
    cx.update_window_entity(&view, |view, window, cx| {
        view.activate_window(window, cx);
        view.deactivate_window(cx);
        view.activate_window(window, cx);
        assert!(view.window_is_active_for_test());
    });

    view.update(cx, |view, cx| {
        assert!(view.take_pending_write_tasks(cx).is_empty());
    });
}

#[gpui::test]
fn activating_the_window_discards_an_unpublished_appearance_language(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);
    let published = CONFIG.appearance().language;
    let unpublished = if published == AppLanguage::English {
        AppLanguage::Japanese
    } else {
        AppLanguage::English
    };

    view.update(cx, |view, _cx| {
        view.set_appearance_language_for_test(unpublished);
        assert_eq!(view.appearance_language_for_test(), unpublished);
    });
    cx.update_window_entity(&view, |view, window, cx| {
        view.activate_window(window, cx);
        assert_eq!(view.appearance_language_for_test(), published);
    });
}

#[gpui::test]
fn preview_capabilities_replace_the_previous_generation_snapshot(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_preview_capabilities(
            ModelPreviewCapabilities::new_for_test(
                vec![
                    ModelPreviewResource::new_for_test("Idle", "Idle"),
                    ModelPreviewResource::new_for_test("Tap", "Tap"),
                ],
                vec![
                    ModelPreviewExpression::new_for_test("external:侦探.exp3.json", "侦探", true),
                    ModelPreviewExpression::new_for_test("Smile", "Smile", false),
                ],
            ),
            cx,
        );
        let capabilities = view.preview_capabilities_for_test();
        assert_eq!(capabilities.motions().len(), 2);
        assert_eq!(capabilities.motions()[1].runtime_id(), "Tap");
        assert_eq!(capabilities.expressions().len(), 2);
        assert!(capabilities.expressions()[0].movable_to_outfit());

        // 模型切换后旧 generation 的能力必须被整体替换，不能残留。
        view.set_preview_capabilities(ModelPreviewCapabilities::default(), cx);
        assert!(view.preview_capabilities_for_test().motions().is_empty());
        assert!(
            view.preview_capabilities_for_test()
                .expressions()
                .is_empty()
        );
    });
}

#[gpui::test]
fn every_configuration_section_renders_without_panicking(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    // 输入组件只在窗口激活后创建；各页面都会读取它们。
    cx.update_window_entity(&view, |view, window, cx| {
        view.activate_window(window, cx);
        view.set_preview_capabilities(
            ModelPreviewCapabilities::new_for_test(
                vec![ModelPreviewResource::new_for_test("Idle", "Idle")],
                vec![
                    ModelPreviewExpression::new_for_test("external:侦探.exp3.json", "侦探", true),
                    ModelPreviewExpression::new_for_test("Smile", "Smile", false),
                ],
            ),
            cx,
        );
    });

    for section in 0..SettingsView::section_count_for_test() {
        view.update(cx, |view, cx| view.select_section_for_test(section, cx));
        cx.draw(
            gpui::Point::default(),
            gpui::size(gpui::px(980.0), gpui::px(620.0)),
            |_window, _cx| gpui::Empty,
        );
        cx.run_until_parked();
    }

    view.update(cx, |view, cx| {
        assert!(view.take_pending_write_tasks(cx).is_empty());
    });
}

#[gpui::test]
async fn scanning_an_empty_directory_reports_zero_models(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    cx.update_window_entity(&view, |view, window, cx| {
        view.start_initial_scan(None, window, cx);
        assert!(view.is_refreshing_for_test());
    });

    wait_for(&view, cx, "初始模型扫描结束", |view| {
        !view.is_refreshing_for_test()
    });

    view.update(cx, |view, _cx| {
        assert_eq!(view.catalog_counts_for_test(), (0, 0));
        assert!(view.status_for_test().is_some());
    });
}

#[gpui::test]
async fn a_second_scan_is_ignored_while_one_is_already_running(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    cx.update_window_entity(&view, |view, window, cx| {
        view.start_initial_scan(None, window, cx);
        assert!(view.is_refreshing_for_test());
        // 重复请求不得叠加后台任务，否则迟到结果会覆盖较新的扫描。
        view.start_initial_scan(None, window, cx);
        assert!(view.is_refreshing_for_test());
    });

    wait_for(&view, cx, "重复扫描收敛", |view| {
        !view.is_refreshing_for_test()
    });
}

#[gpui::test]
async fn cancelling_a_window_scan_allows_the_next_scan(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    cx.update_window_entity(&view, |view, window, cx| {
        view.refresh_models_for_test(window, cx);
        assert!(view.is_refreshing_for_test());
        view.deactivate_window(cx);
        assert!(!view.is_refreshing_for_test());

        view.refresh_models_for_test(window, cx);
        assert!(view.is_refreshing_for_test());
    });

    wait_for(&view, cx, "窗口重建后的模型扫描结束", |view| {
        !view.is_refreshing_for_test()
    });
}

#[gpui::test]
async fn closing_settings_does_not_cancel_the_desktop_startup_scan(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    cx.update_window_entity(&view, |view, window, cx| {
        view.start_initial_scan(None, window, cx);
        assert!(view.is_refreshing_for_test());
        view.deactivate_window(cx);
        assert!(view.is_refreshing_for_test());
    });

    wait_for(&view, cx, "桌宠启动模型扫描结束", |view| {
        !view.is_refreshing_for_test()
    });
}

#[gpui::test]
async fn startup_scan_refreshes_an_already_open_persona_editor(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, window_view, cx) = mount_settings_window(cx, catalog, None);

    view.update(cx, |view, _cx| {
        assert_eq!(view.persona_live2d_refresh_for_test(), (0, 0));
    });
    cx.update_window_entity(&window_view, |_window_view, window, cx| {
        view.update(cx, |view, cx| view.start_initial_scan(None, window, cx));
    });

    wait_for(
        &view,
        cx,
        "打开设置后的启动扫描与人格候选刷新",
        |view| !view.is_refreshing_for_test() && view.persona_live2d_refresh_for_test() == (1, 1),
    );
}

#[gpui::test]
async fn a_scan_discovers_models_written_after_the_view_was_created(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    fs::write(model_directory.join("侦探.exp3.json"), "{}").expect("测试服装应当可以创建");
    let relative_manifest = PathBuf::from("luna").join("luna.model3.json");
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    cx.update_window_entity(&view, |view, window, cx| {
        assert_eq!(view.catalog_counts_for_test(), (0, 0));
        view.start_initial_scan(Some(relative_manifest.clone()), window, cx);
    });

    wait_for(&view, cx, "扫描发现新模型", |view| {
        !view.is_refreshing_for_test() && view.catalog_counts_for_test() == (1, 1)
    });

    view.update(cx, |view, cx| {
        view.set_agent_outfit_tool_enabled_for_test(true);
        view.set_preview_capabilities(
            ModelPreviewCapabilities::new_for_test(
                Vec::new(),
                vec![ModelPreviewExpression::new_for_test(
                    "external:侦探.exp3.json",
                    "侦探",
                    true,
                )],
            ),
            cx,
        );
        view.set_expression_category_for_test(
            "external:侦探.exp3.json",
            ModelExpressionCategory::Outfit,
        );
        let variant_id = format!("variant:{}", relative_manifest.to_string_lossy());
        let expression_id = "expression:external:侦探.exp3.json".to_owned();
        let initial_outfits = view.available_agent_outfits();
        let initial_ids = initial_outfits
            .iter()
            .map(|outfit| outfit.id().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(initial_ids, [variant_id.clone(), expression_id.clone()]);

        view.set_model_resource_name_for_test(
            ModelResourceKind::Expression,
            "external:侦探.exp3.json",
            "同名套装",
        );
        view.set_model_resource_name_for_test(
            ModelResourceKind::Variant,
            &relative_manifest.to_string_lossy(),
            "同名套装",
        );
        assert_eq!(
            view.available_agent_outfits(),
            vec![
                OutfitOption::new(variant_id.clone(), "同名套装"),
                OutfitOption::new(expression_id.clone(), "同名套装 (2)"),
            ]
        );

        view.set_model_resource_name_for_test(
            ModelResourceKind::Expression,
            "external:侦探.exp3.json",
            "侦探套装",
        );
        view.set_model_resource_name_for_test(
            ModelResourceKind::Variant,
            &relative_manifest.to_string_lossy(),
            "基础套装",
        );
        let outfits = view.available_agent_outfits();
        assert_eq!(
            outfits,
            vec![
                OutfitOption::new(variant_id.clone(), "基础套装"),
                OutfitOption::new(expression_id.clone(), "侦探套装"),
            ]
        );
        assert_eq!(
            outfits
                .iter()
                .map(|outfit| outfit.id().to_owned())
                .collect::<Vec<_>>(),
            initial_ids,
            "本地化或重命名显示标签不得改变稳定 ID"
        );
        assert_eq!(
            view.resolve_agent_outfit(&variant_id),
            Some(AgentOutfitAction::Unchanged)
        );
        assert_eq!(
            view.resolve_agent_outfit(&expression_id),
            Some(AgentOutfitAction::PreviewExpression(
                "external:侦探.exp3.json".to_owned()
            ))
        );
        assert!(view.resolve_agent_outfit("侦探套装").is_none());

        view.set_agent_outfit_tool_enabled_for_test(false);
        assert!(view.available_agent_outfits().is_empty());
        assert!(view.resolve_agent_outfit(&expression_id).is_none());
    });
}

#[gpui::test]
async fn scan_fallback_does_not_replace_a_temporarily_missing_global_model(
    cx: &mut TestAppContext,
) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    let missing = PathBuf::from("pending/missing.model3.json");
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    cx.update_window_entity(&view, |view, window, cx| {
        view.start_initial_scan(Some(missing.clone()), window, cx);
    });
    wait_for(
        &view,
        cx,
        "缺失全局模型的回退扫描结束",
        |view| !view.is_refreshing_for_test(),
    );

    view.update(cx, |view, cx| {
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(missing.as_path())
        );
        assert!(
            view.take_pending_write_tasks(cx).is_empty(),
            "扫描回退只能影响运行时，不能静默持久化"
        );
    });
}

#[gpui::test]
fn model_page_variants_follow_global_selection_when_runtime_differs(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    for (family, manifests) in [
        (
            "global",
            ["global.model3.json", "global-alt.model3.json"].as_slice(),
        ),
        ("bound", ["bound.model3.json"].as_slice()),
    ] {
        let model_directory = directory.path().join(family);
        fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
        for manifest in manifests {
            fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
        }
    }
    let global = PathBuf::from("global/global.model3.json");
    let alternate = PathBuf::from("global/global-alt.model3.json");
    let runtime = PathBuf::from("bound/bound.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&global))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_model_selections_for_test(global.clone(), runtime.clone(), Some("coat"));
        let variants = view.global_model_variants_for_test();
        assert_eq!(variants.len(), 2);
        assert!(variants.contains(&global));
        assert!(variants.contains(&alternate));
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(runtime.as_path())
        );
        assert_eq!(view.active_outfit_for_test(), Some("coat"));

        // 全局回退模型变化不应触碰人格绑定的运行时变体或表达式服装。
        view.stage_global_model_selection_for_test(alternate, cx);
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(runtime.as_path())
        );
        assert_eq!(view.active_outfit_for_test(), Some("coat"));
    });
}

#[gpui::test]
fn unrelated_catalog_revision_does_not_hide_latest_global_save_failure(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("global");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["global.model3.json", "global-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("global/global.model3.json");
    let alternate = PathBuf::from("global/global-alt.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        let save_revision = view.stage_global_model_selection_for_test(alternate, cx);
        view.invalidate_catalog_revision_for_test();
        view.finish_global_model_selection_for_test(
            save_revision,
            Some(initial.clone()),
            Err(ConfigWriteError::InvalidValue("模拟写入失败".to_owned())),
            cx,
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert!(view.status_for_test().is_some());
    });
}

#[gpui::test]
fn an_older_global_save_failure_cannot_rollback_a_newer_selection(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("global");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in [
        "global.model3.json",
        "global-alt.model3.json",
        "global-new.model3.json",
    ] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("global/global.model3.json");
    let alternate = PathBuf::from("global/global-alt.model3.json");
    let newest = PathBuf::from("global/global-new.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        let old_revision = view.stage_global_model_selection_for_test(alternate, cx);
        view.stage_global_model_selection_for_test(newest.clone(), cx);
        view.finish_global_model_selection_for_test(
            old_revision,
            Some(initial),
            Err(ConfigWriteError::InvalidValue("旧写入失败".to_owned())),
            cx,
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(newest.as_path())
        );
        assert_eq!(view.status_for_test(), None);
    });
}

#[gpui::test]
fn persona_live2d_binding_precedes_global_and_missing_binding_falls_back(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    for family in ["luna", "mate"] {
        let model_directory = directory.path().join(family);
        fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
        fs::write(model_directory.join(format!("{family}.model3.json")), "{}")
            .expect("测试模型清单应当可以创建");
    }
    let global = PathBuf::from("mate/mate.model3.json");
    let bound = PathBuf::from("luna/luna.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&global))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, _| {
        let (relative, path, warning) =
            view.resolve_persona_live2d_model_for_test(Some(&bound), Some(&global));
        assert_eq!(relative, Some(bound.clone()));
        assert_eq!(path, Some(directory.path().join(&bound)));
        assert!(!warning);

        let missing = PathBuf::from("missing/missing.model3.json");
        let (relative, path, warning) =
            view.resolve_persona_live2d_model_for_test(Some(&missing), Some(&global));
        assert_eq!(relative, Some(global.clone()));
        assert_eq!(path, Some(directory.path().join(&global)));
        assert!(warning);
    });
}

#[gpui::test]
fn bound_persona_outfit_change_does_not_schedule_a_global_model_write(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["luna.model3.json", "luna-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("luna/luna.model3.json");
    let alternate = PathBuf::from("luna/luna-alt.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        let loaded = view
            .commit_agent_outfit_with_binding_for_test(
                AgentOutfitAction::LoadVariant(alternate.clone()),
                true,
                cx,
            )
            .expect("人格绑定下的换装应成功")
            .expect("清单换装应返回模型路径");
        assert_eq!(loaded, directory.path().join(alternate));
        assert!(
            view.take_pending_write_tasks(cx).is_empty(),
            "人格运行时换装不得派生全局模型配置写入"
        );
    });
}

#[gpui::test]
fn unchanged_persona_model_reapply_preserves_the_active_expression_outfit(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    let relative = PathBuf::from("luna/luna.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&relative))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(relative, "coat");
        view.reapply_persona_model_for_test(cx);
        assert_eq!(view.active_outfit_for_test(), Some("coat"));
    });
}

#[gpui::test]
async fn rescanning_the_same_runtime_model_clears_the_expression_outfit_marker(
    cx: &mut TestAppContext,
) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    let relative = PathBuf::from("luna/luna.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&relative))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, _cx| {
        view.set_model_selections_for_test(relative.clone(), relative.clone(), Some("coat"));
    });
    cx.update_window_entity(&view, |view, window, cx| {
        view.refresh_models_for_test(window, cx);
    });
    wait_for(&view, cx, "同路径模型重扫完成", |view| {
        !view.is_refreshing_for_test()
    });

    view.update(cx, |view, _cx| {
        assert_eq!(view.active_outfit_for_test(), None);
    });
}
