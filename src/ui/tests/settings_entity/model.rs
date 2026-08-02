use super::*;

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
fn explicit_persona_binding_adopts_a_matching_preapplied_runtime_without_reloading(
    cx: &mut TestAppContext,
) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["luna.model3.json", "luna-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("luna/luna.model3.json");
    let preapplied = PathBuf::from("luna/luna-alt.model3.json");
    let preapplied_path = directory.path().join(&preapplied);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial.clone(), "coat");
        let (save_revision, immediate_path) =
            view.stage_preapplied_model_selection_for_test(preapplied.clone(), cx);
        assert_eq!(
            view.pending_model_runtime_for_test(),
            Some(preapplied.as_path())
        );

        view.apply_published_persona_model_for_test(
            initial.clone(),
            "bound-persona",
            Some(&preapplied),
            cx,
        );

        let mut load_paths = vec![immediate_path];
        load_paths.extend(view.emitted_model_paths_for_test().into_iter().flatten());
        assert_eq!(load_paths, vec![preapplied_path]);
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::Model),
            0
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(preapplied.as_path())
        );
        assert_eq!(
            view.applied_persona_model_for_test(),
            Some(preapplied.as_path())
        );
        assert_eq!(view.applied_persona_id_for_test(), Some("bound-persona"));
        assert_eq!(view.active_outfit_for_test(), None);
        assert_eq!(view.pending_model_runtime_for_test(), None);

        view.finish_global_model_selection_with_persona_for_test(
            save_revision,
            Some(preapplied.clone()),
            Some(initial),
            Err(ConfigWriteError::PersistenceUnavailable),
            ("bound-persona", Some(&preapplied)),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(preapplied.as_path()),
            "人格接管后，旧全局写入失败不得回滚其 runtime"
        );
        assert_eq!(view.emitted_model_paths_for_test(), Vec::new());
    });
}

#[gpui::test]
fn missing_persona_binding_fallback_adopts_a_matching_preapplied_runtime_without_reloading(
    cx: &mut TestAppContext,
) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["luna.model3.json", "luna-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("luna/luna.model3.json");
    let preapplied = PathBuf::from("luna/luna-alt.model3.json");
    let missing = PathBuf::from("missing/missing.model3.json");
    let preapplied_path = directory.path().join(&preapplied);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial, "coat");
        let (save_revision, immediate_path) =
            view.stage_preapplied_model_selection_for_test(preapplied.clone(), cx);

        view.apply_published_persona_model_for_test(
            preapplied.clone(),
            "missing-persona",
            Some(&missing),
            cx,
        );

        let mut load_paths = vec![immediate_path];
        load_paths.extend(view.emitted_model_paths_for_test().into_iter().flatten());
        assert_eq!(load_paths, vec![preapplied_path]);
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::Model),
            0
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(preapplied.as_path())
        );
        assert_eq!(
            view.applied_persona_model_for_test(),
            Some(preapplied.as_path())
        );
        assert_eq!(view.applied_persona_id_for_test(), Some("missing-persona"));
        assert_eq!(view.active_outfit_for_test(), None);
        assert_eq!(view.pending_model_runtime_for_test(), None);

        view.finish_global_model_selection_with_persona_for_test(
            save_revision,
            Some(preapplied.clone()),
            Some(preapplied.clone()),
            Ok(Some(())),
            ("missing-persona", Some(&missing)),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(preapplied.as_path())
        );
        assert_eq!(view.emitted_model_paths_for_test(), Vec::new());
    });
}
