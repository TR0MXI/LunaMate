use std::sync::Arc;

use crate::model::interaction::{
    COMMAND_CHANNEL_CAPACITY, HitAreaActivation, ModelCommand, command_channel,
    try_tap_motion_groups,
};

use std::sync::mpsc::TrySendError;

#[test]
fn model_command_channel_is_bounded_and_non_blocking() {
    let (sender, _receiver) = command_channel();
    for _ in 0..COMMAND_CHANNEL_CAPACITY {
        sender
            .try_send(ModelCommand::ActivateHitArea(HitAreaActivation::new(
                Arc::from("HitArea"),
                Arc::from("Body"),
            )))
            .expect("channel should accept commands up to its capacity");
    }

    assert!(matches!(
        sender.try_send(ModelCommand::ActivateHitArea(HitAreaActivation::new(
            Arc::from("HitArea"),
            Arc::from("Body"),
        ))),
        Err(TrySendError::Full(_))
    ));
}

#[test]
fn tap_groups_are_ordered_deduplicated_and_lazy() {
    let area = HitAreaActivation::new(Arc::from("HitArea"), Arc::from("Body"));
    let mut candidates = Vec::new();
    let matched = try_tap_motion_groups(&area, |group| {
        candidates.push(group.to_owned());
        group == "Tap@HitArea"
    });

    assert!(matched);
    assert_eq!(candidates, ["Tap@Body", "TapBody", "Tap@HitArea"]);

    let area = HitAreaActivation::new(Arc::from("Body"), Arc::from("Body"));
    let mut candidates = Vec::new();
    assert!(!try_tap_motion_groups(&area, |group| {
        candidates.push(group.to_owned());
        false
    }));
    assert_eq!(candidates, ["Tap@Body", "TapBody", "Tap"]);
}
