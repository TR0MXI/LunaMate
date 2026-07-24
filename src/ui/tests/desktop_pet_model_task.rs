use std::future::{pending, ready};

use futures::executor::block_on;

use crate::{
    model::frame_wake_channel,
    ui::desktop_pet::model_task::{FrameWaitResult, wait_for_frame_or_rate_change},
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
