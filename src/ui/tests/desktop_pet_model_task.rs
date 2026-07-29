use std::{
    future::{pending, ready},
    path::Path,
};

use futures::executor::block_on;

use crate::{
    model::frame_wake_channel,
    ui::desktop_pet::model_task::{
        FrameWaitResult, model_generation_can_be_reused, wait_for_frame_or_rate_change,
    },
};

#[test]
fn frame_rate_change_interrupts_a_pending_frame_wait() {
    let (wake, receiver) = frame_wake_channel();
    wake.wake();

    assert_eq!(
        block_on(wait_for_frame_or_rate_change(pending(), &receiver)),
        FrameWaitResult::FrameRateChanged
    );
    let (wake, receiver) = frame_wake_channel();
    wake.wake();
    assert_eq!(
        block_on(wait_for_frame_or_rate_change(ready(true), &receiver)),
        FrameWaitResult::FrameRateChanged
    );
}

#[test]
fn completed_and_closed_frame_waits_are_distinguished() {
    let (_wake, receiver) = frame_wake_channel();
    assert_eq!(
        block_on(wait_for_frame_or_rate_change(ready(true), &receiver)),
        FrameWaitResult::FrameReady
    );
    assert_eq!(
        block_on(wait_for_frame_or_rate_change(ready(false), &receiver)),
        FrameWaitResult::Closed
    );
}

#[test]
fn forced_model_reload_does_not_reuse_an_equal_active_generation() {
    let path = Path::new("luna/luna.model3.json");

    assert!(model_generation_can_be_reused(
        Some(path),
        Some(path),
        true,
        false,
    ));
    assert!(!model_generation_can_be_reused(
        Some(path),
        Some(path),
        true,
        true,
    ));
    assert!(!model_generation_can_be_reused(
        Some(path),
        Some(Path::new("luna/other.model3.json")),
        true,
        false,
    ));
}
