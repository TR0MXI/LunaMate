//! 验证对话图片规范化边界与快照数据最小化。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

use crate::agent::media::{ImageAttachment, ImageInputError, load_image, prepare_dynamic_image};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-agent-media-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试目录应当可以创建");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &Path, width: u32, height: u32) {
    DynamicImage::new_rgb8(width, height)
        .save_with_format(path, ImageFormat::Png)
        .expect("测试 PNG 应当可以写入");
}

/// 改写合法 PNG 的 IHDR 声明尺寸，模拟解压炸弹：文件头很小但声明的像素数极大。
fn png_declaring(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = {
        let mut buffer = std::io::Cursor::new(Vec::new());
        DynamicImage::new_rgb8(1, 1)
            .write_to(&mut buffer, ImageFormat::Png)
            .expect("测试 PNG 应当可以编码");
        buffer.into_inner()
    };
    // PNG 布局固定：8 字节签名 + 4 字节长度后紧跟 IHDR 块类型、13 字节数据与 CRC。
    const CHUNK: std::ops::Range<usize> = 12..29;
    bytes[16..20].copy_from_slice(&width.to_be_bytes());
    bytes[20..24].copy_from_slice(&height.to_be_bytes());
    let checksum = png_crc32(&bytes[CHUNK]).to_be_bytes();
    bytes[CHUNK.end..CHUNK.end + 4].copy_from_slice(&checksum);
    bytes
}

fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn large_image_is_resized_and_encoded_as_bounded_jpeg() {
    let image = DynamicImage::new_rgb8(4_096, 1_024);
    let attachment =
        prepare_dynamic_image(image, "large.jpg".to_owned()).expect("有效大图应当可以缩放");
    let bytes = attachment.bytes().expect("当前进程应保留图片内容");

    assert_eq!((attachment.width(), attachment.height()), (2_048, 512));
    assert!(bytes.len() <= 4 * 1024 * 1024);
    assert_eq!(
        image::guess_format(bytes).expect("输出图片格式应当可以识别"),
        ImageFormat::Jpeg
    );
}

#[test]
fn transparent_pixels_are_flattened_before_jpeg_encoding() {
    let source = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 255, 0]));
    let attachment = prepare_dynamic_image(
        DynamicImage::ImageRgba8(source),
        "transparent.jpg".to_owned(),
    )
    .expect("透明图片应当可以规范化");
    let decoded =
        image::load_from_memory(attachment.bytes().expect("规范化图片内容应当仍在内存中"))
            .expect("规范化 JPEG 应当可以解码")
            .to_rgb8();
    let pixel = decoded.get_pixel(0, 0);

    assert!(pixel.0.iter().all(|channel| *channel > 240));
}

#[test]
fn unicode_filename_is_bounded_by_utf8_bytes() {
    let name = format!("{}.png", "图".repeat(96));
    let attachment = prepare_dynamic_image(DynamicImage::new_rgb8(2, 2), name)
        .expect("Unicode 文件名不应影响图片规范化");

    assert!(attachment.name().len() <= 128);
    assert!(attachment.name().ends_with(".jpg"));
}

#[test]
fn zero_sized_images_are_rejected_before_encoding() {
    assert_eq!(
        prepare_dynamic_image(DynamicImage::new_rgb8(0, 0), "empty.png".to_owned()),
        Err(ImageInputError::DimensionsTooLarge)
    );
}

#[test]
fn small_images_keep_their_original_dimensions() {
    let attachment = prepare_dynamic_image(DynamicImage::new_rgb8(64, 32), "small.png".to_owned())
        .expect("小图不应被缩放");

    assert_eq!((attachment.width(), attachment.height()), (64, 32));
    assert_eq!(attachment.name(), "small.jpg");
    assert!(attachment.byte_len() > 0);
    assert!(attachment.has_safe_metadata());
}

#[test]
fn user_selected_file_is_normalized_and_named_after_its_stem() {
    let directory = TestDirectory::new();
    let source = directory.path().join("头像.png");
    write_png(&source, 40, 20);

    let attachment = load_image(&source).expect("有效 PNG 文件应当可以加载");

    assert_eq!(attachment.name(), "头像.jpg");
    assert_eq!((attachment.width(), attachment.height()), (40, 20));
    assert_eq!(
        image::guess_format(attachment.bytes().expect("加载后应保留图片内容"))
            .expect("输出应当是可识别格式"),
        ImageFormat::Jpeg
    );
}

#[test]
fn missing_and_non_regular_paths_are_reported_as_unreadable() {
    let directory = TestDirectory::new();

    assert_eq!(
        load_image(&directory.path().join("absent.png")),
        Err(ImageInputError::Unreadable)
    );
    assert_eq!(
        load_image(directory.path()),
        Err(ImageInputError::Unreadable)
    );
}

#[test]
fn oversized_source_files_are_rejected_without_reading_them() {
    let directory = TestDirectory::new();
    let source = directory.path().join("huge.png");
    let file = fs::File::create(&source).expect("测试大文件应当可以创建");
    // 稀疏文件只声明长度，用于验证大小上限在读取内容之前生效。
    file.set_len(20 * 1024 * 1024 + 1)
        .expect("测试大文件应当可以设置长度");
    drop(file);

    assert_eq!(load_image(&source), Err(ImageInputError::SourceTooLarge));
}

