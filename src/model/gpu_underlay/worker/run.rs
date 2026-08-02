//! 加载模型并执行 GPU worker 帧循环。

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_channel::{Sender as AsyncSender, TrySendError};
use parking_lot::Mutex;

use crate::{config::CONFIG, platform::SurfaceFactory};

use super::super::super::{
    frame_scheduler::FramePacer,
    interaction::MAX_COMMANDS_PER_FRAME,
    live2d::{AnimatedModel, GpuModelRenderer},
};
use super::super::{
    GpuUnavailableKind, GpuUnderlayEvent, LatestFrameSlot, PresentedFrame, WorkerMailbox,
};
use super::wait::wait_for_replacement;
use super::{
    CLEAR_RETRY_INITIAL_DELAY, ClearSurfaceResult, GpuFrameError, GpuSurface, ModelFailureStage,
    PauseWaitResult, RetryWaitResult, SURFACE_RETRY_INITIAL_DELAY, SurfaceRetryBackoff,
    model_failure_event, wait_for_surface_retry, wait_while_paused,
};

/// 从配置同步限帧档位与 swapchain 呈现模式。
fn sync_frame_rate(surface: &mut GpuSurface, pacer: &mut FramePacer) -> Result<(), String> {
    let frame_rate = CONFIG.frame_rate();
    pacer.set_target_fps(
        frame_rate.limit(),
        frame_rate.allows_frame_rate_degradation(),
    );
    surface.set_present_mode_for_frame_rate(frame_rate)
}

