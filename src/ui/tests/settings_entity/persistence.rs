use super::*;

#[gpui::test]
fn failed_scalar_writes_keep_applied_state_config_events_and_reload_consistent(
    cx: &mut TestAppContext,
) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);
    let original_size = CONFIG.model_window_size();
    let requested_size = if original_size == ModelWindowSize::Large {
        ModelWindowSize::Compact
    } else {
        ModelWindowSize::Large
    };
    let original_eye_tracking = CONFIG.eye_tracking();
    let original_tray_menu = CONFIG.use_native_tray_menu();

    view.update(cx, |view, cx| {
        view.inject_model_window_size_failure_for_test(requested_size, cx);
        view.inject_eye_tracking_failure_for_test(!original_eye_tracking, cx);
        view.inject_native_tray_menu_failure_for_test(!original_tray_menu, cx);

        assert_eq!(view.model_window_size_for_test(), original_size);
        assert_eq!(view.applied_model_window_size_for_test(), original_size);
        assert_eq!(view.eye_tracking_for_test(), original_eye_tracking);
        assert_eq!(view.applied_eye_tracking_for_test(), original_eye_tracking);
        assert_eq!(view.use_native_tray_menu_for_test(), original_tray_menu);
        assert_eq!(
            view.applied_use_native_tray_menu_for_test(),
            original_tray_menu
        );
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::ModelWindowSize),
            0
        );
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::EyeTracking),
            0
        );
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::NativeTrayMenu),
            0
        );
    });

    assert_eq!(CONFIG.model_window_size(), original_size);
    assert_eq!(CONFIG.eye_tracking(), original_eye_tracking);
    assert_eq!(CONFIG.use_native_tray_menu(), original_tray_menu);
    let reload_catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let reloaded = view.update(cx, |_view, cx| {
        cx.new(|cx| SettingsView::new(reload_catalog, unavailable_agent(), None, cx))
    });
    reloaded.update(cx, |reloaded, _cx| {
        assert_eq!(reloaded.applied_model_window_size_for_test(), original_size);
        assert_eq!(
            reloaded.applied_eye_tracking_for_test(),
            original_eye_tracking
        );
        assert_eq!(
            reloaded.applied_use_native_tray_menu_for_test(),
            original_tray_menu
        );
    });
}

#[gpui::test]
fn failed_appearance_write_does_not_publish_theme_language_or_locale(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);
    let original = CONFIG.appearance().as_ref().clone();
    let locale_before = rust_i18n::locale().to_string();
    let mut requested = original.clone();
    requested.language = if original.language == AppLanguage::English {
        AppLanguage::Japanese
    } else {
        AppLanguage::English
    };
    requested.theme = if original.theme == ThemePreset::Dark {
        ThemePreset::Light
    } else {
        ThemePreset::Dark
    };

    view.update(cx, |view, cx| {
        view.inject_appearance_failure_for_test(requested, cx);
        assert_eq!(view.appearance_language_for_test(), original.language);
        assert_eq!(view.applied_appearance_for_test(), &original);
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::Appearance),
            0
        );
    });

    assert_eq!(CONFIG.appearance().as_ref(), &original);
    assert_eq!(rust_i18n::locale().to_string(), locale_before);
    let reload_catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let reloaded = view.update(cx, |_view, cx| {
        cx.new(|cx| SettingsView::new(reload_catalog, unavailable_agent(), None, cx))
    });
    reloaded.update(cx, |reloaded, _cx| {
        assert_eq!(reloaded.applied_appearance_for_test(), &original);
    });
}

#[gpui::test]
fn failed_model_resource_write_keeps_only_the_editable_draft(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);
    let key = ModelResourceKey::new(
        format!("failure-{}/model.model3.json", std::process::id()),
        ModelResourceKind::Motion,
        "motion:failure",
    );
    let original = CONFIG.model_resource_settings();
    let requested_name = if original.name(&key) == Some("未发布名称") {
        "另一个未发布名称"
    } else {
        "未发布名称"
    };

    view.update(cx, |view, cx| {
        view.inject_model_resource_name_failure_for_test(key.clone(), requested_name, cx);
        assert_eq!(
            view.model_resource_names_for_test(&key),
            (Some(requested_name), original.name(&key))
        );
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::ModelResources),
            0
        );
    });

    assert_eq!(CONFIG.model_resource_settings().as_ref(), original.as_ref());
    let reload_catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let reloaded = view.update(cx, |_view, cx| {
        cx.new(|cx| SettingsView::new(reload_catalog, unavailable_agent(), None, cx))
    });
    reloaded.update(cx, |reloaded, _cx| {
        assert_eq!(
            reloaded.model_resource_names_for_test(&key),
            (original.name(&key), original.name(&key))
        );
    });
}

