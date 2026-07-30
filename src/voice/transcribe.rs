//! 在专用 OS 线程中复用 Whisper 模型，并把单段 PCM 转换为有界文本。

use std::{
    ffi::c_void,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use async_channel::Sender;
use parking_lot::{Condvar, Mutex};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::config::{SharedVoiceRuntimeSettings, VoiceTranscriptionBackend};

use super::VoiceCommand;

pub(super) const MAX_WHISPER_MODEL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub(super) struct TranscriptionJob {
    pub(super) revision: u64,
    pub(super) utterance_id: u64,
    pub(super) samples: Vec<f32>,
    pub(super) settings: SharedVoiceRuntimeSettings,
}

pub(super) struct TranscriptionResult {
    pub(super) revision: u64,
    pub(super) utterance_id: u64,
    pub(super) result: Result<String, String>,
}

struct QueueState {
    pending: Option<TranscriptionJob>,
    shutdown: bool,
}

/// 只保留最新待处理 utterance；配置切换不会让旧任务占住单槽队列。
pub(super) struct TranscriptionQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
    desired_revision: Arc<AtomicU64>,
}

impl TranscriptionQueue {
    pub(super) fn new(desired_revision: Arc<AtomicU64>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(QueueState {
                pending: None,
                shutdown: false,
            }),
            changed: Condvar::new(),
            desired_revision,
        })
    }

    pub(super) fn submit(&self, job: TranscriptionJob) -> bool {
        if self.is_cancelled(job.revision) {
            return false;
        }
        let mut state = self.state.lock();
        if state.shutdown || self.desired_revision.load(Ordering::Acquire) != job.revision {
            return false;
        }
        state.pending = Some(job);
        self.changed.notify_one();
        true
    }

    pub(super) fn cancel_pending(&self) {
        self.state.lock().pending = None;
    }

    pub(super) fn shutdown(&self) {
        let mut state = self.state.lock();
        state.pending = None;
        state.shutdown = true;
        self.changed.notify_all();
    }

    fn is_cancelled(&self, revision: u64) -> bool {
        self.desired_revision.load(Ordering::Acquire) != revision || self.state.lock().shutdown
    }

    fn next(&self) -> Option<TranscriptionJob> {
        let mut state = self.state.lock();
        loop {
            if state.shutdown {
                return None;
            }
            if let Some(job) = state.pending.take() {
                return Some(job);
            }
            self.changed.wait(&mut state);
        }
    }

    #[cfg(test)]
    pub(super) fn take_pending_for_test(&self) -> Option<TranscriptionJob> {
        self.state.lock().pending.take()
    }
}

pub(super) fn run(queue: Arc<TranscriptionQueue>, voice_commands: Sender<VoiceCommand>) {
    let mut transcriber = Transcriber::default();
    while let Some(job) = queue.next() {
        if queue.is_cancelled(job.revision) {
            continue;
        }
        let started = Instant::now();
        let sample_count = job.samples.len();
        let result = transcriber.transcribe(&job, &queue);
        if queue.is_cancelled(job.revision) {
            log::debug!(
                "丢弃已过期的 Whisper 结果：revision={}, utterance_id={}",
                job.revision,
                job.utterance_id
            );
            continue;
        }
        match &result {
            Ok(text) => log::debug!(
                "Whisper 推理完成：revision={}, utterance_id={}, samples={sample_count}, transcript_bytes={}, elapsed_ms={}",
                job.revision,
                job.utterance_id,
                text.len(),
                started.elapsed().as_millis()
            ),
            Err(_) => log::debug!(
                "Whisper 推理失败：revision={}, utterance_id={}, samples={sample_count}, elapsed_ms={}",
                job.revision,
                job.utterance_id,
                started.elapsed().as_millis()
            ),
        }
        let _ = voice_commands.try_send(VoiceCommand::TranscriptionFinished(TranscriptionResult {
            revision: job.revision,
            utterance_id: job.utterance_id,
            result,
        }));
    }
}

#[derive(Default)]
struct Transcriber {
    context: Option<WhisperContext>,
    loaded_path: Option<std::path::PathBuf>,
    loaded_for_gpu_request: bool,
    loaded_with_gpu: bool,
}

impl Transcriber {
    fn transcribe(
        &mut self,
        job: &TranscriptionJob,
        queue: &TranscriptionQueue,
    ) -> Result<String, String> {
        let path = match job.settings.backend.as_ref() {
            Some(VoiceTranscriptionBackend::LocalWhisper(path)) => path.as_path(),
            Some(VoiceTranscriptionBackend::Remote(_)) | None => {
                return Err("当前转写任务不是本地 Whisper".to_owned());
            }
        };
        validate_model_file(path, MAX_WHISPER_MODEL_BYTES, "Whisper")?;
        let request_gpu = job.settings.use_gpu;
        self.ensure_context(path, request_gpu)?;
        let ran_on_gpu = self.loaded_with_gpu;
        match self.run_once(
            &job.samples,
            job.settings.whisper_language.as_deref(),
            queue,
            job.revision,
        ) {
            Ok(text) => Ok(text),
            Err(RunError::Inference(_)) if ran_on_gpu => {
                if queue.is_cancelled(job.revision) {
                    return Err("Whisper 转写已取消".to_owned());
                }
                log::warn!(
                    "Whisper GPU 推理失败，正在回退 CPU：revision={}, utterance_id={}, stage=inference",
                    job.revision,
                    job.utterance_id
                );
                self.context = None;
                self.loaded_path = None;
                self.ensure_context(path, false)?;
                // 同一模型和 GPU 偏好后续复用本次 CPU 回退，不在每个 utterance 重试坏设备。
                self.loaded_for_gpu_request = true;
                self.run_once(
                    &job.samples,
                    job.settings.whisper_language.as_deref(),
                    queue,
                    job.revision,
                )
                .map_err(RunError::into_string)
            }
            Err(error) => Err(error.into_string()),
        }
    }

