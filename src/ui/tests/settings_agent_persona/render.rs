use super::*;

#[gpui::test]
fn persona_editor_renders_five_tabs_and_row_delete_buttons(cx: &mut TestAppContext) {
    let (_view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None)],
            selected: Some("a".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.run_until_parked();

    let tabs = cx.debug_bounds("persona-tabs").expect("人格页签行应已渲染");
    for (index, selector) in [
        "persona-tab-0",
        "persona-tab-1",
        "persona-tab-2",
        "persona-tab-3",
        "persona-tab-4",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "第 {index} 个人格页签应已渲染"
        );
    }
    let first_tab = cx
        .debug_bounds("persona-tab-0")
        .expect("第一个人格页签应已渲染");
    let last_tab = cx
        .debug_bounds("persona-tab-4")
        .expect("最后一个人格页签应已渲染");
    assert_eq!(first_tab.origin.x, tabs.origin.x);
    assert_eq!(
        last_tab.origin.x + last_tab.size.width,
        tabs.origin.x + tabs.size.width,
        "人格页签应占满整行，右侧不得留空"
    );
    assert!(cx.debug_bounds("persona-delete-0").is_some());
    assert!(cx.debug_bounds("persona-delete-1").is_some());
}

#[gpui::test]
fn persona_list_starts_at_the_top_and_places_its_count_in_the_footer(cx: &mut TestAppContext) {
    let (_view, cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas: vec![persona("a", None), persona("b", None)],
            selected: Some("a".to_owned()),
            pending_deletions: Vec::new(),
        },
    );
    cx.run_until_parked();

    let sidebar = cx
        .debug_bounds("persona-sidebar")
        .expect("人格侧栏应已渲染");
    let list = cx.debug_bounds("persona-list").expect("人格列表应已渲染");
    let count = cx.debug_bounds("persona-count").expect("人格数量应已渲染");
    let add = cx
        .debug_bounds("persona-add")
        .expect("添加人格按钮应已渲染");

    assert_eq!(list.origin.y, sidebar.origin.y);
    assert!(count.origin.y >= list.origin.y + list.size.height);
    assert!(count.origin.x < add.origin.x);
    assert_eq!(
        count.origin.y + count.size.height / 2.0,
        add.origin.y + add.size.height / 2.0
    );
}

#[gpui::test]
fn context_limits_are_empty_inline_inputs_in_a_compact_stats_row(cx: &mut TestAppContext) {
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
        assert_eq!(view.context_limit_inputs_for_test(cx), ["", ""]);
        assert_eq!(
            view.context_limits_for_test(cx),
            PersonaContextLimits::default()
        );
        view.show_context_for_test(cx);
    });
    cx.run_until_parked();

    let stats = cx
        .debug_bounds("context-stats")
        .expect("上下文统计行应已渲染");
    let messages = cx
        .debug_bounds("context-stat-messages")
        .expect("消息统计应已渲染");
    let tokens = cx
        .debug_bounds("context-stat-tokens")
        .expect("Token 统计应已渲染");
    let message_limit = cx
        .debug_bounds("context-limit-messages")
        .expect("消息上限输入应位于统计行");
    let token_limit = cx
        .debug_bounds("context-limit-tokens")
        .expect("Token 上限输入应位于统计行");
    let messages_view = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文消息列表应已渲染");

    assert_eq!(stats.size.height, px(58.0));
    assert!(messages.origin.x < message_limit.origin.x);
    assert!(message_limit.origin.x + message_limit.size.width <= tokens.origin.x);
    assert!(
        tokens.origin.x >= stats.origin.x + stats.size.width / 3.0,
        "Token 统计应展开到统计行中部，避免右侧集中留白"
    );
    assert!(tokens.origin.x < token_limit.origin.x);
    assert!(token_limit.origin.x + token_limit.size.width <= stats.origin.x + stats.size.width);
    assert_eq!(
        messages_view.origin.y,
        stats.origin.y + stats.size.height + px(12.0),
        "消息列表与统计行之间不应残留旧上限控制区"
    );
}

#[gpui::test]
fn persona_editor_accepts_more_than_thirty_two_entries(cx: &mut TestAppContext) {
    let personas = (0..40)
        .map(|index| persona(&format!("p-{index}"), None))
        .collect::<Vec<_>>();
    let (view, _cx) = mount(
        cx,
        LlmSettings::default(),
        PersonaSettings {
            personas,
            selected: Some("p-0".to_owned()),
            pending_deletions: Vec::new(),
        },
    );

    view.update(_cx, |view, _| {
        assert_eq!(view.persona_ids_for_test().len(), 40);
    });
}

#[gpui::test]
fn context_message_editor_uses_the_available_bubble_width(cx: &mut TestAppContext) {
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
            vec![ContextMessage {
                id: 1,
                role: ChatRole::Assistant,
                content: "需要编辑的长消息".to_owned(),
                tokens: 8,
                fixed_tokens: 4,
                trace: None,
            }],
            window,
            cx,
        );
    });
    cx.run_until_parked();
    cx.update_window_entity(&view, |view, window, cx| {
        view.begin_context_message_edit_for_test(1, window, cx);
    });
    cx.run_until_parked();

    let bubble = cx
        .debug_bounds("context-bubble-1")
        .expect("编辑气泡应已渲染");
    assert!(bubble.size.width >= px(300.0));
}

#[gpui::test]
fn context_message_list_fills_the_page_and_initially_scrolls_to_bottom(cx: &mut TestAppContext) {
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
            (1_u64..=30)
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

    view.update(cx, |view, _| {
        let (offset, max_offset) = view.context_scroll_for_test();
        assert!(max_offset > px(0.0));
        assert_eq!(offset, -max_offset);
    });
    let scroll = cx
        .debug_bounds("context-message-scroll")
        .expect("上下文滚动视口应已渲染");
    let page = cx
        .debug_bounds("persona-context-page")
        .expect("上下文页面应已渲染");
    assert_eq!(
        scroll.origin.y + scroll.size.height,
        page.origin.y + page.size.height - px(24.0),
        "消息滚动区应填满页面剩余高度"
    );
}
