//! 在实时回调中完成最小转换，并在语音 worker 上重采样为 16 kHz 单声道 PCM。

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_channel::Sender;
use cpal::{
    FromSample, Sample as _, SampleFormat, SizedSample, Stream,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer as _, Producer as _, Split as _},
};
use rubato::{FftFixedInOut, Resampler as _};

use super::VoiceCommand;

const TARGET_SAMPLE_RATE: usize = 16_000;
const RING_SECONDS: usize = 2;

pub(super) struct Capture {
    stream: Option<Stream>,
    consumer: HeapCons<f32>,
    normalizer: AudioNormalizer,
    scratch: Vec<f32>,
    wake_pending: Arc<AtomicBool>,
    overflowed: Arc<AtomicBool>,
}

pub(super) enum DrainOutcome {
    Continuous,
    Discontinuous,
}

#[derive(Clone)]
struct CaptureRoute {
    commands: Sender<VoiceCommand>,
    revision: u64,
    capture_id: u64,
    wake_pending: Arc<AtomicBool>,
    overflowed: Arc<AtomicBool>,
}

impl Capture {
    pub(super) fn start(
        revision: u64,
        capture_id: u64,
        commands: Sender<VoiceCommand>,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "没有可用的默认麦克风".to_owned())?;
        let supported = device
            .default_input_config()
            .map_err(|error| format!("无法读取默认麦克风配置：{error}"))?;
        let channels = usize::from(supported.channels());
        if channels == 0 {
            return Err("默认麦克风没有可用声道".to_owned());
        }
        let sample_rate = usize::try_from(supported.sample_rate())
            .map_err(|_| "麦克风采样率超出平台表示范围".to_owned())?;
        let ring_capacity = sample_rate.saturating_mul(RING_SECONDS).max(1);
        let (producer, consumer) = HeapRb::<f32>::new(ring_capacity).split();
        let wake_pending = Arc::new(AtomicBool::new(false));
        let overflowed = Arc::new(AtomicBool::new(false));
        let stream_config = supported.into();
        let route = CaptureRoute {
            commands,
            revision,
            capture_id,
            wake_pending: wake_pending.clone(),
            overflowed: overflowed.clone(),
        };
        let stream = match supported.sample_format() {
            SampleFormat::I8 => {
                build_stream::<i8>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::I16 => {
                build_stream::<i16>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::I24 => {
                build_stream::<cpal::I24>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::I32 => {
                build_stream::<i32>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::I64 => {
                build_stream::<i64>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::U8 => {
                build_stream::<u8>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::U16 => {
                build_stream::<u16>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::U24 => {
                build_stream::<cpal::U24>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::U32 => {
                build_stream::<u32>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::U64 => {
                build_stream::<u64>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::F32 => {
                build_stream::<f32>(&device, stream_config, channels, producer, route.clone())
            }
            SampleFormat::F64 => {
                build_stream::<f64>(&device, stream_config, channels, producer, route)
            }
            SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => {
                return Err("默认麦克风只提供不受支持的 DSD 采样格式".to_owned());
            }
            _ => return Err("默认麦克风采样格式不受支持".to_owned()),
        }?;
        stream
            .play()
            .map_err(|error| format!("无法启动麦克风：{error}"))?;
        Ok(Self {
            stream: Some(stream),
            consumer,
            normalizer: AudioNormalizer::new(sample_rate)?,
            scratch: vec![0.0; 4_096],
            wake_pending,
            overflowed,
        })
    }

    pub(super) fn drain_into(&mut self, output: &mut Vec<f32>) -> Result<DrainOutcome, String> {
        // 先开放下一次唤醒；随后到达的回调要么被本轮 drain 读取，要么再次排队。
        self.wake_pending.store(false, Ordering::Release);
        output.clear();
        loop {
            let count = self.consumer.pop_slice(&mut self.scratch);
            if count == 0 {
                break;
            }
            self.normalizer
                .push(&self.scratch[..count], output)
                .map_err(|error| format!("麦克风重采样失败：{error}"))?;
        }
        Ok(if self.overflowed.swap(false, Ordering::AcqRel) {
            DrainOutcome::Discontinuous
        } else {
            DrainOutcome::Continuous
        })
    }

    /// 停止回调、排空环形缓冲，并处理重采样器中不足一个完整块的尾音频。
    pub(super) fn finish_into(mut self, output: &mut Vec<f32>) -> Result<DrainOutcome, String> {
        self.stream = None;
        let outcome = self.drain_into(output)?;
        self.normalizer
            .finish(output)
            .map_err(|error| format!("麦克风重采样收尾失败：{error}"))?;
        Ok(outcome)
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut producer: HeapProd<f32>,
    route: CaptureRoute,
) -> Result<Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let CaptureRoute {
        commands,
        revision,
        capture_id,
        wake_pending,
        overflowed,
    } = route;
    let wake_commands = commands.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                for frame in data.chunks_exact(channels) {
                    let sum = frame.iter().copied().map(f32::from_sample).sum::<f32>();
                    let mono = sum / channels as f32;
                    let mono = if mono.is_finite() {
                        mono.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                    if producer.try_push(mono).is_err() {
                        overflowed.store(true, Ordering::Release);
                        break;
                    }
                }
                if !wake_pending.swap(true, Ordering::AcqRel) {
                    let _ = wake_commands.try_send(VoiceCommand::AudioReady {
                        revision,
                        capture_id,
                    });
                }
            },
            move |error| {
                let _ = commands.try_send(VoiceCommand::CaptureFailed {
                    revision,
                    capture_id,
                    message: error.to_string(),
                });
            },
            None,
        )
        .map_err(|error| format!("无法打开麦克风：{error}"))
}

enum AudioNormalizer {
    Passthrough,
    Resampling {
        resampler: Box<FftFixedInOut<f32>>,
        input_rate: usize,
        pending: VecDeque<f32>,
        input: Vec<Vec<f32>>,
        output: Vec<Vec<f32>>,
    },
}

impl AudioNormalizer {
    fn new(input_rate: usize) -> Result<Self, String> {
        if input_rate == TARGET_SAMPLE_RATE {
            return Ok(Self::Passthrough);
        }
        let requested_chunk = (input_rate / 50).max(1);
        let resampler =
            FftFixedInOut::<f32>::new(input_rate, TARGET_SAMPLE_RATE, requested_chunk, 1)
                .map_err(|error| error.to_string())?;
        let input = resampler.input_buffer_allocate(true);
        let output = resampler.output_buffer_allocate(true);
        Ok(Self::Resampling {
            resampler: Box::new(resampler),
            input_rate,
            pending: VecDeque::with_capacity(requested_chunk.saturating_mul(2)),
            input,
            output,
        })
    }

    fn push(&mut self, samples: &[f32], normalized: &mut Vec<f32>) -> Result<(), String> {
        match self {
            Self::Passthrough => normalized.extend_from_slice(samples),
            Self::Resampling {
                resampler,
                input_rate: _,
                pending,
                input,
                output,
            } => {
                pending.extend(samples.iter().copied());
                let needed = resampler.input_frames_next();
                while pending.len() >= needed {
                    for sample in &mut input[0][..needed] {
                        let Some(next) = pending.pop_front() else {
                            return Err("重采样输入缓冲意外耗尽".to_owned());
                        };
                        *sample = next;
                    }
                    let (_, produced) = resampler
                        .process_into_buffer(input, output, None)
                        .map_err(|error| error.to_string())?;
                    normalized.extend_from_slice(&output[0][..produced]);
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self, normalized: &mut Vec<f32>) -> Result<(), String> {
        let Self::Resampling {
            resampler,
            input_rate,
            pending,
            output,
            ..
        } = self
        else {
            return Ok(());
        };
        if pending.is_empty() {
            return Ok(());
        }
        let tail = pending.drain(..).collect::<Vec<_>>();
        let (_, produced) = resampler
            .process_partial_into_buffer(Some(std::slice::from_ref(&tail)), output, None)
            .map_err(|error| error.to_string())?;
        let expected = tail
            .len()
            .saturating_mul(TARGET_SAMPLE_RATE)
            .saturating_add(input_rate.saturating_sub(1))
            / *input_rate;
        normalized.extend_from_slice(&output[0][..produced.min(expected)]);
        Ok(())
    }
}
