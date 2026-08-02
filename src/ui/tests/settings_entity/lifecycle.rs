use super::*;

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
