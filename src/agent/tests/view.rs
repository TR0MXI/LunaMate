use crate::{
    agent::view::{AgentOverlayLayout, ReplyLifecycle, model_click_event_prompt},
    config::AppLanguage,
};

#[test]
fn model_click_event_names_the_part_in_each_application_language() {
    let cases = [
        (
            AppLanguage::SimplifiedChinese,
            "[事件] 用户点击了 Live2D 模型的“Head”部位。请以当前人格自然回应这次互动。",
        ),
        (
            AppLanguage::TraditionalChinese,
            "[事件] 使用者點擊了 Live2D 模型的「Head」部位。請以目前人格自然回應這次互動。",
        ),
        (
            AppLanguage::English,
            "[Event] The user clicked the \"Head\" area of the Live2D model. Respond naturally to this interaction in character.",
        ),
        (
            AppLanguage::Japanese,
            "[イベント] ユーザーが Live2D モデルの「Head」部位をクリックしました。現在のペルソナとして自然に反応してください。",
        ),
    ];

    for (language, expected) in cases {
        assert_eq!(model_click_event_prompt("Head", language), expected);
    }
}

#[test]
fn narrow_overlay_keeps_space_for_input_and_reply() {
    let narrow = AgentOverlayLayout::for_viewport(121.5, 216.0);
    assert_eq!(narrow.horizontal_inset, 4.0);
    assert_eq!(narrow.control_size, 28.0);
    assert_eq!(narrow.reply_max_height, 84.0);

    let regular = AgentOverlayLayout::for_viewport(220.0, 391.0);
    assert_eq!(regular.horizontal_inset, 12.0);
    assert_eq!(regular.control_size, 32.0);
    assert_eq!(regular.reply_max_height, 180.0);
}

#[test]
fn streaming_reply_never_enters_fade_phase() {
    let mut lifecycle = ReplyLifecycle::new(true);

    assert!(lifecycle.plan_fade(false).is_none());
    assert!(lifecycle.visible());
    assert!(!lifecycle.fading());
}

#[test]
fn hover_invalidates_an_older_fade_revision() {
    let mut lifecycle = ReplyLifecycle::new(true);
    let display_generation = lifecycle.display_generation();
    let revision = lifecycle.plan_fade(true).expect("终态回复应当安排淡出");

    assert!(lifecycle.begin_fade(revision, true));
    assert!(lifecycle.fading());
    assert!(lifecycle.set_hovered(true));
    assert_eq!(lifecycle.display_generation(), display_generation);
    assert!(!lifecycle.fading());
    assert!(!lifecycle.finish_fade(revision, true));
    assert!(lifecycle.visible());

    assert!(lifecycle.set_hovered(false));
    let next_revision = lifecycle
        .plan_fade(true)
        .expect("鼠标离开后应当重新安排淡出");
    assert_ne!(next_revision, revision);
    assert!(lifecycle.begin_fade(next_revision, true));
    assert!(lifecycle.finish_fade(next_revision, true));
    assert!(!lifecycle.visible());
}

#[test]
fn replacement_reply_does_not_inherit_hover_state() {
    let mut lifecycle = ReplyLifecycle::new(true);
    let display_generation = lifecycle.display_generation();
    assert!(lifecycle.set_hovered(true));

    lifecycle.reveal();

    assert_ne!(lifecycle.display_generation(), display_generation);
    assert!(lifecycle.plan_fade(true).is_some());
}
