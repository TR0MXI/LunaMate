use super::*;

#[gpui::test]
fn preview_capabilities_replace_the_previous_generation_snapshot(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let catalog = ModelCatalog::empty(directory.path().to_path_buf());
    let (view, cx) = mount(cx, catalog, None);

    view.update(cx, |view, cx| {
        view.set_preview_capabilities(
            ModelPreviewCapabilities::new_for_test(
                vec![ModelPreviewResource::new_idle_for_test("Idle", "Idle")],
                vec![ModelPreviewResource::new_for_test("Tap", "Tap")],
                vec![
                    ModelPreviewExpression::new_for_test("external:侦探.exp3.json", "侦探", true),
                    ModelPreviewExpression::new_for_test("Smile", "Smile", false),
                ],
            ),
            cx,
        );
        let capabilities = view.preview_capabilities_for_test();
        assert_eq!(capabilities.idle_motions().len(), 1);
        assert_eq!(capabilities.idle_motions()[0].runtime_id(), "Idle");
        assert_eq!(capabilities.motions().len(), 1);
        assert_eq!(capabilities.motions()[0].runtime_id(), "Tap");
        assert_eq!(capabilities.expressions().len(), 2);
        assert!(capabilities.expressions()[0].movable_to_outfit());

        // 模型切换后旧 generation 的能力必须被整体替换，不能残留。
        view.set_preview_capabilities(ModelPreviewCapabilities::default(), cx);
        assert!(
            view.preview_capabilities_for_test()
                .idle_motions()
                .is_empty()
        );
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
                vec![ModelPreviewResource::new_idle_for_test("Idle", "Idle")],
                Vec::new(),
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
