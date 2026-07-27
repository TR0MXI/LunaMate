//! 串行拥有麦克风与 VAD context，并把阻塞转写交给独立工作线程。

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    thread::{self, JoinHandle},
};

use async_channel::{Receiver, Sender};

use crate::config::{SharedVoiceSettings, VoiceMode};

use super::{
    VoiceActivity, VoiceCommand, VoiceEvent, VoicePhase,
    capture::{Capture, DrainOutcome},
    transcribe::{
        self, MAX_WHISPER_MODEL_BYTES, TranscriptionJob, TranscriptionQueue, TranscriptionResult,
        validate_model_file,
    },
    vad::{EndpointDetector, EndpointEvent, MAX_UTTERANCE_SAMPLES, RollingVad, VadEngine as _},
};

const MAX_VAD_MODEL_BYTES: u64 = 64 * 1024 * 1024;
const MIN_MANUAL_SAMPLES: usize = 16_000 / 4;

pub(super) fn spawn(
    settings: SharedVoiceSettings,
    commands: Receiver<VoiceCommand>,
    command_sender: Sender<VoiceCommand>,
    events: Sender<VoiceEvent>,
    activity: Arc<VoiceActivity>,
    desired_revision: Arc<AtomicU64>,
    completion: SyncSender<()>,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("lunamate-voice".to_owned())
        .spawn(move || {
            let transcription_queue = TranscriptionQueue::new(desired_revision.clone());
            let asr_commands = command_sender.clone();
            let asr_queue = transcription_queue.clone();
            let asr_worker = thread::Builder::new()
                .name("lunamate-whisper".to_owned())
                .spawn(move || transcribe::run(asr_queue, asr_commands));
            let Ok(asr_worker) = asr_worker else {
                publish_error(
                    &events,
                    &activity,
                    1,
                    "无法启动 Whisper 工作线程".to_owned(),
                );
                let _ = completion.try_send(());
                return;
            };
            let mut worker = Worker::new(
                settings,
                commands,
                command_sender,
                events,
                activity,
                desired_revision,
                transcription_queue,
            );
            worker.configure_current(1);
            worker.run();
            worker.transcription_queue.shutdown();
            let _ = asr_worker.join();
            let _ = completion.try_send(());
        })
        .map_err(|error| format!("无法启动语音工作线程：{error}"))
}

struct Worker {
    settings: SharedVoiceSettings,
    revision: u64,
    commands: Receiver<VoiceCommand>,
    command_sender: Sender<VoiceCommand>,
    events: Sender<VoiceEvent>,
    activity: Arc<VoiceActivity>,
    desired_revision: Arc<AtomicU64>,
    transcription_queue: Arc<TranscriptionQueue>,
    capture: Option<Capture>,
    active_capture_id: Option<u64>,
    next_capture_id: u64,
    vad: Option<RollingVad>,
    endpoint: EndpointDetector,
    manual_samples: Option<Vec<f32>>,
    active_utterance: Option<u64>,
    next_utterance_id: u64,
    normalized: Vec<f32>,
    transcription_pending: bool,
    transcription_utterance: Option<u64>,
}

impl Worker {
    fn new(
        settings: SharedVoiceSettings,
        commands: Receiver<VoiceCommand>,
        command_sender: Sender<VoiceCommand>,
        events: Sender<VoiceEvent>,
        activity: Arc<VoiceActivity>,
        desired_revision: Arc<AtomicU64>,
        transcription_queue: Arc<TranscriptionQueue>,
    ) -> Self {
        Self {
            settings,
            revision: 1,
            commands,
            command_sender,
            events,
            activity,
            desired_revision,
            transcription_queue,
            capture: None,
            active_capture_id: None,
            next_capture_id: 0,
            vad: None,
            endpoint: EndpointDetector::new(),
            manual_samples: None,
            active_utterance: None,
            next_utterance_id: 0,
            normalized: Vec::with_capacity(4_096),
            transcription_pending: false,
            transcription_utterance: None,
        }
    }

