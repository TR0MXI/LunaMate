//! 在无头 GPUI TestAppContext 中验证设置窗口实体的状态流转。
//!
//! 设置界面通过全局 `CONFIG` 写入用户配置文件，测试只覆盖不触发写入的路径：
//! 实体创建、窗口激活与释放、能力快照与空目录扫描。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{Entity, TestAppContext, VisualTestContext, prelude::*};

use crate::{
    agent::AgentMemoryAccess,
    config::{ModelExpressionCategory, ModelResourceKind},
    model::{ModelCatalog, ModelPreviewCapabilities, ModelPreviewExpression, ModelPreviewResource},
    ui::settings::{AgentOutfitAction, SettingsView},
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
    cx.add_window_view(|_window, cx| {
        SettingsView::new(catalog, AgentMemoryAccess::default(), status, cx)
    })
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
fn reactivating_the_window_restores_the_agent_draft(cx: &mut TestAppContext) {
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

    view.update(cx, |view, cx| {
        view.start_initial_scan(None, cx);
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

    view.update(cx, |view, cx| {
        view.start_initial_scan(None, cx);
        assert!(view.is_refreshing_for_test());
        // 重复请求不得叠加后台任务，否则迟到结果会覆盖较新的扫描。
        view.start_initial_scan(None, cx);
        assert!(view.is_refreshing_for_test());
    });

    wait_for(&view, cx, "重复扫描收敛", |view| {
        !view.is_refreshing_for_test()
    });
}

#[gpui::test]
async fn a_scan_discovers_models_written_after_the_view_was_created(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    fs::write(model_directory.join("侦探.exp3.json"), "{}").expect("测试服装应当可以创建");
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        assert_eq!(view.catalog_counts_for_test(), (0, 0));
        view.start_initial_scan(Some(PathBuf::from("luna/luna.model3.json")), cx);
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
        view.set_model_resource_name_for_test(
            ModelResourceKind::Expression,
            "external:侦探.exp3.json",
            "侦探套装",
        );
        view.set_model_resource_name_for_test(
            ModelResourceKind::Variant,
            "luna/luna.model3.json",
            "基础套装",
        );
        let outfits = view.available_agent_outfits();
        assert_eq!(outfits.len(), 2);
        assert!(outfits.iter().any(|outfit| outfit == "基础套装"));
        assert!(outfits.iter().any(|outfit| outfit == "侦探套装"));
        assert_eq!(
            view.resolve_agent_outfit("侦探套装"),
            Some(AgentOutfitAction::PreviewExpression(
                "external:侦探.exp3.json".to_owned()
            ))
        );
        assert!(view.resolve_agent_outfit("侦探").is_none());

        view.set_agent_outfit_tool_enabled_for_test(false);
        assert!(view.available_agent_outfits().is_empty());
        assert!(view.resolve_agent_outfit("侦探套装").is_none());
    });
}