    fn ensure_context(&mut self, path: &Path, use_gpu: bool) -> Result<(), String> {
        if self.context.is_some()
            && self.loaded_path.as_deref() == Some(path)
            && self.loaded_for_gpu_request == use_gpu
        {
            return Ok(());
        }
        let mut parameters = WhisperContextParameters::default();
        parameters.use_gpu(use_gpu);
        let (context, loaded_with_gpu) = match WhisperContext::new_with_params(path, parameters) {
            Ok(context) => (context, use_gpu),
            Err(_) if use_gpu => {
                log::warn!("Whisper GPU 初始化失败，正在回退 CPU：stage=model_init");
                let mut cpu_parameters = WhisperContextParameters::default();
                cpu_parameters.use_gpu(false);
                (
                    WhisperContext::new_with_params(path, cpu_parameters)
                        .map_err(|error| format!("无法加载 Whisper 模型：{error}"))?,
                    false,
                )
            }
            Err(error) => return Err(format!("无法加载 Whisper 模型：{error}")),
        };
        self.loaded_path = Some(path.to_path_buf());
        self.loaded_for_gpu_request = use_gpu;
        self.loaded_with_gpu = loaded_with_gpu;
        self.context = Some(context);
        Ok(())
    }

    fn run_once(
        &self,
        samples: &[f32],
        language: Option<&str>,
        queue: &TranscriptionQueue,
        revision: u64,
    ) -> Result<String, RunError> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| RunError::Inference("Whisper context 尚未初始化".to_owned()))?;
        let mut state = context
            .create_state()
            .map_err(|error| RunError::Inference(format!("无法创建 Whisper 推理状态：{error}")))?;
        let mut parameters = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        parameters.set_n_threads(inference_threads());
        // whisper-rs 默认语言为英语；显式传入 None 才会走 Whisper 的自动识别路径。
        parameters.set_language(language);
        parameters.set_translate(false);
        parameters.set_no_context(true);
        parameters.set_print_progress(false);
        parameters.set_print_realtime(false);
        parameters.set_print_special(false);
        parameters.set_print_timestamps(false);
        parameters.set_suppress_blank(true);
        let cancellation = InferenceCancellation { queue, revision };
        // SAFETY: `cancellation` 在同步 `state.full` 返回前保持固定地址；回调只读取线程安全的
        // revision/关闭状态，whisper.cpp 不会在 `full` 返回后继续调用本次参数中的回调。
        unsafe {
            parameters.set_abort_callback(Some(abort_inference));
            parameters.set_abort_callback_user_data(
                std::ptr::from_ref(&cancellation)
                    .cast_mut()
                    .cast::<c_void>(),
            );
        }
        state
            .full(parameters, samples)
            .map_err(|error| RunError::Inference(format!("Whisper 转写失败：{error}")))?;

        let mut text = String::new();
        for segment in state.as_iter() {
            let segment = segment
                .to_str_lossy()
                .map_err(|error| RunError::Output(format!("Whisper 文本读取失败：{error}")))?;
            text.push_str(&segment);
        }
        let text = text.trim();
        if text.is_empty() {
            Err(RunError::Output("没有识别到可提交的语音文本".to_owned()))
        } else {
            Ok(text.to_owned())
        }
    }
}

struct InferenceCancellation<'a> {
    queue: &'a TranscriptionQueue,
    revision: u64,
}

unsafe extern "C" fn abort_inference(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return true;
    }
    // SAFETY: 指针由 `run_once` 中仍存活的 `InferenceCancellation` 创建，C 侧只在同步
    // `whisper_full_with_state` 调用期间借用它，且该回调不修改指针目标。
    let cancellation = unsafe { &*user_data.cast::<InferenceCancellation<'_>>() };
    cancellation.queue.is_cancelled(cancellation.revision)
}

enum RunError {
    Inference(String),
    Output(String),
}

impl RunError {
    fn into_string(self) -> String {
        match self {
            Self::Inference(message) | Self::Output(message) => message,
        }
    }
}

fn inference_threads() -> i32 {
    let available = std::thread::available_parallelism().map_or(2, usize::from);
    i32::try_from(available.clamp(1, 8)).unwrap_or(2)
}

pub(super) fn validate_model_file(path: &Path, maximum: u64, label: &str) -> Result<(), String> {
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("无法读取 {label} 模型：{error}"))?;
    if !metadata.is_file() {
        return Err(format!("{label} 模型路径不是普通文件"));
    }
    if metadata.len() == 0 || metadata.len() > maximum {
        return Err(format!("{label} 模型文件大小不在允许范围内"));
    }
    Ok(())
}
