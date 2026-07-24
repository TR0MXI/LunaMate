//! 加载 CPU 模型 generation，并在后台驱动其按需渲染循环。

use std::{
    error::Error,
    fmt,
    future::Future,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{FutureExt as _, future::select};
use gpui::Context;
use rust_i18n::t;

use super::{ModelLoadState, ModelView, model_display_name};
use crate::{
    capabilities::ModelLoadDiagnostics,
    config::{CONFIG, FrameRate},
    frame_scheduler::{FramePacer, FrameWakeReceiver, frame_wake_channel},
    interaction::{MAX_COMMANDS_PER_FRAME, ModelCommand, RenderedModelFrame, command_channel},
    live2d_image::{
        AnimatedModel, ModelLoadError, ModelPreviewCapabilities, RenderCancellation, RenderError,
    },
};

struct LoadedModelGeneration {
    model: AnimatedModel,
    frame: RenderedModelFrame,
    diagnostics: ModelLoadDiagnostics,
    needs_continuous_frames: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameWaitResult {
    FrameReady,
    FrameRateChanged,
    Closed,
}

async fn wait_for_frame_or_rate_change(
    frame: impl Future<Output = bool>,
    frame_rate_receiver: &FrameWakeReceiver,
) -> FrameWaitResult {
    let frame = frame
        .map(|ready| ready.then_some(FrameWaitResult::FrameReady))
        .boxed_local();
    let changed = frame_rate_receiver
        .wait()
        .map(|ready| ready.then_some(FrameWaitResult::FrameRateChanged))
        .boxed_local();
    // 配置已经发布时优先重建节拍，避免边界上再渲染一帧旧模式。
    select(changed, frame)
        .await
        .factor_first()
        .0
        .unwrap_or(FrameWaitResult::Closed)
}

fn frame_pacer(frame_rate: FrameRate) -> FramePacer {
    FramePacer::new(
        frame_rate.limit(),
        frame_rate.allows_frame_rate_degradation(),
    )
}

#[derive(Debug)]
enum ModelGenerationLoadError {
    Model(ModelLoadError),
    InitialFrame(RenderError),
}

impl ModelGenerationLoadError {
    fn is_cancelled(&self) -> bool {
        match self {
            Self::Model(error) => error.is_cancelled(),
            Self::InitialFrame(error) => error.is_cancelled(),
        }
    }
}

impl fmt::Display for ModelGenerationLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(error) => error.fmt(formatter),
            Self::InitialFrame(error) => write!(
                formatter,
                "{}",
                t!(
                    "model_state.initial_frame_failed",
                    error = error.to_string()
                )
            ),
        }
    }
}

impl Error for ModelGenerationLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model(error) => Some(error),
            Self::InitialFrame(error) => Some(error),
        }
    }
}

