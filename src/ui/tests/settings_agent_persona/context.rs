use super::*;

#[test]
fn live_context_revisions_are_scoped_to_the_active_persona() {
    let live = LiveContextUsage::default();
    assert_eq!(live.revision_for("moon"), None);

    live.publish("moon", ContextUsage::default(), Vec::new());
    let moon_revision = live
        .revision_for("moon")
        .expect("刚发布的人格应有实时 revision");
    assert!(moon_revision > 0);
    assert_eq!(live.revision_for("study"), None);

    live.publish("study", ContextUsage::default(), Vec::new());
    assert_eq!(live.revision_for("moon"), None);
    assert!(
        live.revision_for("study")
            .is_some_and(|revision| revision > moon_revision)
    );
}

#[gpui::test]
fn a_non_empty_context_builds_auto_growing_message_editors(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.load_context_messages_for_test(
            vec![ContextMessage {
                id: 1,
                role: ChatRole::User,
                content: "第一行\n第二行".to_owned(),
                tokens: 8,
                fixed_tokens: 4,
                trace: None,
            }],
            window,
            cx,
        );
        assert_eq!(view.context_editor_count_for_test(), 1);
        assert_eq!(view.context_message_ids_for_test(), [1]);
    });
}

#[gpui::test]
fn selected_context_messages_share_one_confirmation(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.load_context_messages_for_test(
            vec![
                ContextMessage {
                    id: 1,
                    role: ChatRole::User,
                    content: "问题一".to_owned(),
                    tokens: 7,
                    fixed_tokens: 4,
                    trace: None,
                },
                ContextMessage {
                    id: 2,
                    role: ChatRole::Assistant,
                    content: "回答一".to_owned(),
                    tokens: 7,
                    fixed_tokens: 4,
                    trace: None,
                },
                ContextMessage {
                    id: 3,
                    role: ChatRole::User,
                    content: "问题二".to_owned(),
                    tokens: 7,
                    fixed_tokens: 4,
                    trace: None,
                },
            ],
            window,
            cx,
        );
        view.toggle_context_message_selected_for_test(1, cx);
        view.toggle_context_message_selected_for_test(3, cx);
        assert_eq!(view.selected_context_messages_for_test(), 2);
        view.copy_selected_context_messages_for_test(cx);

        view.request_delete_selected_context_messages_for_test(cx);
        assert_eq!(
            view.confirm_message_for_test(),
            Some(
                rust_i18n::t!(
                    "persona.confirm_delete_context_messages",
                    persona = "人格 moon",
                    count = 2
                )
                .to_string()
            )
        );
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("问题一\n问题二".to_owned())
    );
}

#[gpui::test]
fn context_bubbles_use_whole_row_selection_without_inline_controls(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            vec![
                ContextMessage {
                    id: 1,
                    role: ChatRole::User,
                    content: "一行短消息".to_owned(),
                    tokens: 10,
                    fixed_tokens: 4,
                    trace: None,
                },
                ContextMessage {
                    id: 2,
                    role: ChatRole::Assistant,
                    content: "第一行\n第二行\n第三行".to_owned(),
                    tokens: 16,
                    fixed_tokens: 4,
                    trace: None,
                },
            ],
            window,
            cx,
        );
        assert_eq!(view.context_message_ids_for_test(), [1, 2]);
    });
    cx.run_until_parked();

    let card = cx
        .debug_bounds("context-card-1")
        .expect("第一条上下文卡片应已渲染");
    assert!(cx.debug_bounds("context-select-1").is_none());
    assert!(cx.debug_bounds("context-delete-1").is_none());

    let center = point(
        card.origin.x + card.size.width / 2.0,
        card.origin.y + card.size.height / 2.0,
    );
    cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 1);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("delete-selected-context-messages")
            .is_some(),
        "选择消息后应显示仅删除选择项的入口"
    );

    cx.simulate_mouse_down(center, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    let menu = cx
        .debug_bounds("context-action-menu")
        .expect("右键图标操作栏应已渲染");
    let edit = cx
        .debug_bounds("context-action-edit")
        .expect("编辑图标应已渲染");
    let copy = cx
        .debug_bounds("context-action-copy")
        .expect("复制图标应已渲染");
    let delete = cx
        .debug_bounds("context-action-delete")
        .expect("删除图标应已渲染");
    assert_eq!(edit.origin.y, copy.origin.y);
    assert_eq!(copy.origin.y, delete.origin.y);
    assert!(edit.origin.x < copy.origin.x && copy.origin.x < delete.origin.x);
    assert!(delete.origin.x + delete.size.width <= menu.origin.x + menu.size.width);

    let copy_center = point(
        copy.origin.x + copy.size.width / 2.0,
        copy.origin.y + copy.size.height / 2.0,
    );
    cx.simulate_mouse_down(copy_center, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(copy_center, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("一行短消息".to_owned())
    );
    assert!(cx.debug_bounds("context-action-menu").is_none());
}

#[gpui::test]
fn dragging_from_a_message_selects_a_contiguous_message_range(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=3)
                .map(|id| ContextMessage {
                    id,
                    role: if id % 2 == 0 {
                        ChatRole::Assistant
                    } else {
                        ChatRole::User
                    },
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let first = cx
        .debug_bounds("context-card-1")
        .expect("第一条消息应已渲染");
    let third = cx
        .debug_bounds("context-card-3")
        .expect("第三条消息应已渲染");
    let start = point(
        first.origin.x + first.size.width / 2.0,
        first.origin.y + first.size.height / 2.0,
    );
    let cursor = point(
        third.origin.x + third.size.width / 2.0,
        third.origin.y + third.size.height / 2.0,
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    assert!(
        cx.debug_bounds("context-selection-box").is_some(),
        "连续框选时应显示选择区域"
    );
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 3);
    });
    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert!(!view.context_selection_active_for_test());
        assert!(!view.context_selection_auto_scroll_scheduled_for_test());
    });
}

