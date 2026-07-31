use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::{
    config::VoiceRuntimeSettings,
    voice::transcribe::{TranscriptionJob, TranscriptionQueue},
};
use lunamate_agent::config::WHISPER_LANGUAGE_CODES;

fn job(revision: u64, utterance_id: u64) -> TranscriptionJob {
    TranscriptionJob {
        revision,
        utterance_id,
        samples: vec![0.0; 512],
        settings: Arc::new(VoiceRuntimeSettings {
            mode: crate::config::VoiceMode::Off,
            backend: None,
            use_gpu: false,
            whisper_language: None,
        }),
    }
}

#[test]
fn pending_transcription_is_a_latest_value_slot() {
    let desired_revision = Arc::new(AtomicU64::new(1));
    let queue = TranscriptionQueue::new(desired_revision);

    assert!(queue.submit(job(1, 10)));
    assert!(queue.submit(job(1, 11)));

    let pending = queue
        .take_pending_for_test()
        .expect("最新 utterance 应当保留在等待槽");
    assert_eq!(pending.utterance_id, 11);
}

#[test]
fn stale_or_shutdown_transcription_cannot_enter_the_queue() {
    let desired_revision = Arc::new(AtomicU64::new(1));
    let queue = TranscriptionQueue::new(desired_revision.clone());
    desired_revision.store(2, Ordering::Release);

    assert!(!queue.submit(job(1, 10)));
    assert!(queue.take_pending_for_test().is_none());

    queue.shutdown();
    assert!(!queue.submit(job(2, 11)));
}

#[test]
fn context_release_clears_pending_transcription_and_wakes_the_worker() {
    let desired_revision = Arc::new(AtomicU64::new(1));
    let queue = TranscriptionQueue::new(desired_revision);
    assert!(queue.submit(job(1, 10)));

    queue.release_context();

    assert!(queue.take_pending_for_test().is_none());
    assert!(queue.take_context_release_for_test());
}

#[test]
fn cancelling_one_utterance_does_not_cancel_the_next_in_the_same_revision() {
    let desired_revision = Arc::new(AtomicU64::new(1));
    let queue = TranscriptionQueue::new(desired_revision);
    assert!(queue.submit(job(1, 10)));

    queue.cancel_utterance(1, 10);

    assert!(queue.take_pending_for_test().is_none());
    assert!(queue.is_cancelled_for_test(1, 10));
    assert!(!queue.is_cancelled_for_test(1, 11));
    assert!(queue.submit(job(1, 11)));
}

#[test]
fn configured_language_catalog_matches_the_linked_whisper_runtime() {
    let linked = (0..=whisper_rs::get_lang_max_id())
        .map(|id| whisper_rs::get_lang_str(id).expect("Whisper 语言 ID 应当存在"))
        .collect::<Vec<_>>();

    assert_eq!(linked, WHISPER_LANGUAGE_CODES);
}
