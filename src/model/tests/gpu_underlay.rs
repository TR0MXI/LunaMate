use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::model::{
    RenderCancellation, RenderedModelFrame, command_channel,
    gpu_underlay::{
        GpuUnderlaySize, LatestFrameSlot, LoadRequest, MailboxUpdate, PresentedFrame,
        WorkerMailbox,
        worker::{PauseWaitResult, RetryWaitResult, wait_for_surface_retry, wait_while_paused},
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
fn worker_pause_state_is_latest_value_and_coalesced() {
    let mailbox = WorkerMailbox::default();
    mailbox.set_paused(true);
    mailbox.set_paused(true);

    let paused = mailbox.wait(Some(Duration::ZERO));
    assert!(paused.paused);
    assert!(paused.pause_changed);
    assert!(mailbox.is_paused());

    let unchanged = mailbox.wait(Some(Duration::ZERO));
    assert!(unchanged.paused);
    assert!(!unchanged.pause_changed);

    mailbox.set_paused(false);
    let resumed = mailbox.wait(Some(Duration::ZERO));
    assert!(!resumed.paused);
    assert!(resumed.pause_changed);
}

#[test]
fn replacement_and_shutdown_remain_observable_while_paused() {
    let mailbox = WorkerMailbox::default();
    mailbox.set_paused(true);
    mailbox.replace_model(load_request(9));

    let replacement = mailbox.wait(None);
    assert!(replacement.paused);
    assert_eq!(
        replacement
            .replacement
            .expect("暂停时必须保留最新模型请求")
            .generation,
        9
    );

    mailbox.shutdown();
    let shutdown = mailbox.wait(None);
    assert!(shutdown.shutdown);
}

#[test]
fn pause_wait_preserves_a_command_wake_for_resume() {
    let mailbox = Arc::new(WorkerMailbox::default());
    mailbox.set_paused(true);
    mailbox.wake();
    let worker_mailbox = mailbox.clone();
    let worker = thread::spawn(move || wait_while_paused(&worker_mailbox));
    let deadline = Instant::now() + Duration::from_secs(1);
    while mailbox.has_pending_wake() {
        assert!(Instant::now() < deadline, "暂停等待未消费测试唤醒");
        thread::yield_now();
    }

    mailbox.set_paused(false);
    let result = worker.join().expect("暂停等待线程必须正常结束");
    assert!(matches!(result, PauseWaitResult::Running));
    assert!(mailbox.wait(Some(Duration::ZERO)).woken);
}

#[test]
fn surface_retry_honors_deadline_and_preserves_wake() {
    let mailbox = WorkerMailbox::default();
    mailbox.wake();
    let delay = Duration::from_millis(10);
    let started = Instant::now();

    let result = wait_for_surface_retry(&mailbox, delay);

    assert!(matches!(result, RetryWaitResult::Ready));
    assert!(started.elapsed() >= delay);
    assert!(mailbox.wait(Some(Duration::ZERO)).woken);
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
