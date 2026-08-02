//! 在独立线程准备 TTS PCM，并由 CPAL 回调只消费已经重采样的有界缓冲。

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use cpal::{
    FromSample, SampleFormat, SizedSample, Stream,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};
use rubato::{FftFixedInOut, Resampler as _};

const OUTPUT_ERROR_REPORT_INTERVAL: Duration = Duration::from_secs(5);

struct OutputErrorReporter {
    occurrences: Arc<AtomicU64>,
    last_report: Instant,
}

impl OutputErrorReporter {
    fn new(occurrences: Arc<AtomicU64>) -> Self {
        Self {
            occurrences,
            last_report: Instant::now(),
        }
    }

    fn report(&mut self, force: bool) {
        if !force && self.last_report.elapsed() < OUTPUT_ERROR_REPORT_INTERVAL {
            return;
        }
        let occurrences = self.occurrences.swap(0, Ordering::AcqRel);
        self.last_report = Instant::now();
        if occurrences > 0 {
            log::warn!("event=audio_output_stream_error occurrences={occurrences} coalesced=true");
        }
    }
}

enum PlaybackCommand {
    Play {
        generation: u64,
        samples: Vec<i16>,
        sample_rate: u32,
    },
    Stop {
        generation: u64,
    },
    Shutdown,
}

/// 可克隆的 TTS 播放控制端；后发播报会替换当前缓冲。
#[derive(Clone)]
pub(crate) struct SpeechPlayback {
    commands: std::sync::mpsc::Sender<PlaybackCommand>,
    generation: Arc<AtomicU64>,
}

impl SpeechPlayback {
    pub(crate) fn start() -> Result<(Self, SpeechPlaybackShutdown), String> {
        let (commands, receiver) = std::sync::mpsc::channel();
        let (completed, completion) = std::sync::mpsc::channel();
        let generation = Arc::new(AtomicU64::new(1));
        let thread_generation = generation.clone();
        let thread = std::thread::Builder::new()
            .name("lunamate-speech-playback".to_owned())
            .spawn(move || {
                run(receiver, thread_generation);
                let _ = completed.send(());
            })
            .map_err(|error| format!("无法启动语音播放线程：{error}"))?;
        Ok((
            Self {
                commands: commands.clone(),
                generation,
            },
            SpeechPlaybackShutdown {
                commands,
                completed: completion,
                thread: Some(thread),
            },
        ))
    }

    pub(crate) fn play(&self, samples: Vec<i16>, sample_rate: u32) {
        let generation = next_generation(&self.generation);
        let _ = self.commands.send(PlaybackCommand::Play {
            generation,
            samples,
            sample_rate,
        });
    }

    pub(crate) fn stop(&self) {
        let generation = next_generation(&self.generation);
        let _ = self.commands.send(PlaybackCommand::Stop { generation });
    }
}

pub(crate) struct SpeechPlaybackShutdown {
    commands: std::sync::mpsc::Sender<PlaybackCommand>,
    completed: std::sync::mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl SpeechPlaybackShutdown {
    pub(crate) fn shutdown(mut self, timeout: Duration) -> bool {
        let _ = self.commands.send(PlaybackCommand::Shutdown);
        if self.completed.recv_timeout(timeout).is_err() {
            self.thread.take();
            return false;
        }
        self.thread
            .take()
            .is_none_or(|thread| thread.join().is_ok())
    }
}

impl Drop for SpeechPlaybackShutdown {
    fn drop(&mut self) {
        let _ = self.commands.send(PlaybackCommand::Shutdown);
    }
}

fn run(commands: std::sync::mpsc::Receiver<PlaybackCommand>, generation: Arc<AtomicU64>) {
    let mut _stream: Option<Stream> = None;
    let mut playback_complete: Option<Arc<AtomicBool>> = None;
    let mut output_errors: Option<OutputErrorReporter> = None;
    loop {
        if let Some(reporter) = &mut output_errors {
            reporter.report(false);
        }
        if playback_complete
            .as_ref()
            .is_some_and(|completed| completed.load(Ordering::Acquire))
        {
            _stream = None;
            playback_complete = None;
            if let Some(mut reporter) = output_errors.take() {
                reporter.report(true);
            }
        }
        let command = match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => command,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            PlaybackCommand::Play {
                generation: command_generation,
                samples,
                sample_rate,
            } if generation.load(Ordering::Acquire) == command_generation => {
                match build_output_stream(
                    samples,
                    sample_rate,
                    command_generation,
                    generation.clone(),
                ) {
                    Ok((next, completed, mut reporter)) => {
                        if generation.load(Ordering::Acquire) == command_generation {
                            if let Some(mut previous) = output_errors.take() {
                                previous.report(true);
                            }
                            _stream = Some(next);
                            playback_complete = Some(completed);
                            reporter.report(false);
                            output_errors = Some(reporter);
                        }
                    }
                    Err(_) => {
                        _stream = None;
                        playback_complete = None;
                        if let Some(mut reporter) = output_errors.take() {
                            reporter.report(true);
                        }
                        log::warn!("event=speech_playback_failed stage=prepare_or_start");
                    }
                }
            }
            PlaybackCommand::Play { .. } => {}
            PlaybackCommand::Stop {
                generation: command_generation,
            } if generation.load(Ordering::Acquire) == command_generation => {
                _stream = None;
                playback_complete = None;
                if let Some(mut reporter) = output_errors.take() {
                    reporter.report(true);
                }
            }
            PlaybackCommand::Stop { .. } => {}
            PlaybackCommand::Shutdown => break,
        }
    }
    if let Some(mut reporter) = output_errors {
        reporter.report(true);
    }
}

