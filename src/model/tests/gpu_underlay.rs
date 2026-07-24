use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::model::{
    RenderCancellation, RenderedModelFrame, command_channel,
    gpu_underlay::{
        GpuUnderlaySize, LatestFrameSlot, LoadRequest, MailboxUpdate, PresentedFrame, WorkerMailbox,
    },
};

fn load_request(generation: u64) -> LoadRequest {
    let (_, commands) = command_channel();
    LoadRequest {
        generation,
        path: None,
        size: GpuUnderlaySize {
            physical: [200, 400],
            logical: [100, 200],
        },
        cancellation: RenderCancellation::default(),
        commands,
        look_target: Arc::new(Mutex::new([0.0, 0.0])),
    }
}

fn presented_frame(generation: u64, presented_frames: u64) -> PresentedFrame {
    PresentedFrame {
        generation,
        frame: RenderedModelFrame::gpu(Vec::new(), [200, 400]),
        presented_at: Instant::now(),
        presented_frames,
    }
}

#[test]
fn worker_wake_is_coalesced() {
    let mailbox = WorkerMailbox::default();
    mailbox.wake();
    mailbox.wake();

    let update: MailboxUpdate = mailbox.wait(Some(Duration::ZERO));
    assert!(update.woken);
    assert!(!mailbox.wait(Some(Duration::ZERO)).woken);
}

#[test]
fn replacement_does_not_fabricate_a_worker_wake() {
    let mailbox = WorkerMailbox::default();
    mailbox.replace_model(load_request(7));

    let update = mailbox.wait(Some(Duration::ZERO));
    assert!(update.replacement.is_some());
    assert!(!update.woken);
}

#[test]
fn real_wake_remains_pending_beside_a_replacement() {
    let mailbox = WorkerMailbox::default();
    mailbox.replace_model(load_request(7));
    mailbox.wake();

    let update = mailbox.wait(Some(Duration::ZERO));
    assert!(update.replacement.is_some());
    assert!(update.woken);
}

#[test]
fn pending_model_replacement_keeps_only_the_latest_generation() {
    let mailbox = WorkerMailbox::default();
    mailbox.replace_model(load_request(7));
    mailbox.replace_model(load_request(8));

    let update = mailbox.wait(Some(Duration::ZERO));
    assert_eq!(
        update.replacement.expect("最新模型请求必须保留").generation,
        8
    );
}

#[test]
fn latest_frame_notification_is_coalesced_until_consumed() {
    let mut slot = LatestFrameSlot::default();
    slot.begin_generation(7);
    assert!(slot.publish(presented_frame(7, 1)));
    assert!(!slot.publish(presented_frame(7, 2)));

    let latest = slot.take().expect("latest slot 必须保留最近一帧");
    assert_eq!(latest.generation, 7);
    assert_eq!(latest.presented_frames, 2);
    assert!(slot.publish(presented_frame(7, 3)));
}

#[test]
fn failed_latest_frame_notification_can_be_retried() {
    let mut slot = LatestFrameSlot::default();
    slot.begin_generation(7);
    assert!(slot.publish(presented_frame(7, 1)));

    slot.notification_failed();

    assert!(slot.publish(presented_frame(7, 2)));
    let latest = slot.take().expect("重试前 latest slot 必须保留最近一帧");
    assert_eq!(latest.presented_frames, 2);
}

#[test]
fn stale_generation_cannot_publish_after_replacement() {
    let mut slot = LatestFrameSlot::default();
    slot.begin_generation(8);

    assert!(!slot.publish(presented_frame(7, 1)));
    assert!(slot.take().is_none());
}

#[test]
fn shutdown_interrupts_an_idle_worker() {
    let mailbox = WorkerMailbox::default();
    mailbox.shutdown();

    let update = mailbox.wait(None);
    assert!(update.shutdown);
    assert!(update.replacement.is_none());
}
