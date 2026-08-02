//! 建立并独占 WGPU surface、adapter、device 与呈现资源。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::executor::block_on;
use gpui_wgpu::wgpu;
use parking_lot::Mutex;

use crate::{
    config::CONFIG,
    platform::{InitializationCancellation, SurfaceFactory, SurfaceSeed},
};

use super::super::super::{
    interaction::RenderedModelFrame,
    live2d::{AnimatedModel, GpuModelRenderer, SurfaceAlphaMode},
};
use super::{GpuFrameError, GpuSurface, present_mode_for_frame_rate};

impl GpuSurface {
    pub(super) fn new(
        factory: SurfaceFactory,
        size: super::super::GpuUnderlaySize,
        mailbox: Arc<super::super::WorkerMailbox>,
    ) -> Result<Option<Self>, String> {
        // SurfaceSeed 在全部可失败初始化完成前保持整体所有权，确保提前返回时先释放
        // WGPU surface，再释放 Wayland child/AppKit 关联资源，最后释放 Instance。
        let seed = factory.create(mailbox.as_ref())?;
        if mailbox.is_shutdown() {
            return Ok(None);
        }
        let adapter = block_on(seed.instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&seed.surface),
        }))
        .map_err(|error| format!("找不到兼容 Live2D surface 的 GPU adapter：{error}"))?;
        if mailbox.is_shutdown() {
            return Ok(None);
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("lunamate.live2d.device"),
            ..Default::default()
        }))
        .map_err(|error| format!("无法创建 Live2D GPU device：{error}"))?;
        let device_lost = Arc::new(AtomicBool::new(false));
        let lost_flag = device_lost.clone();
        let lost_wake = mailbox.clone();
        device.set_device_lost_callback(move |_, _| {
            lost_flag.store(true, Ordering::Release);
            lost_wake.wake();
        });
        let device_error = Arc::new(Mutex::new(None));
        let uncaptured_error = device_error.clone();
        let error_wake = mailbox.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            *uncaptured_error.lock() = Some(error.to_string());
            error_wake.wake();
        }));

        let capabilities = seed.surface.get_capabilities(&adapter);
        let format = mocari::render::wgpu::preferred_surface_format(&capabilities.formats)
            .ok_or_else(|| "Live2D surface 没有可用颜色格式".to_owned())?;
        let (composite_alpha, alpha_mode) = if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            (
                wgpu::CompositeAlphaMode::PreMultiplied,
                SurfaceAlphaMode::Premultiplied,
            )
        } else if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
        {
            (
                wgpu::CompositeAlphaMode::PostMultiplied,
                SurfaceAlphaMode::Postmultiplied,
            )
        } else {
            return Err("Live2D surface 不支持透明 Alpha 合成".to_owned());
        };
        let [width, height] = size.physical;
        let mut config = seed
            .surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| "GPU adapter 无法配置 Live2D surface".to_owned())?;
        config.format = format;
        config.alpha_mode = composite_alpha;
        config.present_mode =
            present_mode_for_frame_rate(CONFIG.frame_rate(), &capabilities.present_modes);
        seed.surface.configure(&device, &config);
        if let Some(error) = device_error.lock().take() {
            return Err(format!("配置 Live2D GPU surface 失败：{error}"));
        }
        if mailbox.is_shutdown() {
            return Ok(None);
        }
        let SurfaceSeed {
            surface,
            owner,
            instance,
        } = seed;

        Ok(Some(Self {
            surface,
            _owner: owner,
            _instance: instance,
            _adapter: adapter,
            device,
            queue,
            config,
            supported_present_modes: capabilities.present_modes,
            alpha_mode,
            device_lost,
            device_error,
            size,
        }))
    }

    pub(super) fn resize(&mut self, size: super::super::GpuUnderlaySize) -> Result<(), String> {
        let [width, height] = size.physical;
        if width == 0 || height == 0 {
            return Err("Live2D surface 尺寸必须非零".to_owned());
        }
        if self.size == size {
            return Ok(());
        }
        self.size = size;
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        // 与切换呈现模式一致：立即取出本次 configure 的校验错误，避免它被归因到后续帧。
        if let Some(error) = self.device_error.lock().take() {
            return Err(format!("配置 Live2D GPU surface 失败：{error}"));
        }
        Ok(())
    }

    /// 在运行时切换限帧设置时同步更新 swapchain 呈现模式。
    pub(super) fn set_present_mode_for_frame_rate(
        &mut self,
        frame_rate: crate::config::FrameRate,
    ) -> Result<(), String> {
        let present_mode = present_mode_for_frame_rate(frame_rate, &self.supported_present_modes);
        if self.config.present_mode == present_mode {
            return Ok(());
        }

        self.config.present_mode = present_mode;
        self.surface.configure(&self.device, &self.config);
        if let Some(error) = self.device_error.lock().take() {
            return Err(format!("切换 Live2D GPU 呈现模式失败：{error}"));
        }
        Ok(())
    }

    pub(super) fn clear(&mut self) -> Result<bool, String> {
        match self.render_surface(|device, queue, target| {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lunamate.live2d.clear-encoder"),
            });
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("lunamate.live2d.clear-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            queue.submit([encoder.finish()]);
            Ok(())
        }) {
            Ok(rendered) => Ok(rendered),
            Err(GpuFrameError::Surface(error)) => Err(error),
            Err(GpuFrameError::Cancelled | GpuFrameError::Model(_)) => {
                Err("清空 Live2D surface 时发生内部状态错误".to_owned())
            }
        }
    }

    pub(super) fn render_model(
        &mut self,
        model: &mut AnimatedModel,
        renderer: &mut GpuModelRenderer,
        delta: Duration,
        look: [f32; 2],
    ) -> Result<Option<RenderedModelFrame>, GpuFrameError> {
        let mut frame = None;
        let rendered = self.render_surface(|device, queue, target| {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lunamate.live2d.frame-encoder"),
            });
            let encoded = renderer
                .encode_frame(model, delta, look, (device, queue), &mut encoder, target)
                .map_err(|error| {
                    if error.is_cancelled() {
                        GpuFrameError::Cancelled
                    } else {
                        GpuFrameError::Model(error.to_string())
                    }
                })?;
            queue.submit([encoder.finish()]);
            renderer.recall_vertex_uploads();
            frame = Some(encoded);
            Ok(())
        })?;
        Ok(rendered.then_some(frame).flatten())
    }

    fn render_surface(
        &mut self,
        mut encode: impl FnMut(
            &wgpu::Device,
            &wgpu::Queue,
            &wgpu::TextureView,
        ) -> Result<(), GpuFrameError>,
    ) -> Result<bool, GpuFrameError> {
        if self.device_lost.load(Ordering::Acquire) {
            return Err(GpuFrameError::Surface(
                "Live2D GPU device 已丢失".to_owned(),
            ));
        }
        if let Some(error) = self.device_error.lock().take() {
            return Err(GpuFrameError::Surface(format!(
                "Live2D GPU 操作失败：{error}"
            )));
        }
        self._owner
            .prepare_present(self.size)
            .map_err(GpuFrameError::Surface)?;
        let (surface_texture, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => (texture, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => (texture, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return Err(GpuFrameError::Surface(
                    "Live2D GPU surface 已丢失".to_owned(),
                ));
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(GpuFrameError::Surface(
                    "获取 Live2D GPU surface texture 时发生校验错误".to_owned(),
                ));
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        encode(&self.device, &self.queue, &view)?;
        surface_texture.present();
        if suboptimal {
            self.surface.configure(&self.device, &self.config);
        }
        if let Some(error) = self.device_error.lock().take() {
            return Err(GpuFrameError::Surface(format!(
                "Live2D GPU 提交失败：{error}"
            )));
        }
        Ok(true)
    }
}

impl Drop for GpuSurface {
    fn drop(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(1)),
        });
    }
}