fn build_output_stream(
    samples: Vec<i16>,
    input_rate: u32,
    playback_generation: u64,
    current_generation: Arc<AtomicU64>,
) -> Result<(Stream, Arc<AtomicBool>, OutputErrorReporter), String> {
    if samples.is_empty() || input_rate == 0 {
        return Err("TTS PCM 为空或采样率无效".to_owned());
    }
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "没有可用的默认音频输出设备".to_owned())?;
    let supported = device
        .default_output_config()
        .map_err(|error| format!("无法读取默认音频输出配置：{error}"))?;
    let output_rate = usize::try_from(supported.sample_rate())
        .map_err(|_| "输出设备采样率超出平台表示范围".to_owned())?;
    let channels = usize::from(supported.channels());
    if channels == 0 {
        return Err("默认音频输出设备没有可用声道".to_owned());
    }
    let input = samples
        .into_iter()
        .map(|sample| f32::from(sample) / f32::from(i16::MAX))
        .collect::<Vec<_>>();
    let normalized = resample(input, input_rate as usize, output_rate)?;
    let queue = VecDeque::from(normalized);
    let completed = Arc::new(AtomicBool::new(false));
    let config = supported.config();
    let (stream, output_errors) = match supported.sample_format() {
        SampleFormat::I8 => build_typed::<i8>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::I16 => build_typed::<i16>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::I24 => build_typed::<cpal::I24>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::I32 => build_typed::<i32>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::I64 => build_typed::<i64>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::U8 => build_typed::<u8>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::U16 => build_typed::<u16>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::U24 => build_typed::<cpal::U24>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::U32 => build_typed::<u32>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::U64 => build_typed::<u64>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::F32 => build_typed::<f32>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation.clone(),
        ),
        SampleFormat::F64 => build_typed::<f64>(
            &device,
            &config,
            channels,
            queue,
            completed.clone(),
            playback_generation,
            current_generation,
        ),
        SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => {
            return Err("默认输出设备只提供不受支持的 DSD 格式".to_owned());
        }
        _ => return Err("默认输出设备采样格式不受支持".to_owned()),
    }?;
    stream
        .play()
        .map_err(|error| format!("无法启动音频输出：{error}"))?;
    Ok((stream, completed, OutputErrorReporter::new(output_errors)))
}

fn build_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut queue: VecDeque<f32>,
    completed: Arc<AtomicBool>,
    playback_generation: u64,
    current_generation: Arc<AtomicU64>,
) -> Result<(Stream, Arc<AtomicU64>), String>
where
    T: SizedSample + FromSample<f32>,
{
    let output_errors = Arc::new(AtomicU64::new(0));
    let callback_errors = output_errors.clone();
    let stream = device
        .build_output_stream(
            *config,
            move |output: &mut [T], _| {
                if current_generation.load(Ordering::Acquire) != playback_generation {
                    for sample in output {
                        *sample = T::from_sample(0.0);
                    }
                    completed.store(true, Ordering::Release);
                    return;
                }
                for frame in output.chunks_mut(channels) {
                    let sample = queue.pop_front().unwrap_or(0.0);
                    for channel in frame {
                        *channel = T::from_sample(sample);
                    }
                }
                if queue.is_empty() {
                    completed.store(true, Ordering::Release);
                }
            },
            move |_| {
                callback_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|error| format!("无法打开音频输出：{error}"))?;
    Ok((stream, output_errors))
}

fn next_generation(generation: &AtomicU64) -> u64 {
    generation
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
        .max(1)
}

fn resample(samples: Vec<f32>, input_rate: usize, output_rate: usize) -> Result<Vec<f32>, String> {
    if input_rate == output_rate {
        return Ok(samples);
    }
    let chunk = (input_rate / 50).max(1);
    let mut resampler = FftFixedInOut::<f32>::new(input_rate, output_rate, chunk, 1)
        .map_err(|error| error.to_string())?;
    let mut output = Vec::with_capacity(
        samples
            .len()
            .saturating_mul(output_rate)
            .saturating_div(input_rate)
            .saturating_add(chunk),
    );
    let mut offset = 0;
    let mut buffer = resampler.output_buffer_allocate(true);
    while samples.len().saturating_sub(offset) >= resampler.input_frames_next() {
        let needed = resampler.input_frames_next();
        let (_, produced) = resampler
            .process_into_buffer(&[&samples[offset..offset + needed]], &mut buffer, None)
            .map_err(|error| error.to_string())?;
        output.extend_from_slice(&buffer[0][..produced]);
        offset += needed;
    }
    if offset < samples.len() {
        let tail = &samples[offset..];
        let (_, produced) = resampler
            .process_partial_into_buffer(Some(&[tail]), &mut buffer, None)
            .map_err(|error| error.to_string())?;
        let expected = tail
            .len()
            .saturating_mul(output_rate)
            .saturating_add(input_rate.saturating_sub(1))
            / input_rate;
        output.extend_from_slice(&buffer[0][..produced.min(expected)]);
    }
    Ok(output)
}

#[cfg(test)]
pub(super) fn resample_for_test(
    samples: Vec<f32>,
    input_rate: usize,
    output_rate: usize,
) -> Result<Vec<f32>, String> {
    resample(samples, input_rate, output_rate)
}
