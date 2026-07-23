//! 将已推进的 Mocari Drawable 录制到独立 WGPU surface。

use gpui_wgpu::wgpu;
use mocari::render::wgpu::{
    WgpuClippingPlan, WgpuClippingResources, WgpuLive2dRenderer, WgpuMaskRenderTarget,
    WgpuMeshBuffers, WgpuTexture, WgpuTransform,
};

use super::{AnimatedModel, RenderError};
use crate::interaction::RenderedModelFrame;

const MASK_TEXTURE_SIZE: u32 = 512;
const VERTEX_UPLOAD_CHUNK_SIZE: wgpu::BufferAddress = 1024 * 1024;

/// 描述窗口合成器要求的颜色 Alpha 表示。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SurfaceAlphaMode {
    /// surface 接收预乘 Alpha，Mocari 输出可以直接写入。
    Premultiplied,
    /// surface 接收直通 Alpha，需要在最终 pass 中解除预乘。
    Postmultiplied,
}

/// 保存一个模型 generation 内持久复用的 WGPU 资源。
pub(crate) struct GpuModelRenderer {
    renderer: WgpuLive2dRenderer,
    textures: Vec<WgpuTexture>,
    mesh_buffers: WgpuMeshBuffers,
    vertex_upload_belt: wgpu::util::StagingBelt,
    clipping: WgpuClippingResources,
    mask_target: WgpuMaskRenderTarget,
    transform: WgpuTransform,
    straight_alpha_output: Option<StraightAlphaOutput>,
}