#[gpui::test]
fn dragging_from_viewport_padding_selects_messages(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=3)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let scroll = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let third = cx
        .debug_bounds("context-card-3")
        .expect("第三条消息应已渲染");
    let start = point(scroll.origin.x + px(4.0), third.origin.y - px(80.0));
    let cursor = point(
        third.origin.x + third.size.width / 2.0,
        third.origin.y + third.size.height / 2.0,
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 3);
    });
    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
}

#[gpui::test]
fn scrolling_during_marquee_selection_keeps_the_box_and_drag_alive(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=40)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let viewport = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let last = cx
        .debug_bounds("context-card-40")
        .expect("最后一条消息应已渲染");
    let start = point(
        last.origin.x + last.size.width / 2.0,
        last.origin.y + last.size.height / 2.0,
    );
    let cursor = point(start.x - px(120.0), viewport.origin.y + px(80.0));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    let selection_before = cx
        .debug_bounds("context-selection-box")
        .expect("滚动前应显示框选区域");
    let offset_before = view.update(cx, |view, _| {
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
        (
            view.context_scroll_for_test().0,
            view.selected_context_messages_for_test(),
        )
    });

    cx.simulate_event(ScrollWheelEvent {
        position: cursor,
        delta: ScrollDelta::Pixels(point(px(0.0), px(180.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    let selection_after_scroll = cx
        .debug_bounds("context-selection-box")
        .expect("滚动后框选区域不应消失");
    assert_eq!(selection_after_scroll.origin, selection_before.origin);
    assert_eq!(
        selection_after_scroll.size.width,
        selection_before.size.width
    );
    assert!(selection_after_scroll.size.height > selection_before.size.height);
    view.update(cx, |view, _| {
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() >= offset_before.1);
        assert_ne!(view.context_scroll_for_test().0, offset_before.0);
    });

    // 模拟部分平台在滚动后发出的无按键状态过渡移动事件。
    cx.simulate_mouse_move(
        point(cursor.x + px(40.0), cursor.y + px(20.0)),
        None,
        Modifiers::none(),
    );
    assert_eq!(
        cx.debug_bounds("context-selection-box"),
        Some(selection_after_scroll)
    );
    view.update(cx, |view, _| {
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
    });

    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert!(!view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
    });
}

#[gpui::test]
fn marquee_selection_auto_scrolls_from_the_viewport_edge(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=40)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let viewport = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let last = cx
        .debug_bounds("context-card-40")
        .expect("最后一条消息应已渲染");
    let start = point(
        last.origin.x + last.size.width / 2.0,
        last.origin.y + last.size.height / 2.0,
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    let cursor = point(start.x, viewport.origin.y + px(8.0));
    let offset_before = view.update(cx, |view, _| view.context_scroll_for_test().0);
    cx.simulate_mouse_move(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, cx| {
        assert!(view.context_selection_auto_scroll_scheduled_for_test());
        assert!(view.advance_context_selection_auto_scroll_for_test(cx));
        assert_ne!(view.context_scroll_for_test().0, offset_before);
        assert!(view.context_selection_active_for_test());
        assert!(view.selected_context_messages_for_test() > 0);
    });
    cx.simulate_mouse_up(cursor, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        assert!(!view.context_selection_active_for_test());
        assert!(!view.context_selection_auto_scroll_scheduled_for_test());
    });
}

#[gpui::test]
fn context_keyboard_shortcuts_select_delete_and_cancel_edit(cx: &mut TestAppContext) {
    let (view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("moon", None)],
            selected: Some("moon".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    cx.update_window_entity(&view, |view, window, cx| {
        view.show_context_for_test(cx);
        view.load_context_messages_for_test(
            (1_u64..=3)
                .map(|id| ContextMessage {
                    id,
                    role: ChatRole::User,
                    content: format!("消息 {id}"),
                    tokens: 6,
                    fixed_tokens: 4,
                    trace: None,
                })
                .collect(),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    let first = cx
        .debug_bounds("context-card-1")
        .expect("第一条消息应已渲染");
    let first_center = point(
        first.origin.x + first.size.width / 2.0,
        first.origin.y + first.size.height / 2.0,
    );
    cx.simulate_mouse_down(first_center, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(first_center, MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes("alt-a");
    view.update(cx, |view, _| {
        assert_eq!(view.selected_context_messages_for_test(), 3);
    });
    cx.simulate_keystrokes("delete");
    view.update(cx, |view, cx| {
        assert!(view.pending_confirm_for_test().is_some());
        view.cancel_confirm_for_test(cx);
    });

    cx.update_window_entity(&view, |view, window, cx| {
        view.begin_context_message_edit_for_test(1, window, cx);
        view.set_context_message_content_for_test(1, "未提交修改", window, cx);
    });
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.context_message_content_for_test(1, cx).as_deref(),
            Some("消息 1")
        );
    });
}
