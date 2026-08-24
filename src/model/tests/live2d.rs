use std::{
    error::Error,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::model::{AnimatedModel, ModelLoadError, RenderCancellation, live2d::render_model};

#[test]
fn invalid_raster_dimensions_have_a_distinct_fatal_error() {
    let result = AnimatedModel::load_path_for_test(
        Path::new("unused.model3.json"),
        0,
        128,
        RenderCancellation::default(),
    );
    let Err(error) = result else {
        panic!("零宽光栅必须在读取模型前失败");
    };

    assert!(matches!(
        &error,
        ModelLoadError::InvalidRasterDimensions {
            width: 0,
            height: 128
        }
    ));
    assert!(error.source().is_none());
}

#[test]
fn missing_manifest_is_a_required_resource_fatal_error() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间必须晚于 Unix 纪元")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "lunamate-missing-model-{}-{nonce}.model3.json",
        std::process::id()
    ));

    let result = AnimatedModel::load_path_for_test(&path, 1, 1, RenderCancellation::default());
    let Err(error) = result else {
        panic!("缺失清单必须阻止主体加载");
    };

    assert!(matches!(&error, ModelLoadError::RequiredResources(_)));
    assert!(error.source().is_some());
}

#[test]
fn cancelled_generation_stops_before_reading_model_resources() {
    let cancellation = RenderCancellation::default();
    cancellation.cancel();

    let result =
        AnimatedModel::load_path_for_test(Path::new("unused.model3.json"), 128, 128, cancellation);
    let Err(error) = result else {
        panic!("已取消 generation 不得继续读取模型");
    };

    assert!(error.is_cancelled());
    assert!(error.source().is_none());
}

#[test]
#[ignore = "需要本地授权的 Hiyori 模型；提交最小 fixture 后应移除此标记"]
fn renders_local_model_when_available() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models/hiyori_free/runtime/hiyori_free_t08.model3.json");

    let image = render_model(&path, 128).expect("local test model should render");
    let bytes = image.as_bytes(0).expect("render should contain one frame");
    assert!(bytes.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
    let eye_pixels = (15..25)
        .flat_map(|y| (52..76).map(move |x| (y * 128 + x) * 4))
        .filter(|offset| {
            let pixel = &bytes[*offset..*offset + 4];
            pixel[3] > 128 && pixel[0] > pixel[2].saturating_add(8)
        })
        .count();
    assert!(eye_pixels > 0, "idle pose should render blue eye details");
}

#[test]
#[ignore = "需要本地授权的 Hiyori 模型；提交最小 fixture 后应移除此标记"]
fn idle_motion_changes_rendered_frame_when_local_model_is_available() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models/hiyori_free/runtime/hiyori_free_t08.model3.json");

    let mut model =
        AnimatedModel::load_path_for_test(&path, 128, 128, RenderCancellation::default())
            .expect("local test model should load");
    let first = model
        .render_frame(Duration::ZERO, [0.0, 0.0])
        .expect("first frame should render");
    let second = model
        .render_frame(Duration::from_millis(100), [0.5, -0.5])
        .expect("animated frame should render");
    assert_ne!(
        first.image().expect("CPU 首帧必须包含图像").as_bytes(0),
        second.image().expect("CPU 动画帧必须包含图像").as_bytes(0)
    );
}

#[test]
#[ignore = "需要本地授权的 Hiyori 模型；提交最小 fixture 后应移除此标记"]
fn renders_to_a_rectangular_target_when_local_model_is_available() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models/hiyori_free/runtime/hiyori_free_t08.model3.json");

    let mut model =
        AnimatedModel::load_path_for_test(&path, 72, 128, RenderCancellation::default())
            .expect("rectangular render target should load");
    let image = model
        .render_frame(Duration::ZERO, [0.0, 0.0])
        .expect("rectangular frame should render");
    let bytes = image
        .image()
        .expect("CPU 矩形帧必须包含图像")
        .as_bytes(0)
        .expect("render should contain one frame");

    assert_eq!(bytes.len(), 72 * 128 * 4);
    assert!(bytes.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
}
