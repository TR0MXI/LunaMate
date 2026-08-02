use super::*;

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
fn scan_fallback_does_not_replace_a_temporarily_missing_global_model(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    let runtime = PathBuf::from("luna/luna.model3.json");
    let missing = PathBuf::from("pending/missing.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&runtime))
        .expect("测试模型目录应当可以扫描");
    let scan_result = ModelCatalog::load(directory.path().to_path_buf(), Some(&missing))
        .expect("测试模型目录应当可以重扫");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_model_selections_for_test(missing.clone(), runtime, None);
        let scan_revision = view.model_scan_revision_for_test();
        view.finish_model_scan_for_test(
            scan_revision,
            scan_result,
            Some(missing.clone()),
            ("scan-persona", None),
            cx,
        );
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

#[gpui::test]
fn completed_scan_reconciles_the_selection_committed_after_scan_start(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["luna.model3.json", "luna-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let old = PathBuf::from("luna/luna.model3.json");
    let committed = PathBuf::from("luna/luna-alt.model3.json");
    let committed_path = directory.path().join(&committed);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&old))
        .expect("测试模型目录应当可以扫描");
    let scan_result = ModelCatalog::load(directory.path().to_path_buf(), Some(&old))
        .expect("测试扫描结果应当可以预先构造");
    assert_eq!(scan_result.selected_relative_path(), Some(old.as_path()));
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(old, "coat");
        let save_revision = view.stage_global_model_selection_for_test(committed.clone(), cx);
        // 此时扫描已捕获旧配置 A，但模型写入 B 尚未完成。
        let scan_revision = view.model_scan_revision_for_test();

        view.finish_global_model_selection_with_persona_for_test(
            save_revision,
            Some(committed.clone()),
            Some(committed.clone()),
            Ok(Some(())),
            ("scan-persona", None),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(committed.as_path())
        );

        // B 发布成功后扫描 revision 已失效，旧 A 结果无权再接管状态。
        view.finish_model_scan_for_test(
            scan_revision,
            scan_result,
            Some(committed.clone()),
            ("scan-persona", None),
            cx,
        );

        assert_eq!(
            view.global_model_selection_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(
            view.applied_global_model_selection_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(
            view.applied_persona_model_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(view.applied_persona_id_for_test(), Some("scan-persona"));
        assert_eq!(view.pending_model_runtime_for_test(), None);
        assert_eq!(
            view.emitted_model_paths_for_test(),
            vec![Some(committed_path)],
            "失效扫描不得重载或回滚到启动时捕获的 A"
        );
    });
}
