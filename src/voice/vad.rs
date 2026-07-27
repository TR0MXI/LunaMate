//! 使用安全滚动窗口运行 Silero VAD，并实现有预录与迟滞的自动端点检测。

use std::{collections::VecDeque, path::Path};

use whisper_rs::{WhisperVadContext, WhisperVadContextParams};

const SAMPLE_RATE: usize = 16_000;
const SILERO_FRAME_SAMPLES: usize = 512;
const ROLLING_CONTEXT_FRAMES: usize = 32;
const ROLLING_HOP_FRAMES: usize = 8;
const ROLLING_CONTEXT_SAMPLES: usize = ROLLING_CONTEXT_FRAMES * SILERO_FRAME_SAMPLES;
const PRE_ROLL_SAMPLES: usize = SAMPLE_RATE / 2;
const START_THRESHOLD: f32 = 0.55;
const CONTINUE_THRESHOLD: f32 = 0.35;
const START_HISTORY_FRAMES: usize = 5;
const START_REQUIRED_FRAMES: usize = 3;
const END_SILENCE_SAMPLES: usize = SAMPLE_RATE * 9 / 10;
const MIN_SPEECH_SAMPLES: usize = SAMPLE_RATE / 4;
pub(super) const MAX_UTTERANCE_SAMPLES: usize = SAMPLE_RATE * 30;

pub(super) trait VadEngine {
    /// 接收一个 512-sample 帧，并返回本次已经完成分析的连续帧概率。
    fn analyze_frame(&mut self, samples: &[f32]) -> Result<Vec<f32>, String>;
    fn reset(&mut self);
}

/// 每 256 ms 重算最近约一秒上下文，避免依赖跨调用保留的原生 LSTM 状态。
pub(super) struct RollingVad {
    context: WhisperVadContext,
    rolling_samples: VecDeque<f32>,
    frames_since_analysis: usize,
}

impl RollingVad {
    pub(super) fn load(path: &Path) -> Result<Self, String> {
        let path = path
            .to_str()
            .ok_or_else(|| "Silero VAD 模型路径不是有效的 UTF-8".to_owned())?;
        let mut params = WhisperVadContextParams::default();
        params.set_n_threads(1);
        // whisper.cpp 1.8.3 内部强制 VAD 使用 CPU；不要把请求偏好误报为实际后端。
        params.set_use_gpu(false);
        params.set_gpu_device(0);
        let context = WhisperVadContext::new(path, params)
            .map_err(|error| format!("无法加载 Silero VAD 模型：{error}"))?;
        Ok(Self {
            context,
            rolling_samples: VecDeque::with_capacity(ROLLING_CONTEXT_SAMPLES),
            frames_since_analysis: 0,
        })
    }
}

impl VadEngine for RollingVad {
    fn analyze_frame(&mut self, samples: &[f32]) -> Result<Vec<f32>, String> {
        if samples.len() != SILERO_FRAME_SAMPLES {
            return Err("Silero VAD 流式帧必须包含 512 个采样点".to_owned());
        }
        for sample in samples.iter().copied() {
            if self.rolling_samples.len() == ROLLING_CONTEXT_SAMPLES {
                self.rolling_samples.pop_front();
            }
            self.rolling_samples.push_back(sample);
        }

        self.frames_since_analysis += 1;
        if self.frames_since_analysis < ROLLING_HOP_FRAMES {
            return Ok(Vec::new());
        }
        self.frames_since_analysis = 0;

        let rolling_samples = self.rolling_samples.make_contiguous();
        self.context
            .detect_speech(rolling_samples)
            .map_err(|error| format!("Silero VAD 推理失败：{error}"))?;
        let probabilities = self.context.probabilities();
        if probabilities.len() < ROLLING_HOP_FRAMES {
            return Err("Silero VAD 返回的概率数量不足".to_owned());
        }
        Ok(probabilities[probabilities.len() - ROLLING_HOP_FRAMES..].to_vec())
    }

    fn reset(&mut self) {
        self.rolling_samples.clear();
        self.frames_since_analysis = 0;
    }
}

pub(super) enum EndpointEvent {
    Started,
    Complete(Vec<f32>),
    Discarded,
}

struct Recording {
    samples: Vec<f32>,
    silence_samples: usize,
    voiced_samples: usize,
    started: bool,
}

/// 在 16 kHz PCM 流上维护预录、重叠分析窗口和一段活动录音。
pub(super) struct EndpointDetector {
    frame: Vec<f32>,
    pending_samples: VecDeque<f32>,
    pre_roll: VecDeque<f32>,
    recent_probabilities: VecDeque<f32>,
    recording: Option<Recording>,
}