pub(in crate::model::gpu_underlay) fn run(
    factory: SurfaceFactory,
    mailbox: Arc<WorkerMailbox>,
    events: AsyncSender<GpuUnderlayEvent>,
    latest_frame: Arc<Mutex<LatestFrameSlot>>,
) {
    let Some(mut request) = wait_for_replacement(&mailbox) else {
        return;
    };
    let initialization_started = Instant::now();
    let mut surface = match GpuSurface::new(factory, request.size, mailbox.clone()) {
        Ok(Some(surface)) => surface,
        Ok(None) => return,
        Err(_) => {
            let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                kind: GpuUnavailableKind::Initialization,
            });
            return;
        }
    };
    log::info!(
        "event=gpu_surface_ready backend={:?} present_mode={:?} alpha_mode={:?} width={} height={} elapsed_ms={}",
        surface._adapter.get_info().backend,
        surface.config.present_mode,
        surface.alpha_mode,
        surface.config.width,
        surface.config.height,
        initialization_started.elapsed().as_millis()
    );

    'worker: loop {
        match wait_while_paused(&mailbox) {
            PauseWaitResult::Running => {}
            PauseWaitResult::Replaced(replacement) => {
                request = replacement;
                continue;
            }
            PauseWaitResult::Shutdown => return,
        }
        if surface.resize(request.size).is_err() {
            let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                kind: GpuUnavailableKind::Resize,
            });
            return;
        }
        match clear_surface_until_ready(&mut surface, &mailbox) {
            Ok(ClearSurfaceResult::Cleared) => {}
            Ok(ClearSurfaceResult::Replaced(replacement)) => {
                request = replacement;
                continue;
            }
            Ok(ClearSurfaceResult::Paused) => continue,
            Ok(ClearSurfaceResult::Shutdown) => return,
            Err(_) => {
                let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                    kind: GpuUnavailableKind::SurfaceClear,
                });
                return;
            }
        }
        let Some(path) = request.path.clone() else {
            let Some(replacement) = wait_for_replacement(&mailbox) else {
                return;
            };
            request = replacement;
            continue;
        };

        let mut model = match AnimatedModel::load_for_gpu(
            &path,
            request.size.physical[0],
            request.size.physical[1],
            request.cancellation.clone(),
        ) {
            Ok(model) => model,
            Err(error) if error.is_cancelled() => {
                let Some(replacement) = wait_for_replacement(&mailbox) else {
                    return;
                };
                request = replacement;
                continue;
            }
            Err(error) => {
                let _ = events.send_blocking(model_failure_event(
                    ModelFailureStage::Load,
                    request.generation,
                    error.to_string(),
                ));
                let Some(replacement) = wait_for_replacement(&mailbox) else {
                    return;
                };
                request = replacement;
                continue;
            }
        };
        let diagnostics = model.diagnostics().clone();
        let capabilities = model.preview_capabilities();
        match wait_while_paused(&mailbox) {
            PauseWaitResult::Running => {}
            PauseWaitResult::Replaced(replacement) => {
                request = replacement;
                continue;
            }
            PauseWaitResult::Shutdown => return,
        }
        let mut renderer = match GpuModelRenderer::new(
            &surface.device,
            &surface.queue,
            &model,
            surface.config.format,
            surface.alpha_mode,
        ) {
            Ok(renderer) => renderer,
            Err(error) => {
                let _ = events.send_blocking(model_failure_event(
                    ModelFailureStage::Gpu,
                    request.generation,
                    error.to_string(),
                ));
                let Some(replacement) = wait_for_replacement(&mailbox) else {
                    return;
                };
                request = replacement;
                continue;
            }
        };
        let mut first_frame_retry = SurfaceRetryBackoff::new(SURFACE_RETRY_INITIAL_DELAY);
        let first_frame = loop {
            match wait_while_paused(&mailbox) {
                PauseWaitResult::Running => {}
                PauseWaitResult::Replaced(replacement) => {
                    request = replacement;
                    continue 'worker;
                }
                PauseWaitResult::Shutdown => return,
            }
            match surface.render_model(&mut model, &mut renderer, Duration::ZERO, [0.0, 0.0]) {
                Ok(Some(frame)) => {
                    first_frame_retry.reset();
                    if mailbox.is_paused() {
                        continue;
                    }
                    break frame;
                }
                Ok(None) => {
                    match wait_for_surface_retry(&mailbox, first_frame_retry.next_delay()) {
                        RetryWaitResult::Ready | RetryWaitResult::Paused => continue,
                        RetryWaitResult::Replaced(replacement) => {
                            request = replacement;
                            continue 'worker;
                        }
                        RetryWaitResult::Shutdown => return,
                    }
                }
                Err(GpuFrameError::Cancelled) => {
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        return;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Model(error)) => {
                    let _ = events.send_blocking(model_failure_event(
                        ModelFailureStage::Gpu,
                        request.generation,
                        error,
                    ));
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        return;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Surface(_)) => {
                    let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                        kind: GpuUnavailableKind::Surface,
                    });
                    return;
                }
            }
        };
        let first_presented_at = Instant::now();
        let mut presented_frames = 1_u64;
        if events
            .send_blocking(GpuUnderlayEvent::ModelLoaded {
                generation: request.generation,
                frame: first_frame,
                presented_at: first_presented_at,
                presented_frames,
                diagnostics,
                capabilities,
            })
            .is_err()
        {
            return;
        }

        let mut previous_frame = Instant::now();
        let initial_frame_rate = CONFIG.frame_rate();
        let mut pacer = FramePacer::new(
            initial_frame_rate.limit(),
            initial_frame_rate.allows_frame_rate_degradation(),
        );
        let mut needs_next_frame = model.needs_continuous_frames();
        let mut render_requested = false;
        let mut reset_delta = false;
        let mut surface_retry = SurfaceRetryBackoff::new(SURFACE_RETRY_INITIAL_DELAY);
        loop {
            if mailbox.is_paused() {
                pacer.reset_after_idle();
                reset_delta = true;
                render_requested = true;
                match wait_while_paused(&mailbox) {
                    PauseWaitResult::Running => continue,
                    PauseWaitResult::Replaced(replacement) => {
                        request = replacement;
                        continue 'worker;
                    }
                    PauseWaitResult::Shutdown => break 'worker,
                }
            }
            if sync_frame_rate(&mut surface, &mut pacer).is_err() {
                let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                    kind: GpuUnavailableKind::FrameRateSync,
                });
                break 'worker;
            }
            let should_render = needs_next_frame || render_requested;
            let timeout = should_render.then(|| pacer.delay_until_next_frame(Instant::now()));
            let update = mailbox.wait(timeout);
            if update.shutdown {
                break 'worker;
            }
            if let Some(replacement) = update.replacement {
                if update.woken {
                    mailbox.wake();
                }
                request = replacement;
                continue 'worker;
            }
            if update.pause_changed {
                pacer.reset_after_idle();
                reset_delta = true;
                render_requested = true;
                if update.paused {
                    continue;
                }
            }
            if sync_frame_rate(&mut surface, &mut pacer).is_err() {
                let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                    kind: GpuUnavailableKind::FrameRateSync,
                });
                break 'worker;
            }
            render_requested |= update.woken;
            if !needs_next_frame && !render_requested {
                continue;
            }
            if pacer.delay_until_next_frame(Instant::now()) > Duration::ZERO {
                continue;
            }
            if mailbox.is_paused() {
                continue;
            }
            if !needs_next_frame {
                pacer.reset_after_idle();
            }
            let frame_started = Instant::now();
            let delta = if reset_delta || !needs_next_frame {
                Duration::ZERO
            } else {
                frame_started.saturating_duration_since(previous_frame)
            };
            reset_delta = false;
            previous_frame = frame_started;
            render_requested = false;
            let mut command_count = 0;
            for command in request.commands.try_iter().take(MAX_COMMANDS_PER_FRAME) {
                command_count += 1;
                model.handle_command(command);
            }
            let command_batch_full = command_count == MAX_COMMANDS_PER_FRAME;
            let look = *request.look_target.lock();
            match surface.render_model(&mut model, &mut renderer, delta, look) {
                Ok(Some(frame)) => {
                    surface_retry.reset();
                    if !mailbox.is_paused() {
                        presented_frames = presented_frames.saturating_add(1);
                        let should_notify = latest_frame.lock().publish(PresentedFrame {
                            generation: request.generation,
                            frame,
                            presented_at: Instant::now(),
                            presented_frames,
                        });
                        if should_notify {
                            match events.try_send(GpuUnderlayEvent::FrameAvailable {
                                generation: request.generation,
                            }) {
                                Ok(()) => {}
                                Err(TrySendError::Full(_)) => {
                                    latest_frame.lock().notification_failed();
                                }
                                Err(TrySendError::Closed(_)) => break 'worker,
                            }
                        }
                    }
                }
                Ok(None) => render_requested = true,
                Err(GpuFrameError::Cancelled) => {
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        break 'worker;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Model(error)) => {
                    match clear_surface_until_ready(&mut surface, &mailbox) {
                        Ok(ClearSurfaceResult::Cleared) => {}
                        Ok(ClearSurfaceResult::Replaced(replacement)) => {
                            request = replacement;
                            continue 'worker;
                        }
                        Ok(ClearSurfaceResult::Paused) => {}
                        Ok(ClearSurfaceResult::Shutdown) => break 'worker,
                        Err(_) => {
                            let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                                kind: GpuUnavailableKind::SurfaceClear,
                            });
                            break 'worker;
                        }
                    }
                    let _ = events.send_blocking(model_failure_event(
                        ModelFailureStage::Gpu,
                        request.generation,
                        error,
                    ));
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        break 'worker;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Surface(_)) => {
                    let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                        kind: GpuUnavailableKind::Surface,
                    });
                    break 'worker;
                }
            }
            let frame_completed = Instant::now();
            needs_next_frame = model.needs_continuous_frames() || command_batch_full;
            pacer.complete_frame(frame_started, frame_completed);
            if render_requested {
                pacer.postpone_next_frame(Instant::now(), surface_retry.next_delay());
            }
        }
    }
}

/// 重试透明清屏，直到成功 present、generation 被替换或 worker 被关闭。
fn clear_surface_until_ready(
    surface: &mut GpuSurface,
    mailbox: &WorkerMailbox,
) -> Result<ClearSurfaceResult, String> {
    let mut retry = SurfaceRetryBackoff::new(CLEAR_RETRY_INITIAL_DELAY);
    loop {
        match surface.clear()? {
            true => return Ok(ClearSurfaceResult::Cleared),
            false => match wait_for_surface_retry(mailbox, retry.next_delay()) {
                RetryWaitResult::Ready => {}
                RetryWaitResult::Replaced(replacement) => {
                    return Ok(ClearSurfaceResult::Replaced(replacement));
                }
                RetryWaitResult::Paused => return Ok(ClearSurfaceResult::Paused),
                RetryWaitResult::Shutdown => return Ok(ClearSurfaceResult::Shutdown),
            },
        }
    }
}
