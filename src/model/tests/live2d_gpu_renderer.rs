use gpui_wgpu::wgpu;
use mocari::{
    moc3::{Moc3DrawableMesh, Moc3DrawableVertex},
    render::wgpu::{WgpuLive2dRenderer, WgpuMeshBuffers},
};

use crate::model::live2d::gpu_renderer::StraightAlphaOutput;

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
fn maintained_mocari_renderer_accepts_gpui_wgpu_device() {
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

    let mut unchanged_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