impl GpuModelRenderer {
    /// 上传静态纹理和网格，并建立蒙版、变换及可选 Alpha 转换资源。
    ///
    /// # Errors
    ///
    /// 模型网格、纹理、蒙版布局或 GPU 资源无法建立时返回错误。
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: &AnimatedModel,
        target_format: wgpu::TextureFormat,
        alpha_mode: SurfaceAlphaMode,
    ) -> Result<Self, RenderError> {
        let runtime_model = model.runtime_model();
        let cancellation = model.cancellation();
        super::renderer::validate_gpu_model(
            runtime_model.runtime().meshes(),
            runtime_model.textures(),
            model.dimensions()[0],
            model.dimensions()[1],
            model.cancellation(),
        )?;
        let model_format = if alpha_mode == SurfaceAlphaMode::Postmultiplied {
            wgpu::TextureFormat::Rgba16Float
        } else {
            target_format
        };
        let renderer = WgpuLive2dRenderer::new(device, model_format);
        let mut textures = Vec::with_capacity(runtime_model.textures().len());
        for texture in runtime_model.textures() {
            cancellation.checkpoint()?;
            textures.push(
                renderer
                    .create_rgba8_texture(
                        device,
                        queue,
                        texture.width(),
                        texture.height(),
                        texture.rgba(),
                    )
                    .map_err(|error| RenderError::new(format!("无法上传 Live2D 纹理：{error}")))?,
            );
        }
        cancellation.checkpoint()?;
        let mesh_buffers =
            WgpuMeshBuffers::from_drawables(device, runtime_model.runtime().meshes())
                .ok_or_else(|| RenderError::new("无法建立 Live2D GPU 网格缓冲"))?;
        // 动态 drawable 通常产生许多小上传；跨帧复用 staging chunk，避免每次写入都
        // 在 DX12 后端创建并销毁独立资源。
        let vertex_upload_belt =
            wgpu::util::StagingBelt::new(device.clone(), VERTEX_UPLOAD_CHUNK_SIZE);
        cancellation.checkpoint()?;
        let mut plan = WgpuClippingPlan::from_mesh_buffers(&mesh_buffers);
        plan.prepare_single_texture_masks(&mesh_buffers)
            .map_err(|error| RenderError::new(format!("无法规划 Live2D GPU 蒙版：{error}")))?;
        let clipping = renderer
            .create_clipping_resources(device, &plan)
            .map_err(|error| RenderError::new(format!("无法建立 Live2D GPU 蒙版资源：{error}")))?;
        let mask_target = renderer
            .create_mask_render_target(device, MASK_TEXTURE_SIZE)
            .map_err(|error| RenderError::new(format!("无法建立 Live2D GPU 蒙版纹理：{error}")))?;
        cancellation.checkpoint()?;
        let [width, height] = model.dimensions();
        let matrix = model.transform().clip_matrix(width, height);
        let transform = renderer.create_transform(device, &matrix);
        let straight_alpha_output = (alpha_mode == SurfaceAlphaMode::Postmultiplied).then(|| {
            StraightAlphaOutput::new(device, model_format, target_format, [width, height])
        });

        Ok(Self {
            renderer,
            textures,
            mesh_buffers,
            vertex_upload_belt,
            clipping,
            mask_target,
            transform,
            straight_alpha_output,
        })
    }

    /// 推进模型、更新动态网格并把本帧命令录制到目标纹理。
    ///
    /// 返回值只能在对应 command buffer 成功提交并 present 后发布给 UI。
    pub(crate) fn encode_frame(
        &mut self,
        model: &mut AnimatedModel,
        delta: std::time::Duration,
        look: [f32; 2],
        gpu: (&wgpu::Device, &wgpu::Queue),
        encoder: &mut wgpu::CommandEncoder,
        surface_view: &wgpu::TextureView,
    ) -> Result<RenderedModelFrame, RenderError> {
        let (device, queue) = gpu;
        let hit_areas = model.advance_frame(delta, look)?;
        self.update_meshes(device, queue, encoder, model)?;

        let model_target = self
            .straight_alpha_output
            .as_ref()
            .map_or(surface_view, StraightAlphaOutput::model_target);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lunamate.live2d.mask-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.mask_target.view(),
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
            self.renderer
                .draw_masks_with_textures(
                    &mut pass,
                    &self.mesh_buffers,
                    &self.clipping,
                    &self.textures,
                )
                .map_err(|error| RenderError::new(format!("Live2D GPU 蒙版绘制失败：{error}")))?;
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lunamate.live2d.model-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: model_target,
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
            self.renderer
                .draw_with_textures_clipping_and_transform(
                    &mut pass,
                    &self.mesh_buffers,
                    &self.textures,
                    &self.clipping,
                    &self.mask_target,
                    &self.transform,
                )
                .map_err(|error| RenderError::new(format!("Live2D GPU 模型绘制失败：{error}")))?;
        }
        if let Some(output) = &self.straight_alpha_output {
            output.encode(encoder, surface_view);
        }
        // generation 可能在命令录制期间被替换；提交前再次检查，避免发布已过期帧。
        model.cancellation().checkpoint()?;
        self.vertex_upload_belt.finish();

        Ok(RenderedModelFrame::gpu(hit_areas, model.dimensions()))
    }

    /// 在本帧命令提交后请求异步回收 staging chunk，供后续帧重新映射使用。
    pub(crate) fn recall_vertex_uploads(&mut self) {
        self.vertex_upload_belt.recall();
    }

    fn update_meshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        model: &AnimatedModel,
    ) -> Result<(), RenderError> {
        let meshes = model.runtime_model().runtime().meshes();
        super::renderer::validate_gpu_frame(meshes, model.cancellation())?;
        let update =
            match self
                .mesh_buffers
                .update_drawables(encoder, &mut self.vertex_upload_belt, meshes)
            {
                Ok(update) => update,
                Err(_) => {
                    self.mesh_buffers = WgpuMeshBuffers::from_drawables(device, meshes)
                        .ok_or_else(|| RenderError::new("无法重建 Live2D GPU 网格缓冲"))?;
                    self.rebuild_clipping(device)?;
                    return Ok(());
                }
            };

        if update.bounds_changed() || update.visibility_changed() {
            let mut plan = WgpuClippingPlan::from_mesh_buffers(&self.mesh_buffers);
            plan.prepare_single_texture_masks(&self.mesh_buffers)
                .map_err(|error| {
                    RenderError::new(format!("无法更新 Live2D GPU 蒙版规划：{error}"))
                })?;
            let reused = self
                .renderer
                .update_clipping_resources(queue, &mut self.clipping, &plan)
                .map_err(|error| {
                    RenderError::new(format!("无法更新 Live2D GPU 蒙版资源：{error}"))
                })?;
            if !reused {
                self.clipping = self
                    .renderer
                    .create_clipping_resources(device, &plan)
                    .map_err(|error| {
                        RenderError::new(format!("无法重建 Live2D GPU 蒙版资源：{error}"))
                    })?;
            }
        }
        Ok(())
    }

    fn rebuild_clipping(&mut self, device: &wgpu::Device) -> Result<(), RenderError> {
        let mut plan = WgpuClippingPlan::from_mesh_buffers(&self.mesh_buffers);
        plan.prepare_single_texture_masks(&self.mesh_buffers)
            .map_err(|error| RenderError::new(format!("无法重建 Live2D GPU 蒙版规划：{error}")))?;
        self.clipping = self
            .renderer
            .create_clipping_resources(device, &plan)
            .map_err(|error| RenderError::new(format!("无法重建 Live2D GPU 蒙版资源：{error}")))?;
        Ok(())
    }
}