    fn run(&mut self) {
        while let Ok(command) = self.commands.recv_blocking() {
            match command {
                VoiceCommand::Configure { revision, settings } => {
                    if revision > self.revision
                        && revision == self.desired_revision.load(Ordering::Acquire)
                    {
                        self.settings = settings;
                        self.configure_current(revision);
                    }
                }
                VoiceCommand::PushToTalk { revision, pressed }
                    if revision == self.revision && self.is_current() =>
                {
                    self.set_push_to_talk(pressed);
                }
                VoiceCommand::PushToTalk { .. } => {}
                VoiceCommand::AudioReady {
                    revision,
                    capture_id,
                } if self.accepts_capture_event(revision, capture_id) => {
                    self.consume_audio();
                }
                VoiceCommand::AudioReady { .. } => {}
                VoiceCommand::CaptureFailed {
                    revision,
                    capture_id,
                    message,
                } if self.accepts_capture_event(revision, capture_id) => {
                    self.fail(format!("麦克风采集失败：{message}"));
                }
                VoiceCommand::CaptureFailed { .. } => {}
                VoiceCommand::TranscriptionFinished(result) => {
                    self.finish_transcription(result);
                }
                VoiceCommand::Shutdown => break,
            }
        }
        self.stop_capture();
        self.cancel_active_utterance();
        self.cancel_transcription_utterance();
        self.transcription_pending = false;
        self.transcription_queue.cancel_pending();
        self.vad = None;
        self.publish_phase(VoicePhase::Off);
    }

    fn configure_current(&mut self, revision: u64) {
        if self.desired_revision.load(Ordering::Acquire) != revision {
            return;
        }
        self.stop_capture();
        self.transcription_queue.cancel_pending();
        self.cancel_active_utterance();
        self.cancel_transcription_utterance();
        self.revision = revision;
        self.vad = None;
        self.endpoint.reset();
        self.manual_samples = None;
        self.transcription_pending = false;
        self.normalized.clear();

        match self.settings.mode {
            VoiceMode::Off => self.publish_phase(VoicePhase::Off),
            VoiceMode::PushToTalk => {
                let Some(path) = self.settings.whisper_model.as_deref() else {
                    self.fail("按住说话模式需要先选择 Whisper 模型".to_owned());
                    return;
                };
                if let Err(error) = validate_model_file(path, MAX_WHISPER_MODEL_BYTES, "Whisper") {
                    self.fail(error);
                    return;
                }
                self.publish_phase(VoicePhase::Listening);
            }
            VoiceMode::Auto | VoiceMode::Mixed => {
                if let Err(error) = self.prepare_auto_vad() {
                    self.fail(error);
                    return;
                }
                if let Err(error) = self.start_capture() {
                    self.fail(error);
                    return;
                }
                self.publish_phase(VoicePhase::Listening);
            }
        }
    }

    fn prepare_auto_vad(&mut self) -> Result<(), String> {
        let whisper_path = self
            .settings
            .whisper_model
            .as_deref()
            .ok_or_else(|| "自动或混合语音模式需要先选择 Whisper 模型".to_owned())?;
        validate_model_file(whisper_path, MAX_WHISPER_MODEL_BYTES, "Whisper")?;
        let vad_path = self
            .settings
            .vad_model
            .as_deref()
            .ok_or_else(|| "自动或混合语音模式需要先选择 Silero VAD 模型".to_owned())?;
        validate_model_file(vad_path, MAX_VAD_MODEL_BYTES, "Silero VAD")?;
        self.vad = Some(RollingVad::load(vad_path)?);
        Ok(())
    }

    fn start_capture(&mut self) -> Result<(), String> {
        if self.capture.is_none() {
            self.next_capture_id = self.next_capture_id.wrapping_add(1).max(1);
            let capture_id = self.next_capture_id;
            let capture = Capture::start(self.revision, capture_id, self.command_sender.clone())?;
            self.active_capture_id = Some(capture_id);
            self.capture = Some(capture);
        }
        Ok(())
    }

    fn stop_capture(&mut self) -> Option<Capture> {
        self.active_capture_id = None;
        self.capture.take()
    }

    fn accepts_capture_event(&self, revision: u64, capture_id: u64) -> bool {
        revision == self.revision && self.active_capture_id == Some(capture_id) && self.is_current()
    }