#[gpui::test]
fn replaced_write_result_cannot_rollback_a_newer_window_size_draft(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);
    let applied = CONFIG.model_window_size();
    let old = if applied == ModelWindowSize::Compact {
        ModelWindowSize::Standard
    } else {
        ModelWindowSize::Compact
    };
    let newer = [
        ModelWindowSize::Large,
        ModelWindowSize::ExtraLarge,
        ModelWindowSize::Auto,
    ]
    .into_iter()
    .find(|candidate| *candidate != applied && *candidate != old)
    .expect("窗口尺寸档位必须存在第三个候选");

    view.update(cx, |view, cx| {
        let old_revision = view.replace_model_window_size_draft_for_test(old);
        view.replace_model_window_size_draft_for_test(newer);
        view.finish_model_window_size_write_for_test(old_revision, old, Ok(None), cx);

        assert_eq!(view.model_window_size_for_test(), newer);
        assert_eq!(view.applied_model_window_size_for_test(), applied);
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::ModelWindowSize),
            0
        );
    });
    assert_eq!(CONFIG.model_window_size(), applied);
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
    let config_before = CONFIG.selected_model();

    view.update(cx, |view, cx| {
        let save_revision = view.stage_global_model_selection_for_test(alternate.clone(), cx);
        view.invalidate_catalog_revision_for_test();
        view.finish_global_model_selection_for_test(
            save_revision,
            Some(alternate),
            Some(initial.clone()),
            Err(ConfigWriteError::InvalidValue("模拟写入失败".to_owned())),
            cx,
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::Model),
            0
        );
        assert!(view.status_for_test().is_some());
    });
    assert_eq!(CONFIG.selected_model(), config_before);
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
        let old_revision = view.stage_global_model_selection_for_test(alternate.clone(), cx);
        view.stage_global_model_selection_for_test(newest.clone(), cx);
        view.finish_global_model_selection_for_test(
            old_revision,
            Some(alternate),
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
fn ordinary_model_selection_loads_once_only_after_save_succeeds(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("global");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["global.model3.json", "global-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("global/global.model3.json");
    let alternate = PathBuf::from("global/global-alt.model3.json");
    let alternate_path = directory.path().join(&alternate);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial.clone(), "coat");
        let save_revision = view.stage_global_model_selection_for_test(alternate.clone(), cx);
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(view.emitted_model_paths_for_test(), Vec::new());

        view.finish_global_model_selection_for_test(
            save_revision,
            Some(alternate.clone()),
            Some(alternate.clone()),
            Ok(Some(())),
            cx,
        );

        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(alternate.as_path())
        );
        assert_eq!(
            view.emitted_model_paths_for_test(),
            vec![Some(alternate_path)]
        );
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::Model),
            1
        );
    });
}

#[gpui::test]
fn failed_preapplied_agent_model_save_rolls_back_every_marker_and_runtime(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("global");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["global.model3.json", "global-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("global/global.model3.json");
    let alternate = PathBuf::from("global/global-alt.model3.json");
    let initial_path = directory.path().join(&initial);
    let alternate_path = directory.path().join(&alternate);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial.clone(), "coat");
        let (save_revision, immediate_path) =
            view.stage_preapplied_model_selection_for_test(alternate.clone(), cx);
        assert_eq!(immediate_path, alternate_path);
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(alternate.as_path())
        );
        assert_eq!(
            view.applied_global_model_selection_for_test(),
            Some(initial.as_path())
        );

        view.finish_global_model_selection_for_test(
            save_revision,
            Some(alternate),
            Some(initial.clone()),
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );

        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(
            view.applied_global_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(
            view.applied_persona_model_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(view.active_outfit_for_test(), Some("coat"));
        assert_eq!(
            view.emitted_model_paths_for_test(),
            vec![Some(initial_path)]
        );
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::Model),
            1
        );
        assert!(view.status_for_test().is_some());
    });
}