struct StraightAlphaOutput {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
}

impl StraightAlphaOutput {
    fn new(
        device: &wgpu::Device,
        intermediate_format: wgpu::TextureFormat,
        surface_format: wgpu::TextureFormat,
        [width, height]: [u32; 2],
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lunamate.live2d.premultiplied-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: intermediate_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lunamate.live2d.alpha-conversion-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lunamate.live2d.alpha-conversion-bind-group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lunamate.live2d.alpha-conversion-shader"),
            source: wgpu::ShaderSource::Wgsl(STRAIGHT_ALPHA_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lunamate.live2d.alpha-conversion-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lunamate.live2d.alpha-conversion-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            _texture: texture,
            view,
            bind_group,
            pipeline,
        }
    }

    fn model_target(&self) -> &wgpu::TextureView {
        &self.view
    }

    fn encode(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lunamate.live2d.alpha-conversion-pass"),
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

const STRAIGHT_ALPHA_SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return output;
}

@group(0) @binding(0) var frame_texture: texture_2d<f32>;

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureLoad(frame_texture, vec2<i32>(input.position.xy), 0);
    if color.a <= 0.000001 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(color.rgb / color.a, color.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use mocari::moc3::{Moc3DrawableMesh, Moc3DrawableVertex};

    fn triangle_mesh(x_offset: f32) -> Moc3DrawableMesh {
        Moc3DrawableMesh::from_parts(
            0,
            0,
            1.0,
            0.0,
            vec![
                Moc3DrawableVertex::new([x_offset, 0.0], [0.0, 0.0]),
                Moc3DrawableVertex::new([1.0 + x_offset, 0.0], [1.0, 0.0]),
                Moc3DrawableVertex::new([x_offset, 1.0], [0.0, 1.0]),
            ],
            vec![0, 1, 2],
            Vec::new(),
        )
    }

    #[test]
    fn postmultiplied_alpha_pipeline_accepts_float_intermediate_texture() {
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let _output = StraightAlphaOutput::new(
            &device,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Bgra8Unorm,
            [32, 32],
        );
    }

    #[test]
    fn vendored_mocari_renderer_accepts_gpui_wgpu_device() {
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let _renderer = WgpuLive2dRenderer::new(&device, wgpu::TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn changed_vertices_upload_through_reusable_staging_belt() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let initial = triangle_mesh(0.0);
        let changed = triangle_mesh(0.25);
        let mut buffers = WgpuMeshBuffers::from_drawables(&device, std::slice::from_ref(&initial))
            .expect("有效三角形应创建 GPU 网格缓冲");
        let mut staging_belt = wgpu::util::StagingBelt::new(device.clone(), 4096);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test.live2d.vertex-upload"),
        });

        let update = buffers
            .update_drawables(
                &mut encoder,
                &mut staging_belt,
                std::slice::from_ref(&changed),
            )
            .expect("静态拓扑相同的顶点更新应成功");
        assert_eq!(update.uploaded_drawables(), 1);

        staging_belt.finish();
        let submission = queue.submit([encoder.finish()]);
        staging_belt.recall();
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(std::time::Duration::from_secs(1)),
        });

        let mut unchanged_encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("test.live2d.unchanged-vertex-upload"),
            });
        let unchanged = buffers
            .update_drawables(
                &mut unchanged_encoder,
                &mut staging_belt,
                std::slice::from_ref(&changed),
            )
            .expect("相同顶点数据应保持可复用");
        assert_eq!(unchanged.uploaded_drawables(), 0);
    }
}