    fn consume_audio(&mut self) {
        let Some(capture) = &mut self.capture else {
            return;
        };
        let outcome = match capture.drain_into(&mut self.normalized) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.fail(error);
                return;
            }
        };
        if matches!(outcome, DrainOutcome::Discontinuous) {
            self.handle_audio_discontinuity();
            return;
        }
        if self.normalized.is_empty() {
            return;
        }
        self.activity.set_level(audio_level(&self.normalized));
        match self.settings.mode {
            VoiceMode::Auto => self.consume_auto_audio(),
            VoiceMode::Mixed if self.manual_samples.is_some() => self.consume_manual_audio(),
            VoiceMode::Mixed => self.consume_auto_audio(),
            VoiceMode::PushToTalk => self.consume_manual_audio(),
            VoiceMode::Off => {}
        }
    }

    fn consume_auto_audio(&mut self) {
        let Some(vad) = &mut self.vad else {
            self.fail("Silero VAD context 已不可用".to_owned());
            return;
        };
        let events = match self.endpoint.push(&self.normalized, vad) {
            Ok(events) => events,
            Err(error) => {
                self.fail(error);
                return;
            }
        };
        for event in events {
            match event {
                EndpointEvent::Started => self.begin_utterance(),
                EndpointEvent::Complete(samples) => {
                    self.dispatch_transcription(samples);
                    break;
                }
                EndpointEvent::Discarded => {
                    self.cancel_active_utterance();
                    self.publish_phase(VoicePhase::Listening);
                }
            }
        }
    }

    fn consume_manual_audio(&mut self) {
        let (confirm, finish) = {
            let Some(samples) = &mut self.manual_samples else {
                return;
            };
            let remaining = MAX_UTTERANCE_SAMPLES.saturating_sub(samples.len());
            samples.extend_from_slice(&self.normalized[..self.normalized.len().min(remaining)]);
            (
                self.active_utterance.is_none() && samples.len() >= MIN_MANUAL_SAMPLES,
                samples.len() >= MAX_UTTERANCE_SAMPLES,
            )
        };
        if confirm {
            self.begin_utterance();
        }
        if finish {
            self.finish_manual_recording();
        }
    }

    fn set_push_to_talk(&mut self, pressed: bool) {
        if !self.settings.mode.supports_push_to_talk() || self.transcription_pending {
            return;
        }
        if pressed {
            if self.manual_samples.is_some() {
                return;
            }
            if self.settings.mode.uses_vad() && self.vad.is_none() {
                return;
            }
            if let Err(error) = self.start_capture() {
                self.fail(error);
                return;
            }
            let samples = if self.settings.mode.uses_vad() {
                let samples = self.endpoint.take_recording_for_manual();
                if let Some(vad) = &mut self.vad {
                    vad.reset();
                }
                samples.unwrap_or_else(|| Vec::with_capacity(16_000 * 8))
            } else {
                Vec::with_capacity(16_000 * 8)
            };
            let confirm = samples.len() >= MIN_MANUAL_SAMPLES;
            self.manual_samples = Some(samples);
            if confirm && self.active_utterance.is_none() {
                self.begin_utterance();
            } else {
                self.publish_phase(VoicePhase::Recording);
            }
        } else {
            self.finish_manual_recording();
        }
    }

    fn begin_utterance(&mut self) {
        if self.active_utterance.is_some() {
            return;
        }
        self.next_utterance_id = self.next_utterance_id.wrapping_add(1).max(1);
        let utterance_id = self.next_utterance_id;
        self.active_utterance = Some(utterance_id);
        self.publish_phase(VoicePhase::Recording);
        self.publish_event(VoiceEvent::SpeechStarted {
            revision: self.revision,
            utterance_id,
        });
    }

    fn finish_manual_recording(&mut self) {
        if self.manual_samples.is_none() {
            return;
        }
        let mut tail = Vec::new();
        if let Some(capture) = self.stop_capture() {
            match capture.finish_into(&mut tail) {
                Ok(DrainOutcome::Continuous) => {}
                Ok(DrainOutcome::Discontinuous) => {
                    self.handle_audio_discontinuity();
                    return;
                }
                Err(error) => {
                    self.fail(error);
                    return;
                }
            }
        }
        let Some(mut samples) = self.manual_samples.take() else {
            return;
        };
        let remaining = MAX_UTTERANCE_SAMPLES.saturating_sub(samples.len());
        samples.extend_from_slice(&tail[..tail.len().min(remaining)]);
        if samples.len() < MIN_MANUAL_SAMPLES {
            self.cancel_active_utterance();
            if self.settings.mode.uses_vad()
                && let Err(error) = self.resume_automatic_capture()
            {
                self.fail(error);
                return;
            }
            self.publish_phase(VoicePhase::Listening);
            return;
        }
        if self.active_utterance.is_none() {
            self.begin_utterance();
        }
        self.dispatch_transcription(samples);
    }

    fn dispatch_transcription(&mut self, samples: Vec<f32>) {
        let Some(utterance_id) = self.active_utterance.take() else {
            return;
        };
        self.stop_capture();
        self.transcription_pending = true;
        self.transcription_utterance = Some(utterance_id);
        self.publish_phase(VoicePhase::Transcribing);
        let job = TranscriptionJob {
            revision: self.revision,
            utterance_id,
            samples,
            settings: self.settings.clone(),
        };
        if !self.transcription_queue.submit(job) {
            self.transcription_pending = false;
            self.transcription_utterance = None;
            if self.is_current() {
                self.fail("Whisper 转写任务已被配置切换取消".to_owned());
            }
        }
    }

    fn finish_transcription(&mut self, result: TranscriptionResult) {
        if result.revision != self.revision
            || !self.is_current()
            || !self.transcription_pending
            || self.transcription_utterance != Some(result.utterance_id)
        {
            return;
        }
        self.transcription_pending = false;
        self.transcription_utterance = None;
        match result.result {
            Ok(text) => {
                self.publish_event(VoiceEvent::TranscriptReady {
                    revision: self.revision,
                    utterance_id: result.utterance_id,
                    text,
                });
            }
            Err(error) => {
                self.publish_event(VoiceEvent::Error {
                    revision: self.revision,
                    message: error,
                });
            }
        }
        if self.settings.mode.uses_vad()
            && let Err(error) = self.resume_automatic_capture()
        {
            self.fail(error);
            return;
        }
        self.publish_phase(VoicePhase::Listening);
    }

    fn resume_automatic_capture(&mut self) -> Result<(), String> {
        self.endpoint.reset();
        if let Some(vad) = &mut self.vad {
            vad.reset();
        }
        self.start_capture()
    }

    fn publish_phase(&self, phase: VoicePhase) {
        if !self.is_current() {
            return;
        }
        self.activity.set_phase(phase);
        self.publish_event(VoiceEvent::ActivityChanged {
            revision: self.revision,
        });
    }

    fn cancel_active_utterance(&mut self) {
        if let Some(utterance_id) = self.active_utterance.take() {
            self.publish_event(VoiceEvent::UtteranceCancelled {
                revision: self.revision,
                utterance_id,
            });
        }
    }

    fn cancel_transcription_utterance(&mut self) {
        if let Some(utterance_id) = self.transcription_utterance.take() {
            self.publish_event(VoiceEvent::UtteranceCancelled {
                revision: self.revision,
                utterance_id,
            });
        }
    }

    fn fail(&mut self, message: String) {
        self.stop_capture();
        self.vad = None;
        self.manual_samples = None;
        self.cancel_active_utterance();
        self.cancel_transcription_utterance();
        self.transcription_pending = false;
        self.publish_phase(VoicePhase::Error);
        self.publish_event(VoiceEvent::Error {
            revision: self.revision,
            message,
        });
    }

    fn handle_audio_discontinuity(&mut self) {
        self.normalized.clear();
        self.endpoint.reset();
        if let Some(vad) = &mut self.vad {
            vad.reset();
        }
        self.manual_samples = None;
        self.cancel_active_utterance();
        if self.settings.mode == VoiceMode::PushToTalk {
            self.stop_capture();
        } else if self.settings.mode.uses_vad()
            && self.capture.is_none()
            && let Err(error) = self.start_capture()
        {
            self.fail(error);
            return;
        }
        self.publish_phase(VoicePhase::Listening);
        self.publish_event(VoiceEvent::Error {
            revision: self.revision,
            message: "麦克风缓冲溢出，本段录音已取消".to_owned(),
        });
    }

    fn is_current(&self) -> bool {
        self.desired_revision.load(Ordering::Acquire) == self.revision
    }

    fn publish_event(&self, event: VoiceEvent) {
        if self.is_current() {
            let _ = self.events.try_send(event);
        }
    }
}

fn audio_level(samples: &[f32]) -> f32 {
    let mean_square = samples
        .iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .map(|sample| sample * sample)
        .sum::<f32>()
        / samples.len().max(1) as f32;
    (mean_square.sqrt() * 4.0).clamp(0.0, 1.0)
}

fn publish_error(
    events: &Sender<VoiceEvent>,
    activity: &VoiceActivity,
    revision: u64,
    message: String,
) {
    activity.set_phase(VoicePhase::Error);
    let _ = events.try_send(VoiceEvent::ActivityChanged { revision });
    let _ = events.try_send(VoiceEvent::Error { revision, message });
}