#[test]
fn non_image_and_unsupported_formats_are_rejected() {
    let directory = TestDirectory::new();
    let text = directory.path().join("notes.png");
    fs::write(&text, b"this is definitely not an image").expect("测试文本应当可以写入");
    let gif = directory.path().join("animation.gif");
    fs::write(&gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").expect("测试 GIF 应当可以写入");

    assert_eq!(load_image(&text), Err(ImageInputError::UnsupportedFormat));
    assert_eq!(load_image(&gif), Err(ImageInputError::UnsupportedFormat));
}

#[test]
fn declared_dimensions_are_validated_before_pixels_are_decoded() {
    let directory = TestDirectory::new();
    let edge = directory.path().join("wide.png");
    fs::write(&edge, png_declaring(16_385, 1)).expect("测试 PNG 头应当可以写入");
    let bomb = directory.path().join("bomb.png");
    fs::write(&bomb, png_declaring(16_000, 16_000)).expect("测试 PNG 头应当可以写入");

    assert_eq!(load_image(&edge), Err(ImageInputError::DimensionsTooLarge));
    assert_eq!(load_image(&bomb), Err(ImageInputError::DimensionsTooLarge));
}

#[test]
fn truncated_pixel_data_is_reported_as_a_decode_failure() {
    let directory = TestDirectory::new();
    let source = directory.path().join("truncated.png");
    let mut bytes = {
        let mut buffer = std::io::Cursor::new(Vec::new());
        DynamicImage::new_rgb8(16, 16)
            .write_to(&mut buffer, ImageFormat::Png)
            .expect("测试 PNG 应当可以编码");
        buffer.into_inner()
    };
    bytes.truncate(bytes.len() - 16);
    fs::write(&source, bytes).expect("截断 PNG 应当可以写入");

    assert_eq!(load_image(&source), Err(ImageInputError::Decode));
}

#[test]
fn snapshots_keep_metadata_but_drop_pixel_contents() {
    let attachment = prepare_dynamic_image(DynamicImage::new_rgb8(8, 8), "avatar.png".to_owned())
        .expect("测试图片应当可以规范化");

    let encoded = serde_json::to_string(&attachment).expect("图片附件应当可以序列化");
    assert!(!encoded.contains("bytes"));

    let restored: ImageAttachment =
        serde_json::from_str(&encoded).expect("图片附件应当可以反序列化");
    assert_eq!(restored.name(), attachment.name());
    assert_eq!(restored.width(), attachment.width());
    assert_eq!(restored.height(), attachment.height());
    assert!(restored.bytes().is_none());
    assert_eq!(restored.byte_len(), 0);
    assert!(restored.has_safe_metadata());
}

#[test]
fn debug_output_reports_availability_instead_of_pixels() {
    let attachment = prepare_dynamic_image(DynamicImage::new_rgb8(4, 4), "avatar.png".to_owned())
        .expect("测试图片应当可以规范化");

    let rendered = format!("{attachment:?}");

    assert!(rendered.contains("available: true"));
    assert!(!rendered.contains("bytes: ["));
}

#[test]
fn restored_snapshots_with_tampered_metadata_are_rejected() {
    let tampered: ImageAttachment = serde_json::from_str(r#"{"name":"","width":8,"height":8}"#)
        .expect("空名称快照应当可以反序列化");
    assert!(!tampered.has_safe_metadata());

    let oversized: ImageAttachment =
        serde_json::from_str(r#"{"name":"a.jpg","width":4096,"height":8}"#)
            .expect("超边长快照应当可以反序列化");
    assert!(!oversized.has_safe_metadata());

    let empty: ImageAttachment = serde_json::from_str(r#"{"name":"a.jpg","width":0,"height":8}"#)
        .expect("零宽快照应当可以反序列化");
    assert!(!empty.has_safe_metadata());
}

#[test]
fn image_errors_map_to_distinct_localized_messages() {
    let categories = [
        ImageInputError::Unreadable,
        ImageInputError::SourceTooLarge,
        ImageInputError::UnsupportedFormat,
        ImageInputError::DimensionsTooLarge,
        ImageInputError::Decode,
        ImageInputError::ScreenCapture,
    ];

    let mut messages = Vec::new();
    for category in categories {
        let message = category.to_string();
        assert!(!message.is_empty(), "{category:?} 应当有本地化说明");
        assert!(!messages.contains(&message), "{category:?} 的说明应当唯一");
        messages.push(message);
    }

    // 编码与超限都属于内部准备失败，对用户使用同一条脱敏说明。
    assert_eq!(
        ImageInputError::Encode.to_string(),
        ImageInputError::OutputTooLarge.to_string()
    );
}

/// 截屏依赖真实桌面会话：Windows/macOS 需要屏幕录制授权，Linux 需要 XDG Screenshot
/// portal 与用户确认。CI 与无头环境无法满足，需用户在目标桌面手动运行验证。
#[test]
#[ignore = "需要真实桌面会话与截屏授权，请在目标桌面环境手动运行"]
fn primary_screen_capture_produces_a_bounded_attachment() {
    let attachment = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("测试必须能创建 Tokio 运行时")
        .block_on(crate::agent::media::capture_primary_screen())
        .expect("授权后的截屏应当成功");

    assert!(attachment.has_safe_metadata());
    assert_eq!(attachment.name(), "screenshot.jpg");
    assert!(attachment.byte_len() <= 4 * 1024 * 1024);
}
