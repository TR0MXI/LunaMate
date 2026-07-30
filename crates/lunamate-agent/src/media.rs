//! 校验并规范化宿主提供的对话图片字节。

use std::{fmt, io::Cursor, path::Path, sync::Arc};

use image::{
    DynamicImage, ExtendedColorType, GenericImageView as _, ImageFormat, Rgb, RgbImage,
    codecs::jpeg::JpegEncoder, imageops::FilterType,
};
use rust_i18n::t;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::config::AppLanguage;

// 宿主对用户文件使用更低上限；这里还需容纳平台截图适配器的 64 MiB 输入边界。
const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SOURCE_EDGE: u32 = 16_384;
const MAX_SOURCE_PIXELS: u64 = 40_000_000;
const MAX_OUTPUT_EDGE: u32 = 2_048;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const JPEG_QUALITIES: [u8; 3] = [82, 68, 54];

const RESTORED_IMAGE_NAME: &str = "image.jpg";
const RESTORED_IMAGE_EDGE: u32 = 1;

/// 会话内可复用的规范化图片；快照只保存不透明的存在标记。
#[derive(Clone, Eq, PartialEq)]
pub struct ImageAttachment {
    name: String,
    width: u32,
    height: u32,
    bytes: Option<Arc<[u8]>>,
}

impl Serialize for ImageAttachment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for ImageAttachment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if !bool::deserialize(deserializer)? {
            return Err(de::Error::custom("图片附件存在标记必须为 true"));
        }
        Ok(Self {
            name: RESTORED_IMAGE_NAME.to_owned(),
            width: RESTORED_IMAGE_EDGE,
            height: RESTORED_IMAGE_EDGE,
            bytes: None,
        })
    }
}

impl fmt::Debug for ImageAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageAttachment")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("available", &self.bytes.is_some())
            .finish()
    }
}

impl ImageAttachment {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.as_ref().map_or(0, |bytes| bytes.len())
    }

    pub fn has_safe_metadata(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= 128
            && self.width > 0
            && self.height > 0
            && self.width <= MAX_OUTPUT_EDGE
            && self.height <= MAX_OUTPUT_EDGE
            && self.byte_len() <= MAX_OUTPUT_BYTES
    }
}

/// 校验并规范化宿主已经读取的一张 JPEG、PNG 或 WebP 图片。
pub fn prepare_image(bytes: &[u8], name: String) -> Result<ImageAttachment, ImageInputError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(ImageInputError::SourceTooLarge);
    }
    let format = image::guess_format(bytes).map_err(|_| ImageInputError::UnsupportedFormat)?;
    if !matches!(
        format,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
    ) {
        return Err(ImageInputError::UnsupportedFormat);
    }
    let dimensions = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| ImageInputError::Decode)?;
    validate_dimensions(dimensions)?;
    let image =
        image::load_from_memory_with_format(bytes, format).map_err(|_| ImageInputError::Decode)?;
    prepare_dynamic_image(image, name)
}

/// 规范化宿主已经解码的图片，供原生截图实现避免重复编码源格式。
pub fn prepare_dynamic_image(
    image: DynamicImage,
    name: String,
) -> Result<ImageAttachment, ImageInputError> {
    validate_dimensions(image.dimensions())?;
    let image = if image.width().max(image.height()) > MAX_OUTPUT_EDGE {
        image.resize(MAX_OUTPUT_EDGE, MAX_OUTPUT_EDGE, FilterType::Triangle)
    } else {
        image
    };
    let rgb = flatten_alpha(image);
    let mut encoded = None;
    for quality in JPEG_QUALITIES {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )
            .map_err(|_| ImageInputError::Encode)?;
        if bytes.len() <= MAX_OUTPUT_BYTES {
            encoded = Some(bytes);
            break;
        }
    }
    let bytes = encoded.ok_or(ImageInputError::OutputTooLarge)?;
    Ok(ImageAttachment {
        name: normalized_name(Path::new(&name)),
        width: rgb.width(),
        height: rgb.height(),
        bytes: Some(Arc::from(bytes)),
    })
}

fn validate_dimensions((width, height): (u32, u32)) -> Result<(), ImageInputError> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_SOURCE_EDGE
        || height > MAX_SOURCE_EDGE
        || pixels > MAX_SOURCE_PIXELS
    {
        return Err(ImageInputError::DimensionsTooLarge);
    }
    Ok(())
}

fn flatten_alpha(image: DynamicImage) -> RgbImage {
    let rgba = image.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (source, target) in rgba.pixels().zip(rgb.pixels_mut()) {
        let alpha = u16::from(source[3]);
        let inverse = 255_u16.saturating_sub(alpha);
        *target = Rgb([
            blend_channel(source[0], alpha, inverse),
            blend_channel(source[1], alpha, inverse),
            blend_channel(source[2], alpha, inverse),
        ]);
    }
    rgb
}

fn blend_channel(channel: u8, alpha: u16, inverse: u16) -> u8 {
    ((u16::from(channel) * alpha + 255 * inverse + 127) / 255) as u8
}

fn normalized_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("image");
    let mut bounded_stem = String::new();
    for character in stem.chars() {
        if bounded_stem.len().saturating_add(character.len_utf8()) > 96 {
            break;
        }
        bounded_stem.push(character);
    }
    let stem = if bounded_stem.is_empty() {
        "image"
    } else {
        &bounded_stem
    };
    format!("{stem}.jpg")
}

/// 对外部图片和系统截屏失败进行脱敏分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageInputError {
    Unreadable,
    SourceTooLarge,
    UnsupportedFormat,
    DimensionsTooLarge,
    Decode,
    Encode,
    OutputTooLarge,
    ScreenCapture,
}

impl ImageInputError {
    /// 返回适合直接展示给用户、且绑定到单次操作语言的脱敏说明。
    pub fn localized_message(self, language: AppLanguage) -> String {
        match self {
            Self::Unreadable => t!("chat.error.image_unreadable", locale = language.id()),
            Self::SourceTooLarge => t!("chat.error.image_file_too_large", locale = language.id()),
            Self::UnsupportedFormat => t!("chat.error.image_format", locale = language.id()),
            Self::DimensionsTooLarge => {
                t!("chat.error.image_dimensions", locale = language.id())
            }
            Self::Decode => t!("chat.error.image_decode", locale = language.id()),
            Self::Encode | Self::OutputTooLarge => {
                t!("chat.error.image_prepare", locale = language.id())
            }
            Self::ScreenCapture => t!("chat.error.screen_capture", locale = language.id()),
        }
        .to_string()
    }
}

impl fmt::Display for ImageInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Unreadable => "image source is unreadable",
            Self::SourceTooLarge => "image source exceeds the byte limit",
            Self::UnsupportedFormat => "image format is unsupported",
            Self::DimensionsTooLarge => "image dimensions exceed the limit",
            Self::Decode => "image decoding failed",
            Self::Encode => "image encoding failed",
            Self::OutputTooLarge => "encoded image exceeds the byte limit",
            Self::ScreenCapture => "screen capture failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ImageInputError {}
