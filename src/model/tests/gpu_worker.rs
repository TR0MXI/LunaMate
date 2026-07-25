use std::time::Duration;

use gpui_wgpu::wgpu;

use crate::{
    config::FrameRate,
    model::gpu_underlay::{
        GpuUnderlayEvent,
        worker::{
            ModelFailureStage, SurfaceRetryBackoff, model_failure_event,
            present_mode_for_frame_rate,
        },
    },
};

#[test]
fn model_failure_stages_map_to_distinct_events() {
    let load = model_failure_event(ModelFailureStage::Load, 7, "load".to_owned());
    assert!(matches!(
        load,
        GpuUnderlayEvent::ModelLoadFailed {
            generation: 7,
            error
        } if error == "load"
    ));

    let gpu = model_failure_event(ModelFailureStage::Gpu, 8, "gpu".to_owned());
    assert!(matches!(
        gpu,
        GpuUnderlayEvent::ModelGpuFailed {
            generation: 8,
            error
        } if error == "gpu"
    ));
}

#[test]
fn preset_frame_rates_keep_fifo_presentation() {
    let modes = [
        wgpu::PresentMode::Fifo,
        wgpu::PresentMode::Immediate,
        wgpu::PresentMode::Mailbox,
    ];

    assert_eq!(
        present_mode_for_frame_rate(FrameRate::Fps120, &modes),
        wgpu::PresentMode::Fifo
    );
}

#[test]
fn custom_frame_rate_avoids_the_display_vsync_cap() {
    let modes = [
        wgpu::PresentMode::Fifo,
        wgpu::PresentMode::Mailbox,
        wgpu::PresentMode::Immediate,
    ];
    let custom = FrameRate::custom(240).expect("测试帧率必须有效");

    assert_eq!(custom.limit(), Some(240));
    assert!(!custom.allows_frame_rate_degradation());
    assert_eq!(
        present_mode_for_frame_rate(custom, &modes),
        wgpu::PresentMode::Immediate
    );
}

#[test]
fn follow_display_keeps_fifo_presentation_without_a_software_limit() {
    let modes = [
        wgpu::PresentMode::Fifo,
        wgpu::PresentMode::Immediate,
        wgpu::PresentMode::Mailbox,
    ];

    assert_eq!(FrameRate::FollowDisplay.limit(), None);
    assert!(!FrameRate::FollowDisplay.allows_frame_rate_degradation());
    assert_eq!(
        present_mode_for_frame_rate(FrameRate::FollowDisplay, &modes),
        wgpu::PresentMode::Fifo
    );
}

#[test]
fn unlimited_presentation_prefers_immediate_when_available() {
    let modes = [
        wgpu::PresentMode::Fifo,
        wgpu::PresentMode::Mailbox,
        wgpu::PresentMode::Immediate,
    ];

    assert_eq!(
        present_mode_for_frame_rate(FrameRate::Unlimited, &modes),
        wgpu::PresentMode::Immediate
    );
}

#[test]
fn unlimited_presentation_uses_mailbox_before_fifo_fallback() {
    assert_eq!(
        present_mode_for_frame_rate(
            FrameRate::Unlimited,
            &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox]
        ),
        wgpu::PresentMode::Mailbox
    );
    assert_eq!(
        present_mode_for_frame_rate(FrameRate::Unlimited, &[wgpu::PresentMode::Fifo]),
        wgpu::PresentMode::Fifo
    );
}

#[test]
fn surface_retry_backoff_is_bounded_and_resettable() {
    let mut retry = SurfaceRetryBackoff::new(Duration::from_millis(16));
    assert_eq!(retry.next_delay(), Duration::from_millis(16));
    assert_eq!(retry.next_delay(), Duration::from_millis(32));
    for _ in 0..16 {
        let _ = retry.next_delay();
    }
    assert_eq!(retry.next_delay(), Duration::from_secs(1));

    retry.reset();
    assert_eq!(retry.next_delay(), Duration::from_millis(16));
}
