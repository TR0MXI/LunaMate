//! 加载 CPU 模型 generation，并在后台驱动其按需渲染循环。

use std::{
    error::Error,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{FutureExt as _, future::select};
use gpui::Context;
use rust_i18n::t;

use super::{DesktopPetView, ModelLoadState, model_display_name};
use crate::{
    config::{CONFIG, FrameRate},
    model::{
        AnimatedModel, FramePacer, FrameWakeReceiver, MAX_COMMANDS_PER_FRAME, ModelCommand,
        ModelLoadDiagnostics, ModelLoadError, ModelPreviewCapabilities, RenderCancellation,
        RenderError, RenderedModelFrame, command_channel, frame_wake_channel,
    },
};

struct LoadedModelGeneration {
    model: AnimatedModel,
    frame: RenderedModelFrame,
    diagnostics: ModelLoadDiagnostics,
    needs_continuous_frames: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum FrameWaitResult {
    FrameReady,
    FrameRateChanged,
    Closed,
}

pub(in crate::ui) async fn wait_for_frame_or_rate_change(
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

impl DesktopPetView {
    /// 加载模型；已经就绪或正在加载的相同路径会复用当前 generation。
    pub(super) fn load_model(&mut self, model_path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.load_model_inner(model_path, false, cx);
    }

    /// 强制用新 generation 替换当前模型，供设置变更与资源重扫使用。
    pub(super) fn reload_model(&mut self, model_path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.load_model_inner(model_path, true, cx);
    }

    fn load_model_inner(
        &mut self,
        model_path: Option<PathBuf>,
        force_reload: bool,
        cx: &mut Context<Self>,
    ) {
        let generation_active = matches!(
            self.model_state,
            ModelLoadState::Loading(_) | ModelLoadState::Ready { .. }
        );
        if model_generation_can_be_reused(
            self.selected_model.as_deref(),
            model_path.as_deref(),
            generation_active,
            force_reload,
        ) {
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
        log::info!(
            "Live2D 模型 generation 已创建：generation={}, renderer={}, has_model={}, width={}, height={}",
            self.model_generation,
            if self.gpu_underlay.is_some() {
                "gpu"
            } else {
                "cpu"
            },
            model_path.is_some(),
            self.raster_dimensions[0],
            self.raster_dimensions[1]
        );
        self.frame = None;
        self.sync_cursor_tracking_task(cx);
        self.frame_rate_meter.reset();
        self.actual_fps = 0.0;
        self.model_commands = None;
        self.reset_look_target();
        self.config.update(cx, |config, cx| {
            config.set_preview_capabilities(ModelPreviewCapabilities::default(), cx);
        });
        self.clear_agent_outfits(cx);

        let Some(model_path) = model_path else {
            self.model_state = ModelLoadState::NoModel;
            if self.cpu_fallback_pending {
                log::info!(
                    "Live2D 已完成 GPU 到 CPU 回退：generation={}, renderer=cpu, has_model=false",
                    self.model_generation
                );
                self.cpu_fallback_pending = false;
            }
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
        let mut observed_visibility_revision = self.visibility_revision;
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
                            if this.cpu_fallback_pending {
                                log::error!(
                                    "Live2D GPU 回退后的 CPU 模型加载失败：generation={generation}, stage=model_load"
                                );
                            } else {
                                log::warn!(
                                    "Live2D 模型加载失败：generation={generation}, renderer=cpu, stage=model_load"
                                );
                            }
                            this.cpu_fallback_pending = false;
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
                            this.clear_agent_outfits(cx);
                            this.frame = None;
                            this.sync_cursor_tracking_task(cx);
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
                    if this.desktop_pet_visible {
                        this.frame = Some(Arc::new(frame));
                        this.sync_cursor_tracking_task(cx);
                    }
                    let diagnostic_count = diagnostics.entries().len();
                    let movable_expression_count = capabilities
                        .expressions()
                        .iter()
                        .filter(|expression| expression.movable_to_outfit())
                        .count();
                    let motion_count = capabilities
                        .motions()
                        .len()
                        .saturating_add(capabilities.idle_motions().len());
                    let expression_count = capabilities.expressions().len();
                    this.model_state = ModelLoadState::ready(diagnostics);
                    log::info!(
                        "Live2D 模型已就绪：generation={generation}, renderer=cpu, movable_expressions={movable_expression_count}, motions={motion_count}, expressions={expression_count}, diagnostics={diagnostic_count}"
                    );
                    if diagnostic_count > 0 {
                        log::warn!(
                            "Live2D 模型存在非致命能力问题：generation={generation}, diagnostics={diagnostic_count}"
                        );
                    }
                    if this.cpu_fallback_pending {
                        log::info!(
                            "Live2D 已完成 GPU 到 CPU 回退：generation={generation}, renderer=cpu"
                        );
                    }
                    this.cpu_fallback_pending = false;
                    this.config.update(cx, |config, cx| {
                        config.set_preview_capabilities(capabilities.clone(), cx);
                    });
                    this.sync_agent_outfits(cx);
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
                let activity = this
                    .update(cx, |this, _| {
                        (this.model_generation == generation && this.gpu_underlay.is_none())
                            .then_some((this.desktop_pet_visible, this.visibility_revision))
                    })
                    .ok()
                    .flatten();
                let Some((visible, visibility_revision)) = activity else {
                    break;
                };
                if observed_visibility_revision != visibility_revision {
                    observed_visibility_revision = visibility_revision;
                    pacer.reset_after_idle();
                    reset_delta = true;
                    needs_next_frame = true;
                }
                if !visible {
                    pacer.reset_after_idle();
                    reset_delta = true;
                    needs_next_frame = true;
                    if !frame_rate_receiver.wait().await {
                        break;
                    }
                    continue;
                }

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
                                if this.desktop_pet_visible {
                                    let first_visible_frame = this.frame.is_none();
                                    this.frame = Some(Arc::new(frame));
                                    if first_visible_frame {
                                        this.sync_cursor_tracking_task(cx);
                                    }
                                    cx.notify();
                                }
                                true
                            }
                            Err(error) if error.is_cancelled() => false,
                            Err(error) => {
                                log::error!("{}", t!("log.frame_render_stopped", error = error));
                                this.frame = None;
                                this.sync_cursor_tracking_task(cx);
                                this.config.update(cx, |config, cx| {
                                    config.set_preview_capabilities(
                                        ModelPreviewCapabilities::default(),
                                        cx,
                                    );
                                });
                                this.clear_agent_outfits(cx);
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

/// 返回当前 generation 是否可以满足本次模型请求。
pub(in crate::ui) fn model_generation_can_be_reused(
    selected: Option<&Path>,
    requested: Option<&Path>,
    generation_active: bool,
    force_reload: bool,
) -> bool {
    !force_reload && generation_active && selected == requested
}
