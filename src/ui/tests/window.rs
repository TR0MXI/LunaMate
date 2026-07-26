use crate::{
    config::ModelWindowSize,
    model::GpuUnderlaySize,
    ui::window::{
        DISPLAY_MARGIN_FRACTION, MAX_RASTER_DIMENSION, MAX_WINDOW_WIDTH, MIN_WINDOW_WIDTH,
        PHONE_ASPECT_RATIO, WINDOW_HEIGHT_FRACTION, desktop_pet_window_min_size,
        desktop_pet_window_size, gpu_underlay_size, raster_dimensions_for_window,
    },
};

fn assert_close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
}

#[test]
fn desktop_pet_window_uses_a_phone_aspect_ratio() {
    let [width, height] = desktop_pet_window_size(1920.0, 1080.0, ModelWindowSize::Auto);
    assert_close(height, 1080.0 * WINDOW_HEIGHT_FRACTION);
    assert_close(height / width, PHONE_ASPECT_RATIO);

    let [width, height] = desktop_pet_window_size(2560.0, 1440.0, ModelWindowSize::Auto);
    assert_eq!(width, MAX_WINDOW_WIDTH);
    assert_close(height, MAX_WINDOW_WIDTH * PHONE_ASPECT_RATIO);
}

#[test]
fn desktop_pet_window_stays_on_small_displays() {
    let [width, height] = desktop_pet_window_size(800.0, 600.0, ModelWindowSize::Auto);
    assert_eq!(width, MIN_WINDOW_WIDTH);
    assert_close(height / width, PHONE_ASPECT_RATIO);

    let [width, height] = desktop_pet_window_size(320.0, 240.0, ModelWindowSize::Auto);
    assert!(width <= 320.0 * DISPLAY_MARGIN_FRACTION);
    assert!(height <= 240.0 * DISPLAY_MARGIN_FRACTION);

    let min_size = desktop_pet_window_min_size(320.0, 240.0);
    assert!(f32::from(min_size.width) <= width);
    assert!(f32::from(min_size.height) <= height);
}

#[test]
fn configured_window_size_presets_resize_the_window_instead_of_the_model() {
    for (preset, expected_width) in [
        (ModelWindowSize::Compact, 240.0),
        (ModelWindowSize::Standard, 300.0),
        (ModelWindowSize::Large, 360.0),
        (ModelWindowSize::ExtraLarge, 420.0),
    ] {
        let [width, height] = desktop_pet_window_size(1920.0, 1080.0, preset);
        assert_close(width, expected_width);
        assert_close(height / width, PHONE_ASPECT_RATIO);
    }
}

#[test]
fn raster_dimensions_preserve_aspect_ratio_and_cap_the_long_edge() {
    assert_eq!(raster_dimensions_for_window(360.0, 640.0, 1.0), [450, 800]);
    assert_eq!(
        raster_dimensions_for_window(360.0, 640.0, 2.0),
        [720, MAX_RASTER_DIMENSION]
    );
}

#[test]
fn gpu_underlay_uses_native_physical_pixels_and_logical_compositor_size() {
    assert_eq!(
        gpu_underlay_size(360.0, 640.0, 2.0),
        GpuUnderlaySize {
            physical: [720, 1280],
            logical: [360, 640],
        }
    );
    assert_eq!(
        gpu_underlay_size(300.2, 500.2, 1.25),
        GpuUnderlaySize {
            physical: [375, 625],
            logical: [300, 500],
        }
    );
}

#[test]
fn gpu_underlay_physical_size_stays_an_integer_multiple_of_the_logical_size() {
    // 桌宠高度为宽度的 16/9，分数结果必须仍能被合成器表示为整数 buffer scale。
    let width = MIN_WINDOW_WIDTH;
    let height = width * PHONE_ASPECT_RATIO;
    for (scale_factor, expected_scale) in [(1.0, 1), (2.0, 2), (3.0, 3)] {
        let size = gpu_underlay_size(width, height, scale_factor);
        for axis in 0..2 {
            assert_eq!(size.physical[axis], size.logical[axis] * expected_scale);
        }
    }
}