#[gpui::test]
fn successful_preapplied_agent_model_save_keeps_one_immediate_load(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("global");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in ["global.model3.json", "global-alt.model3.json"] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("global/global.model3.json");
    let alternate = PathBuf::from("global/global-alt.model3.json");
    let alternate_path = directory.path().join(&alternate);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial, "coat");
        let (save_revision, immediate_path) =
            view.stage_preapplied_model_selection_for_test(alternate.clone(), cx);
        let mut load_paths = vec![immediate_path];

        view.finish_global_model_selection_for_test(
            save_revision,
            Some(alternate.clone()),
            Some(alternate.clone()),
            Ok(Some(())),
            cx,
        );
        load_paths.extend(view.emitted_model_paths_for_test().into_iter().flatten());

        assert_eq!(load_paths, vec![alternate_path]);
        assert_eq!(
            view.emitted_event_count_for_test(SettingsEventKindForTest::Model),
            0
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(alternate.as_path())
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(alternate.as_path())
        );
        assert_eq!(
            view.applied_global_model_selection_for_test(),
            Some(alternate.as_path())
        );
        assert_eq!(
            view.applied_persona_model_for_test(),
            Some(alternate.as_path())
        );
        assert_eq!(view.active_outfit_for_test(), None);
    });
}

#[gpui::test]
fn older_preapplied_save_failure_cannot_rollback_the_newer_runtime(cx: &mut TestAppContext) {
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
    let initial_path = directory.path().join(&initial);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial.clone(), "coat");
        let (old_revision, _) =
            view.stage_preapplied_model_selection_for_test(alternate.clone(), cx);
        let (new_revision, _) = view.stage_preapplied_model_selection_for_test(newest.clone(), cx);

        view.finish_global_model_selection_for_test(
            old_revision,
            Some(alternate),
            Some(initial.clone()),
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(newest.as_path())
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(newest.as_path())
        );
        assert_eq!(view.emitted_model_paths_for_test(), Vec::new());
        assert_eq!(view.status_for_test(), None);

        view.finish_global_model_selection_for_test(
            new_revision,
            Some(newest),
            Some(initial.clone()),
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(
            view.emitted_model_paths_for_test(),
            vec![Some(initial_path)]
        );
    });
}

#[gpui::test]
fn older_preapplied_success_advances_the_baseline_without_reloading_a_newer_runtime(
    cx: &mut TestAppContext,
) {
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
    let committed = PathBuf::from("global/global-alt.model3.json");
    let pending = PathBuf::from("global/global-new.model3.json");
    let committed_path = directory.path().join(&committed);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial, "coat");
        let (committed_revision, _) =
            view.stage_preapplied_model_selection_for_test(committed.clone(), cx);
        let (pending_revision, _) =
            view.stage_preapplied_model_selection_for_test(pending.clone(), cx);

        view.finish_global_model_selection_for_test(
            committed_revision,
            Some(committed.clone()),
            Some(committed.clone()),
            Ok(Some(())),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(pending.as_path()),
            "旧成功只能推进已提交基线，不能取得较新 pending runtime"
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(pending.as_path())
        );
        assert_eq!(
            view.applied_global_model_selection_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(view.emitted_model_paths_for_test(), Vec::new());

        view.finish_global_model_selection_for_test(
            pending_revision,
            Some(pending),
            Some(committed.clone()),
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(
            view.applied_persona_model_for_test(),
            Some(committed.as_path())
        );
        assert_eq!(
            view.emitted_model_paths_for_test(),
            vec![Some(committed_path)]
        );
    });
}

