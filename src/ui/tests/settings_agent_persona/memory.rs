use super::*;

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
