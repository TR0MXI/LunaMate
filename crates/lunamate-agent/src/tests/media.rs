//! 验证对话图片规范化边界与快照数据最小化。

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

use crate::{
    config::AppLanguage,
    media::{ImageAttachment, ImageInputError, prepare_dynamic_image, prepare_image},
};

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    DynamicImage::new_rgb8(width, height)
        .write_to(&mut buffer, ImageFormat::Png)
        .expect("测试 PNG 应当可以编码");
    buffer.into_inner()
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
fn source_bytes_are_normalized_and_named_after_the_supplied_stem() {
    let attachment =
        prepare_image(&png(40, 20), "头像.png".to_owned()).expect("有效 PNG 字节应当可以加载");

    assert_eq!(attachment.name(), "头像.jpg");
    assert_eq!((attachment.width(), attachment.height()), (40, 20));
    assert_eq!(
        image::guess_format(attachment.bytes().expect("加载后应保留图片内容"))
            .expect("输出应当是可识别格式"),
        ImageFormat::Jpeg
    );
}

#[test]
fn non_image_and_unsupported_formats_are_rejected() {
    assert_eq!(
        prepare_image(b"this is definitely not an image", "notes.png".to_owned()),
        Err(ImageInputError::UnsupportedFormat)
    );
    assert_eq!(
        prepare_image(
            b"GIF89a\x01\x00\x01\x00\x00\x00\x00;",
            "animation.gif".to_owned()
        ),
        Err(ImageInputError::UnsupportedFormat)
    );
}

#[test]
fn declared_dimensions_are_validated_before_pixels_are_decoded() {
    assert_eq!(
        prepare_image(&png_declaring(16_385, 1), "wide.png".to_owned()),
        Err(ImageInputError::DimensionsTooLarge)
    );
    assert_eq!(
        prepare_image(&png_declaring(16_000, 16_000), "bomb.png".to_owned()),
        Err(ImageInputError::DimensionsTooLarge)
    );
}

#[test]
fn truncated_pixel_data_is_reported_as_a_decode_failure() {
    let mut bytes = png(16, 16);
    bytes.truncate(bytes.len() - 16);

    assert_eq!(
        prepare_image(&bytes, "truncated.png".to_owned()),
        Err(ImageInputError::Decode)
    );
}

#[test]
fn snapshots_keep_only_an_opaque_presence_marker() {
    let attachment = prepare_dynamic_image(
        DynamicImage::new_rgb8(8, 7),
        "private-avatar.png".to_owned(),
    )
    .expect("测试图片应当可以规范化");

    let encoded = serde_json::to_string(&attachment).expect("图片附件应当可以序列化");
    assert_eq!(encoded, "true");
    assert!(!encoded.contains(attachment.name()));
    assert!(!encoded.contains(&attachment.width().to_string()));
    assert!(!encoded.contains(&attachment.height().to_string()));

    let restored: ImageAttachment =
        serde_json::from_str(&encoded).expect("图片附件应当可以反序列化");
    assert_eq!(restored.name(), "image.jpg");
    assert_eq!((restored.width(), restored.height()), (1, 1));
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
    assert!(!rendered.contains("avatar"));
    assert!(!rendered.contains("bytes: ["));
}

#[test]
fn malformed_or_legacy_snapshot_markers_are_rejected() {
    for malformed in [
        "false",
        "{}",
        r#"{"name":"old.jpg","width":8,"height":8}"#,
        r#""true""#,
        "1",
    ] {
        assert!(
            serde_json::from_str::<ImageAttachment>(malformed).is_err(),
            "非法图片标记不应被接受：{malformed}"
        );
    }
}

#[test]
fn image_errors_map_to_distinct_localized_messages() {
    let language = AppLanguage::English;
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
        let message = category.localized_message(language);
        assert!(!message.is_empty(), "{category:?} 应当有本地化说明");
        assert!(!messages.contains(&message), "{category:?} 的说明应当唯一");
        messages.push(message);
    }

    // 编码与超限都属于内部准备失败，对用户使用同一条脱敏说明。
    assert_eq!(
        ImageInputError::Encode.localized_message(language),
        ImageInputError::OutputTooLarge.localized_message(language)
    );
}

#[test]
fn image_error_language_is_explicit() {
    let cases = [
        (AppLanguage::SimplifiedChinese, "无法读取所选图片"),
        (AppLanguage::TraditionalChinese, "無法讀取所選圖片"),
        (AppLanguage::English, "The selected image could not be read"),
        (AppLanguage::Japanese, "選択した画像を読み取れませんでした"),
    ];

    for (language, expected) in cases {
        assert_eq!(
            ImageInputError::Unreadable.localized_message(language),
            expected
        );
    }
}