#[gpui::test]
fn published_persona_model_replaces_the_rollback_target_of_an_older_pending_write(
    cx: &mut TestAppContext,
) {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("global");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for manifest in [
        "global.model3.json",
        "global-alt.model3.json",
        "persona.model3.json",
    ] {
        fs::write(model_directory.join(manifest), "{}").expect("测试模型清单应当可以创建");
    }
    let initial = PathBuf::from("global/global.model3.json");
    let pending = PathBuf::from("global/global-alt.model3.json");
    let bound = PathBuf::from("global/persona.model3.json");
    let bound_path = directory.path().join(&bound);
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial.clone(), "coat");
        let (pending_revision, _) =
            view.stage_preapplied_model_selection_for_test(pending.clone(), cx);

        view.apply_published_persona_model_for_test(
            initial.clone(),
            "bound-persona",
            Some(&bound),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(bound.as_path())
        );
        assert_eq!(
            view.emitted_model_paths_for_test(),
            vec![Some(bound_path.clone())]
        );

        view.finish_global_model_selection_with_persona_for_test(
            pending_revision,
            Some(pending),
            Some(initial.clone()),
            Err(ConfigWriteError::PersistenceUnavailable),
            ("bound-persona", Some(&bound)),
            cx,
        );
        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(bound.as_path())
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(initial.as_path())
        );
        assert_eq!(view.applied_persona_model_for_test(), Some(bound.as_path()));
        assert_eq!(
            view.emitted_model_paths_for_test(),
            vec![Some(bound_path)],
            "旧 pending 失败不得再次加载人格发布前的模型"
        );
    });
}

#[gpui::test]
fn an_older_failure_cannot_rollback_a_newer_successful_preapplication(cx: &mut TestAppContext) {
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
    let older = PathBuf::from("global/global-alt.model3.json");
    let newer = PathBuf::from("global/global-new.model3.json");
    let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(&initial))
        .expect("测试模型目录应当可以扫描");
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_applied_persona_model_for_test(initial, "coat");
        let (older_revision, _) = view.stage_preapplied_model_selection_for_test(older.clone(), cx);
        let (newer_revision, _) = view.stage_preapplied_model_selection_for_test(newer.clone(), cx);

        view.finish_global_model_selection_for_test(
            newer_revision,
            Some(newer.clone()),
            Some(newer.clone()),
            Ok(Some(())),
            cx,
        );
        view.finish_global_model_selection_for_test(
            older_revision,
            Some(older),
            Some(newer.clone()),
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );

        assert_eq!(
            view.runtime_model_selection_for_test(),
            Some(newer.as_path())
        );
        assert_eq!(
            view.global_model_selection_for_test(),
            Some(newer.as_path())
        );
        assert_eq!(
            view.applied_global_model_selection_for_test(),
            Some(newer.as_path())
        );
        assert_eq!(view.emitted_model_paths_for_test(), Vec::new());
        assert_eq!(view.status_for_test(), None);
    });
}

#[gpui::test]
fn saved_logging_file_policy_is_persisted_but_marked_for_restart(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        let (draft, persisted) = view.logging_settings_for_test();
        assert_eq!(draft, persisted);
        let requested = crate::config::LoggingSettings {
            rotation: !persisted.rotation,
            ..persisted
        };
        let revision = view.stage_logging_settings_for_test(requested);
        assert_eq!(
            view.logging_settings_for_test(),
            (requested, persisted),
            "写入完成前草稿不得冒充已持久化状态"
        );

        view.finish_logging_settings_write_for_test(
            revision,
            requested,
            ApplyLoggingSettingsOutcome::FilePolicyDeferredUntilRestart,
            cx,
        );
        assert_eq!(view.logging_settings_for_test(), (requested, requested));
        let expected = rust_i18n::t!("status.logging_file_policy_saved_restart").to_string();
        assert_eq!(view.status_for_test(), Some(expected.as_str()));
    });
}

#[gpui::test]
fn saved_logging_level_applies_without_file_policy_restart_notice(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        let (_, persisted) = view.logging_settings_for_test();
        let level = [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ]
        .into_iter()
        .find(|level| *level != persisted.level)
        .expect("日志等级必须存在另一个候选");
        let requested = crate::config::LoggingSettings { level, ..persisted };
        let revision = view.stage_logging_settings_for_test(requested);
        view.finish_logging_settings_write_for_test(
            revision,
            requested,
            ApplyLoggingSettingsOutcome::LevelApplied,
            cx,
        );

        assert_eq!(view.logging_settings_for_test(), (requested, requested));
        assert_eq!(view.status_for_test(), None);
    });
}
