//! 组合麦克风采集、Silero VAD、Whisper 转写与可取消的语音事件通路。

mod capture;
mod transcribe;
mod vad;
mod worker;

#[cfg(test)]
mod tests;

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use async_channel::{Receiver, Sender};

use crate::config::{SharedVoiceSettings, VoiceSettings};

/// 语音管线当前对用户可见的阶段。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum VoicePhase {
    #[default]
    Off,
    Listening,
    Recording,
    Transcribing,
    Error,
}

impl VoicePhase {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Listening => 1,
            Self::Recording => 2,
            Self::Transcribing => 3,
            Self::Error => 4,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Listening,
            2 => Self::Recording,
            3 => Self::Transcribing,
            4 => Self::Error,
            _ => Self::Off,
        }
    }
}

/// 供 UI 无锁读取的阶段与最新归一化音量。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct VoiceActivitySnapshot {
    pub(crate) phase: VoicePhase,
    pub(crate) level: f32,
}

#[derive(Default)]
struct VoiceActivity {
    phase: AtomicU8,
    level_bits: AtomicU32,
}

impl VoiceActivity {
    fn set_phase(&self, phase: VoicePhase) {
        self.phase.store(phase.as_u8(), Ordering::Release);
        if !matches!(phase, VoicePhase::Recording) {
            self.set_level(0.0);
        }
    }

    fn set_level(&self, level: f32) {
        self.level_bits
            .store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn snapshot(&self) -> VoiceActivitySnapshot {
        VoiceActivitySnapshot {
            phase: VoicePhase::from_u8(self.phase.load(Ordering::Acquire)),
            level: f32::from_bits(self.level_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0),
        }
    }
}

/// 后台语音管线向桌宠根视图发布的低频语义事件。
pub(crate) enum VoiceEvent {
    ActivityChanged {
        revision: u64,
    },
    SpeechStarted {
        revision: u64,
        utterance_id: u64,
    },
    UtteranceCancelled {
        revision: u64,
        utterance_id: u64,
    },
    TranscriptReady {
        revision: u64,
        utterance_id: u64,
        text: String,
    },
    Error {
        revision: u64,
        message: String,
    },
}

enum VoiceCommand {
    Configure {
        revision: u64,
        settings: SharedVoiceSettings,
    },
    PushToTalk {
        revision: u64,
        pressed: bool,
    },
    AudioReady {
        revision: u64,
        capture_id: u64,
    },
    CaptureFailed {
        revision: u64,
        capture_id: u64,
        message: String,
    },
    TranscriptionFinished(transcribe::TranscriptionResult),
    Shutdown,
}

/// 可克隆的前台控制端；不暴露音频设备、Whisper context 或 FFI 类型。
#[derive(Clone)]
pub(crate) struct VoiceController {
    commands: Sender<VoiceCommand>,
    events: Receiver<VoiceEvent>,
    activity: Arc<VoiceActivity>,
    revision: Arc<AtomicU64>,
}

impl VoiceController {
    /// 启动两个专用工作线程，并立即应用启动配置。
    pub(crate) fn start(settings: SharedVoiceSettings) -> Result<(Self, VoiceShutdown), String> {
        // AudioReady 在采集端已经合并，无界控制通道保证 GPUI 提交配置时不等待 worker。
        let (commands, command_receiver) = async_channel::unbounded();
        // 语义事件频率受端点状态机限制；无界通道避免 UI 停止消费时反向卡住麦克风释放。
        let (event_sender, events) = async_channel::unbounded();
        let activity = Arc::new(VoiceActivity::default());
        let revision = Arc::new(AtomicU64::new(1));
        let completion = std::sync::mpsc::sync_channel(1);
        let initial_mode = settings.mode.id();
        let whisper_configured = settings.whisper_model.is_some();
        let vad_configured = settings.vad_model.is_some();
        let gpu_requested = settings.use_gpu;
        let worker = worker::spawn(
            settings,
            command_receiver,
            commands.clone(),
            event_sender,
            activity.clone(),
            revision.clone(),
            completion.0,
        )?;
        log::info!(
            "语音控制端已创建：revision=1, mode={initial_mode}, whisper_configured={whisper_configured}, vad_configured={vad_configured}, gpu_requested={gpu_requested}"
        );
        Ok((
            Self {
                commands: commands.clone(),
                events,
                activity,
                revision,
            },
            VoiceShutdown {
                commands,
                completion: completion.1,
                worker: Some(worker),
            },
        ))
    }

    /// 返回单消费者事件端；桌宠根视图只应调用一次。
    pub(crate) fn events(&self) -> Receiver<VoiceEvent> {
        self.events.clone()
    }

    /// 原子读取最新活动状态，适合受限帧率的波形动画。
    pub(crate) fn activity(&self) -> VoiceActivitySnapshot {
        self.activity.snapshot()
    }

    /// 返回控制端已经发布的最新配置 generation。
    pub(crate) fn current_revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// 用新配置替换整个语音 generation；旧转写结果会被 worker 丢弃。
    pub(crate) fn configure(&self, settings: VoiceSettings) -> u64 {
        let revision = self
            .revision
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        self.activity.set_phase(VoicePhase::Off);
        let _ = self.commands.try_send(VoiceCommand::Configure {
            revision,
            settings: Arc::new(settings),
        });
        revision
    }

    /// 开始或结束一次由全局语音快捷键控制的录音。
    pub(crate) fn set_push_to_talk(&self, pressed: bool) {
        let revision = self.revision.load(Ordering::Acquire);
        let _ = self
            .commands
            .try_send(VoiceCommand::PushToTalk { revision, pressed });
    }

    /// 窗口释放时先请求停止采集；最终 join 由应用退出边界完成。
    pub(crate) fn request_shutdown(&self) {
        let _ = self.commands.try_send(VoiceCommand::Shutdown);
    }
}

/// 应用事件循环结束后负责有限等待语音线程退出。
pub(crate) struct VoiceShutdown {
    commands: Sender<VoiceCommand>,
    completion: std::sync::mpsc::Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl VoiceShutdown {
    /// 请求停止并在给定上限内等待；超时会分离线程而不是卡住进程收尾。
    pub(crate) fn shutdown(mut self, timeout: Duration) -> bool {
        let _ = self.commands.send_blocking(VoiceCommand::Shutdown);
        match self.completion.recv_timeout(timeout) {
            Ok(()) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                log::warn!(
                    "等待语音工作线程退出超时：timeout_ms={}",
                    timeout.as_millis()
                );
                return false;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if self
                    .worker
                    .take()
                    .is_some_and(|worker| worker.join().is_err())
                {
                    log::error!("语音工作线程在退出时发生 panic");
                } else {
                    log::error!("语音工作线程未报告完成便关闭了完成通道");
                }
                return false;
            }
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("语音工作线程在退出时发生 panic");
            return false;
        }
        log::info!("语音服务已停止");
        true
    }
}

impl Drop for VoiceShutdown {
    fn drop(&mut self) {
        let _ = self.commands.try_send(VoiceCommand::Shutdown);
    }
}
