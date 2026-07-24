//! 加载 CPU 模型 generation，并在后台驱动其按需渲染循环。

use std::{
    error::Error,
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::Context;
use rust_i18n::t;

use super::{ModelLoadState, ModelView, model_display_name};
use crate::{
    capabilities::ModelLoadDiagnostics,
    config::CONFIG,
    frame_scheduler::{FramePacer, frame_wake_channel},
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
        self.model_wake = Some(model_wake);

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
            let mut pacer = FramePacer::new(CONFIG.frame_rate().limit());
            let mut reset_delta = false;
            loop {
                pacer.set_target_fps(CONFIG.frame_rate().limit());
                if needs_next_frame {
                    let next_delay = pacer.delay_until_next_frame(Instant::now());
                    background.timer(next_delay).await;
                } else {
                    if !wake_receiver.wait().await {
                        break;
                    }
                    pacer.reset_after_idle();
                    reset_delta = true;
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