impl ModelView {
    /// 用新 generation 替换当前模型，并选择 GPU underlay 或 CPU 后台循环。
    pub(super) fn load_model(&mut self, model_path: Option<PathBuf>, cx: &mut Context<Self>) {
        let already_active = self.selected_model == model_path
            && matches!(
                self.model_state,
                ModelLoadState::Loading(_) | ModelLoadState::Ready { .. }
            );
        if already_active {
            return;
        }

        if let Some(cancellation) = self.model_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(wake) = self.model_wake.take() {
            wake.close();
        }
        if let Some(wake) = self.frame_rate_wake.take() {
            wake.close();
        }
        self.model_task = None;
        self.model_generation = self.model_generation.wrapping_add(1);
        if self.model_generation == 0 {
            self.model_generation = 1;
        }
        self.selected_model = model_path.clone();
        self.frame = None;
        self.frame_rate_meter.reset();
        self.actual_fps = 0.0;
        self.model_commands = None;
        self.reset_look_target();
        self.config.update(cx, |config, cx| {
            config.set_preview_capabilities(ModelPreviewCapabilities::default(), cx);
        });

        let Some(model_path) = model_path else {
            self.model_state = ModelLoadState::NoModel;
            if self.gpu_underlay.is_some() {
                let (_, command_receiver) = command_channel();
                let cancellation = RenderCancellation::default();
                self.model_cancellation = Some(cancellation.clone());
                if let Some(underlay) = &mut self.gpu_underlay {
                    underlay.load(
                        self.model_generation,
                        None,
                        self.gpu_underlay_size,
                        cancellation,
                        command_receiver,
                        self.look_target.clone(),
                    );
                }
            }
            cx.notify();
            return;
        };

        let generation = self.model_generation;
        let model_name = model_display_name(&model_path);
        let [width, height] = self.raster_dimensions;
        let task_look_target = self.look_target.clone();
        let background = cx.background_executor().clone();
        let (model_commands, command_receiver) = command_channel();
        let cancellation = RenderCancellation::default();
        self.model_commands = Some(model_commands);
        self.model_cancellation = Some(cancellation.clone());
        self.model_state = ModelLoadState::Loading(model_name.clone());
        cx.notify();

        if let Some(underlay) = &mut self.gpu_underlay {
            underlay.load(
                generation,
                Some(model_path),
                self.gpu_underlay_size,
                cancellation,
                command_receiver,
                self.look_target.clone(),
            );
            return;
        }

        let (model_wake, wake_receiver) = frame_wake_channel();
        let (frame_rate_wake, frame_rate_receiver) = frame_wake_channel();
        self.model_wake = Some(model_wake);
        self.frame_rate_wake = Some(frame_rate_wake);

        self.model_task = Some(cx.spawn(async move |this, cx| {
            let loaded = background
                .spawn({
                    let model_path = model_path.clone();
                    let cancellation = cancellation.clone();
                    async move {
                        let mut model =
                            AnimatedModel::load(&model_path, width, height, cancellation)
                                .map_err(ModelGenerationLoadError::Model)?;
                        let diagnostics = model.diagnostics().clone();
                        let frame = model
                            .render_frame(Duration::ZERO, [0.0, 0.0])
                            .map_err(ModelGenerationLoadError::InitialFrame)?;
                        let needs_continuous_frames = model.needs_continuous_frames();
                        Ok::<_, ModelGenerationLoadError>(LoadedModelGeneration {
                            model,
                            frame,
                            diagnostics,
                            needs_continuous_frames,
                        })
                    }
                })
                .await;

            let loaded = match loaded {
                Ok(loaded) => loaded,
                Err(error) => {
                    if error.is_cancelled() {
                        return;
                    }
                    let error = error.to_string();
                    let _ = this.update(cx, |this, cx| {
                        if this.model_generation == generation {
                            this.model_state = ModelLoadState::Failed(
                                t!("model_state.load_failed", name = model_name, error = error)
                                    .to_string(),
                            );
                            this.config.update(cx, |config, cx| {
                                config.set_preview_capabilities(
                                    ModelPreviewCapabilities::default(),
                                    cx,
                                );
                            });
                            this.frame = None;
                            this.model_commands = None;
                            this.model_cancellation = None;
                            if let Some(wake) = this.model_wake.take() {
                                wake.close();
                            }
                            if let Some(wake) = this.frame_rate_wake.take() {
                                wake.close();
                            }
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let mut model = loaded.model;
            let frame = loaded.frame;
            let diagnostics = loaded.diagnostics;
            let capabilities = model.preview_capabilities();
            let mut needs_next_frame = loaded.needs_continuous_frames;

            let should_continue = this
                .update(cx, |this, cx| {
                    if this.model_generation != generation {
                        return false;
                    }
                    this.frame = Some(Arc::new(frame));
                    this.model_state = ModelLoadState::ready(diagnostics);
                    this.config.update(cx, |config, cx| {
                        config.set_preview_capabilities(capabilities.clone(), cx);
                    });
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !should_continue {
                return;
            }

            let mut previous_frame = Instant::now();
            let mut pacer = frame_pacer(CONFIG.frame_rate());
            let mut reset_delta = false;
            loop {
                let mut frame_rate = CONFIG.frame_rate();
                pacer.set_target_fps(
                    frame_rate.limit(),
                    frame_rate.allows_frame_rate_degradation(),
                );
                let wait_for_display = if needs_next_frame {
                    if frame_rate.follows_display() {
                        true
                    } else {
                        let next_delay = pacer.delay_until_next_frame(Instant::now());
                        let timer = background.timer(next_delay).map(|()| true);
                        match wait_for_frame_or_rate_change(timer, &frame_rate_receiver).await {
                            FrameWaitResult::FrameReady => {}
                            FrameWaitResult::FrameRateChanged => {
                                pacer = frame_pacer(CONFIG.frame_rate());
                                continue;
                            }
                            FrameWaitResult::Closed => break,
                        }
                        false
                    }
                } else {
                    match wait_for_frame_or_rate_change(wake_receiver.wait(), &frame_rate_receiver)
                        .await
                    {
                        FrameWaitResult::FrameReady => {}
                        FrameWaitResult::FrameRateChanged => {
                            pacer = frame_pacer(CONFIG.frame_rate());
                            continue;
                        }
                        FrameWaitResult::Closed => break,
                    }
                    pacer.reset_after_idle();
                    reset_delta = true;
                    // 已消费的输入必须跨过后续帧率切换继续保留，直到真正完成一帧。
                    needs_next_frame = true;
                    frame_rate = CONFIG.frame_rate();
                    pacer.set_target_fps(
                        frame_rate.limit(),
                        frame_rate.allows_frame_rate_degradation(),
                    );
                    frame_rate.follows_display()
                };

                if wait_for_display {
                    // 每轮只挂一个一次性回调；后台渲染期间错过的显示帧不会排队追赶。
                    let (frame_wake, frame_receiver) = frame_wake_channel();
                    let armed = this
                        .update_in(cx, move |this, window, _| {
                            if this.model_generation != generation || this.gpu_underlay.is_some() {
                                return false;
                            }
                            window.on_next_frame(move |_, _| {
                                frame_wake.wake();
                            });
                            true
                        })
                        .unwrap_or(false);
                    if !armed {
                        break;
                    }
                    match wait_for_frame_or_rate_change(frame_receiver.wait(), &frame_rate_receiver)
                        .await
                    {
                        FrameWaitResult::FrameReady => {}
                        FrameWaitResult::FrameRateChanged => {
                            pacer = frame_pacer(CONFIG.frame_rate());
                            continue;
                        }
                        FrameWaitResult::Closed => break,
                    }
                }
                wake_receiver.drain();
                let frame_started = Instant::now();
                let delta = if reset_delta {
                    Duration::ZERO
                } else {
                    frame_started.saturating_duration_since(previous_frame)
                };
                reset_delta = false;
                previous_frame = frame_started;
                let look = *task_look_target.lock();
                let mut commands: [Option<ModelCommand>; MAX_COMMANDS_PER_FRAME] =
                    std::array::from_fn(|_| None);
                let mut command_count = 0;
                for command in command_receiver.try_iter().take(MAX_COMMANDS_PER_FRAME) {
                    commands[command_count] = Some(command);
                    command_count += 1;
                }
                let command_batch_full = command_count == MAX_COMMANDS_PER_FRAME;
                // 命令和模型一起移入后台任务，GPUI 线程只负责收集有限数量的输入。
                let render = background.spawn(async move {
                    for command in commands.into_iter().flatten() {
                        model.handle_command(command);
                    }
                    let frame = model.render_frame(delta, look);
                    let needs_continuous_frames = model.needs_continuous_frames();
                    (model, frame, needs_continuous_frames)
                });
                let (returned_model, frame, model_needs_continuous_frames) = render.await;
                model = returned_model;

                let should_continue = this
                    .update(cx, |this, cx| {
                        if this.model_generation != generation {
                            return false;
                        }
                        match frame {
                            Ok(frame) => {
                                this.frame = Some(Arc::new(frame));
                                cx.notify();
                                true
                            }
                            Err(error) if error.is_cancelled() => false,
                            Err(error) => {
                                log::error!("{}", t!("log.frame_render_stopped", error = error));
                                this.frame = None;
                                this.config.update(cx, |config, cx| {
                                    config.set_preview_capabilities(
                                        ModelPreviewCapabilities::default(),
                                        cx,
                                    );
                                });
                                this.model_commands = None;
                                if let Some(cancellation) = this.model_cancellation.take() {
                                    cancellation.cancel();
                                }
                                if let Some(wake) = this.model_wake.take() {
                                    wake.close();
                                }
                                if let Some(wake) = this.frame_rate_wake.take() {
                                    wake.close();
                                }
                                this.model_state = ModelLoadState::Failed(
                                    t!(
                                        "model_state.render_failed",
                                        name = model_name,
                                        error = error.to_string()
                                    )
                                    .to_string(),
                                );
                                cx.notify();
                                false
                            }
                        }
                    })
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }

                let frame_completed = Instant::now();
                let input_pending = wake_receiver.drain();
                needs_next_frame =
                    model_needs_continuous_frames || command_batch_full || input_pending;
                if needs_next_frame {
                    pacer.complete_frame(frame_started, frame_completed);
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};

    use futures::executor::block_on;

    use super::*;

    #[test]
    fn frame_rate_change_interrupts_a_pending_frame_wait() {
        let (wake, receiver) = frame_wake_channel();
        wake.wake();

        assert_eq!(
            block_on(wait_for_frame_or_rate_change(pending(), &receiver)),
            FrameWaitResult::FrameRateChanged
        );
        let (wake, receiver) = frame_wake_channel();
        wake.wake();
        assert_eq!(
            block_on(wait_for_frame_or_rate_change(ready(true), &receiver)),
            FrameWaitResult::FrameRateChanged
        );
    }

    #[test]
    fn completed_and_closed_frame_waits_are_distinguished() {
        let (_wake, receiver) = frame_wake_channel();
        assert_eq!(
            block_on(wait_for_frame_or_rate_change(ready(true), &receiver)),
            FrameWaitResult::FrameReady
        );
        assert_eq!(
            block_on(wait_for_frame_or_rate_change(ready(false), &receiver)),
            FrameWaitResult::Closed
        );
    }
}
