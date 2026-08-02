use super::*;

#[test]
fn tts_binding_rows_ignore_chat_models_and_keep_zero_as_disabled() {
    let providers = Arc::new(LlmSettings {
        models: vec![provider("chat"), tts_model("voice-a"), tts_model("voice-b")],
        selected_model: Some("chat".to_owned()),
        selected_transcription_model: None,
    });

    assert_eq!(tts_model_option_index_for_test(&providers, None), 0);
    assert_eq!(
        tts_model_option_index_for_test(&providers, Some("voice-a")),
        1
    );
    assert_eq!(
        tts_model_option_index_for_test(&providers, Some("voice-b")),
        2
    );
    assert_eq!(tts_model_option_index_for_test(&providers, Some("chat")), 0);
}

#[test]
fn new_persona_ids_skip_identifiers_already_in_use() {
    assert_eq!(
        next_persona_id_for_test(&PersonaSettings {
            personas: Vec::new(),
            selected: None,
            pending_deletions: Vec::new(),
        }),
        "persona-1"
    );

    let settings = PersonaSettings {
        personas: vec![persona("persona-1", None), persona("persona-3", None)],
        selected: None,
        pending_deletions: vec!["persona-2".to_owned()],
    };
    assert_eq!(next_persona_id_for_test(&settings), "persona-4");
}

#[test]
fn bound_provider_maps_to_the_selector_row_and_back() {
    let providers = std::sync::Arc::new(LlmSettings {
        models: vec![provider("a"), provider("b")],
        selected_model: Some("a".to_owned()),
        selected_transcription_model: None,
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
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None), persona("c", None)],
            selected: Some("b".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(cx, |view, _cx| {
        assert_eq!(view.persona_ids_for_test(), ["a", "b", "c"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
    });
}

#[gpui::test]
fn adding_a_persona_selects_it_and_allocates_an_unused_id(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("persona-1", None)],
            selected: Some("persona-1".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.add_persona_for_test(window, cx);

        assert_eq!(view.persona_ids_for_test(), ["persona-1", "persona-2"]);
        assert_eq!(view.editing_index_for_test(), Some(1));
        assert_eq!(view.selected_persona_for_test(), Some("persona-2"));
    });
}

#[gpui::test]
fn switching_personas_keeps_each_bound_provider(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings {
            models: vec![provider("a"), provider("b")],
            selected_model: Some("a".to_owned()),
            selected_transcription_model: None,
        },
        PersonaSettings {
            personas: vec![persona("bound", Some("b")), persona("inherit", None)],
            selected: Some("bound".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.select_persona_for_test(1, window, cx);
        view.select_persona_for_test(0, window, cx);

        // 往返切换不得把显式绑定丢成"跟随全局默认"，也不得给未绑定人格补上绑定。
        assert_eq!(view.bound_provider_for_test(0).as_deref(), Some("b"));
        assert_eq!(view.bound_provider_for_test(1), None);
    });
}

#[gpui::test]
fn switching_personas_preserves_a_missing_live2d_binding(cx: &mut TestAppContext) {
    let mut bound = persona("bound", None);
    bound.live2d_model = Some(PathBuf::from("missing/missing.model3.json"));
    let (view, cx) = mount_with_models(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![bound, persona("inherit", None)],
            selected: Some("bound".to_owned()),
            pending_deletions: Vec::new(),
        },
        vec![(
            "可用模型".to_owned(),
            PathBuf::from("available/model.model3.json"),
        )],
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.select_persona_for_test(1, window, cx);
        view.select_persona_for_test(0, window, cx);
        assert_eq!(
            view.bound_live2d_for_test(0),
            Some(PathBuf::from("missing/missing.model3.json"))
        );
        assert_eq!(view.bound_live2d_for_test(1), None);
    });
}
