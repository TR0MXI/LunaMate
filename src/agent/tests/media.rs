//! 验证对话图片规范化边界与快照数据最小化。

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

use crate::agent::media::prepare_dynamic_image;

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