impl EndpointDetector {
    pub(super) fn new() -> Self {
        Self {
            frame: Vec::with_capacity(SILERO_FRAME_SAMPLES),
            pending_samples: VecDeque::with_capacity(ROLLING_HOP_FRAMES * SILERO_FRAME_SAMPLES),
            pre_roll: VecDeque::with_capacity(PRE_ROLL_SAMPLES),
            recent_probabilities: VecDeque::with_capacity(START_HISTORY_FRAMES),
            recording: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.frame.clear();
        self.pending_samples.clear();
        self.pre_roll.clear();
        self.recent_probabilities.clear();
        self.recording = None;
    }

    /// 把 VAD 候选或活动录音交给手动录音，并保留尚未返回概率的尾部 PCM。
    pub(super) fn take_recording_for_manual(&mut self) -> Option<Vec<f32>> {
        let samples = self.recording.take().map(|mut recording| {
            let remaining = MAX_UTTERANCE_SAMPLES.saturating_sub(recording.samples.len());
            recording.samples.extend(
                self.pending_samples
                    .iter()
                    .chain(self.frame.iter())
                    .copied()
                    .take(remaining),
            );
            recording.samples
        });
        self.reset();
        samples
    }

    pub(super) fn push(
        &mut self,
        samples: &[f32],
        vad: &mut impl VadEngine,
    ) -> Result<Vec<EndpointEvent>, String> {
        let mut events = Vec::with_capacity(2);
        let mut offset = 0;
        while offset < samples.len() {
            let take =
                (SILERO_FRAME_SAMPLES - self.frame.len()).min(samples.len().saturating_sub(offset));
            let chunk = &samples[offset..offset + take];
            self.frame.extend_from_slice(chunk);
            offset += take;
            if self.frame.len() != SILERO_FRAME_SAMPLES {
                continue;
            }
            let probabilities = vad.analyze_frame(&self.frame)?;
            self.pending_samples.extend(self.frame.drain(..));
            if self.pending_samples.len() > ROLLING_CONTEXT_SAMPLES {
                return Err("Silero VAD 未在有界窗口内返回概率".to_owned());
            }
            for probability in probabilities {
                self.append_pending_frame()?;
                if self.observe_probability(probability, &mut events) {
                    vad.reset();
                    if matches!(events.last(), Some(EndpointEvent::Complete(_))) {
                        return Ok(events);
                    }
                }
            }
        }
        Ok(events)
    }

    fn append_pending_frame(&mut self) -> Result<(), String> {
        if self.pending_samples.len() < SILERO_FRAME_SAMPLES {
            return Err("Silero VAD 返回了多于待分析音频的概率".to_owned());
        }
        for _ in 0..SILERO_FRAME_SAMPLES {
            let Some(sample) = self.pending_samples.pop_front() else {
                return Err("Silero VAD 音频与概率失去对齐".to_owned());
            };
            if let Some(recording) = &mut self.recording
                && recording.samples.len() < MAX_UTTERANCE_SAMPLES
            {
                recording.samples.push(sample);
            }
            if self.pre_roll.len() == PRE_ROLL_SAMPLES {
                self.pre_roll.pop_front();
            }
            self.pre_roll.push_back(sample);
        }
        Ok(())
    }

    /// 返回本次观察是否已经结束一段录音。
    fn observe_probability(&mut self, probability: f32, events: &mut Vec<EndpointEvent>) -> bool {
        if self.recording.is_none() {
            if self.recent_probabilities.len() == START_HISTORY_FRAMES {
                self.recent_probabilities.pop_front();
            }
            self.recent_probabilities.push_back(probability);
            let starts = self.recent_probabilities.len() == START_HISTORY_FRAMES
                && self
                    .recent_probabilities
                    .iter()
                    .filter(|probability| **probability >= START_THRESHOLD)
                    .count()
                    >= START_REQUIRED_FRAMES;
            if starts {
                let voiced_samples = self
                    .recent_probabilities
                    .iter()
                    .filter(|probability| **probability >= CONTINUE_THRESHOLD)
                    .count()
                    .saturating_mul(SILERO_FRAME_SAMPLES);
                self.recording = Some(Recording {
                    samples: self.pre_roll.iter().copied().collect(),
                    silence_samples: 0,
                    voiced_samples,
                    started: false,
                });
            }
            return false;
        }

        let Some(recording) = &mut self.recording else {
            return false;
        };
        if probability >= CONTINUE_THRESHOLD {
            recording.silence_samples = 0;
            recording.voiced_samples = recording
                .voiced_samples
                .saturating_add(SILERO_FRAME_SAMPLES);
        } else {
            recording.silence_samples = recording
                .silence_samples
                .saturating_add(SILERO_FRAME_SAMPLES);
        }
        if !recording.started && recording.voiced_samples >= MIN_SPEECH_SAMPLES {
            recording.started = true;
            events.push(EndpointEvent::Started);
        }
        let ended = recording.silence_samples >= END_SILENCE_SAMPLES
            || recording.samples.len() >= MAX_UTTERANCE_SAMPLES;
        if !ended {
            return false;
        }
        let Some(recording) = self.recording.take() else {
            return false;
        };
        if recording.voiced_samples >= MIN_SPEECH_SAMPLES {
            events.push(EndpointEvent::Complete(recording.samples));
        } else {
            events.push(EndpointEvent::Discarded);
        }
        self.recent_probabilities.clear();
        true
    }
}
