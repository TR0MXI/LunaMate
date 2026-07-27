use std::collections::VecDeque;

use crate::voice::vad::{EndpointDetector, EndpointEvent, VadEngine};

struct FakeVad {
    newest: VecDeque<f32>,
    batch_size: usize,
    pending: Vec<f32>,
    resets: usize,
}

impl FakeVad {
    fn new(newest: impl IntoIterator<Item = [f32; 4]>) -> Self {
        Self {
            newest: newest
                .into_iter()
                .map(|probabilities| probabilities[0])
                .collect(),
            batch_size: 1,
            pending: Vec::new(),
            resets: 0,
        }
    }

    fn batched(newest: impl IntoIterator<Item = [f32; 4]>, batch_size: usize) -> Self {
        Self {
            newest: newest
                .into_iter()
                .map(|probabilities| probabilities[0])
                .collect(),
            batch_size,
            pending: Vec::with_capacity(batch_size),
            resets: 0,
        }
    }
}

impl VadEngine for FakeVad {
    fn analyze_frame(&mut self, _samples: &[f32]) -> Result<Vec<f32>, String> {
        self.pending.push(self.newest.pop_front().unwrap_or(0.0));
        if self.pending.len() < self.batch_size {
            return Ok(Vec::new());
        }
        Ok(std::mem::take(&mut self.pending))
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.resets += 1;
    }
}

#[test]
fn automatic_endpoint_keeps_preroll_and_finishes_after_silence() {
    let mut detector = EndpointDetector::new();
    let mut vad = FakeVad::new(
        std::iter::repeat_n([0.0; 4], 11)
            .chain(std::iter::repeat_n([0.9; 4], 5))
            .chain(std::iter::repeat_n([0.8; 4], 4))
            .chain(std::iter::repeat_n([0.0; 4], 32)),
    );
    let events = detector
        .push(&vec![0.1; 8_192], &mut vad)
        .expect("预热窗口应当可处理");
    assert!(events.is_empty(), "最短人声前不得发布不可逆句首事件");
    let events = detector
        .push(&vec![0.3; 2_048], &mut vad)
        .expect("持续人声应当可处理");
    assert!(matches!(events.as_slice(), [EndpointEvent::Started]));

    let mut completed = None;
    for _ in 0..8 {
        for event in detector
            .push(&vec![0.0; 2_048], &mut vad)
            .expect("尾部静音应当可处理")
        {
            if let EndpointEvent::Complete(samples) = event {
                completed = Some(samples);
            }
        }
    }
    let completed = completed.expect("足够静音后应当结束录音");
    assert!(completed.len() >= 8_000, "录音必须保留句首预录音频");
    assert_eq!(vad.resets, 1, "每段录音结束后必须清空滚动窗口");
}

#[test]
fn subminimum_false_positive_does_not_create_a_transcription() {
    let mut detector = EndpointDetector::new();
    let mut vad = FakeVad::new(
        std::iter::repeat_n([0.0; 4], 11)
            .chain(std::iter::repeat_n([0.9; 4], 5))
            .chain(std::iter::repeat_n([0.0; 4], 32)),
    );
    let mut events = detector
        .push(&vec![0.0; 8_192], &mut vad)
        .expect("起点窗口应当可处理");
    for _ in 0..8 {
        events.extend(
            detector
                .push(&vec![0.0; 2_048], &mut vad)
                .expect("静音窗口应当可处理"),
        );
    }

    assert!(
        events
            .iter()
            .all(|event| !matches!(event, EndpointEvent::Complete(_)))
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, EndpointEvent::Started))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EndpointEvent::Discarded))
    );
    assert_eq!(vad.resets, 1);
}

#[test]
fn delayed_probability_batches_remain_aligned_with_audio() {
    let mut detector = EndpointDetector::new();
    let mut vad = FakeVad::batched(
        std::iter::repeat_n([0.0; 4], 11)
            .chain(std::iter::repeat_n([0.9; 4], 9))
            .chain(std::iter::repeat_n([0.0; 4], 32)),
        8,
    );

    let mut events = Vec::new();
    for _ in 0..7 {
        events.extend(
            detector
                .push(&vec![0.2; 8 * 512], &mut vad)
                .expect("延迟概率批次应当与待分析音频对齐"),
        );
    }

    assert!(
        events
            .iter()
            .any(|event| matches!(event, EndpointEvent::Started))
    );
    let completed = events
        .into_iter()
        .find_map(|event| match event {
            EndpointEvent::Complete(samples) => Some(samples),
            EndpointEvent::Started | EndpointEvent::Discarded => None,
        })
        .expect("足够的批量静音概率应当结束录音");
    assert!(completed.len() >= 8_000, "延迟分析仍必须保留句首预录");
    assert_eq!(vad.resets, 1);
}

#[test]
fn a_discarded_false_start_does_not_drop_later_audio_from_the_same_batch() {
    let mut detector = EndpointDetector::new();
    let mut vad = FakeVad::new(
        std::iter::repeat_n([0.0; 4], 11)
            .chain(std::iter::repeat_n([0.9; 4], 5))
            .chain(std::iter::repeat_n([0.0; 4], 29))
            .chain(std::iter::repeat_n([0.9; 4], 8)),
    );

    let events = detector
        .push(&vec![0.0; 53 * 512], &mut vad)
        .expect("同批次中的第二段人声应当继续分析");

    assert!(matches!(
        events.as_slice(),
        [EndpointEvent::Discarded, EndpointEvent::Started]
    ));
    assert_eq!(vad.resets, 1);
}

#[test]
fn manual_takeover_keeps_the_active_recording_and_unanalyzed_tail() {
    let mut detector = EndpointDetector::new();
    let mut vad = FakeVad::batched(
        std::iter::repeat_n([0.0; 4], 11).chain(std::iter::repeat_n([0.9; 4], 13)),
        8,
    );

    let initial = detector
        .push(&vec![0.1; 16 * 512], &mut vad)
        .expect("候选录音应当可以建立");
    assert!(initial.is_empty());
    let started = detector
        .push(&vec![0.2; 8 * 512], &mut vad)
        .expect("足够人声应当确认录音");
    assert!(matches!(started.as_slice(), [EndpointEvent::Started]));

    let mut trailing = vec![0.7; 3 * 512];
    trailing.extend(std::iter::repeat_n(0.8, 123));
    assert!(
        detector
            .push(&trailing, &mut vad)
            .expect("待分析尾音应当进入有界缓冲")
            .is_empty()
    );

    let samples = detector
        .take_recording_for_manual()
        .expect("手动录音应当接管当前 VAD 录音");
    let tail_start = samples
        .len()
        .checked_sub(trailing.len())
        .expect("接管结果必须包含全部尾音");
    assert_eq!(&samples[tail_start..], trailing.as_slice());
    assert!(detector.take_recording_for_manual().is_none());
}
