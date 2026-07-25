//! 校验并规范化对话图片，同时封装 Agent 截屏工具使用的跨平台捕获能力。

use std::{
    fmt, fs,
    io::{Cursor, Read as _},
    path::Path,
    sync::Arc,
};

use image::{
    DynamicImage, ExtendedColorType, GenericImageView as _, ImageFormat, Rgb, RgbImage,
    codecs::jpeg::JpegEncoder, imageops::FilterType,
};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
static SCREEN_CAPTURE_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

const MAX_USER_SOURCE_BYTES: u64 = 20 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_CAPTURE_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOURCE_EDGE: u32 = 16_384;
const MAX_SOURCE_PIXELS: u64 = 40_000_000;
const MAX_OUTPUT_EDGE: u32 = 2_048;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const JPEG_QUALITIES: [u8; 3] = [82, 68, 54];

/// 会话内可复用的规范化图片；快照只保存安全元数据，不保存像素内容。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ImageAttachment {
    name: String,
    width: u32,
    height: u32,
    #[serde(skip, default)]
    bytes: Option<Arc<[u8]>>,
}

impl fmt::Debug for ImageAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageAttachment")
            .field("name", &self.name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("available", &self.bytes.is_some())
            .finish()
    }
}

impl ImageAttachment {
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn width(&self) -> u32 {
        self.width
    }

    pub(super) fn height(&self) -> u32 {
        self.height
    }

    pub(super) fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }

    pub(super) fn byte_len(&self) -> usize {
        self.bytes.as_ref().map_or(0, |bytes| bytes.len())
    }

    pub(super) fn has_safe_metadata(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= 128
            && self.width > 0
            && self.height > 0
            && self.width <= MAX_OUTPUT_EDGE
            && self.height <= MAX_OUTPUT_EDGE
            && self.byte_len() <= MAX_OUTPUT_BYTES
    }
}

/// 从用户明确选择的文件读取并规范化一张图片。
pub(super) fn load_image(path: &Path) -> Result<ImageAttachment, ImageInputError> {
    load_image_with_limit(path, MAX_USER_SOURCE_BYTES, normalized_name(path))
}

fn load_image_with_limit(
    path: &Path,
    max_source_bytes: u64,
    name: String,
) -> Result<ImageAttachment, ImageInputError> {
    let bytes = read_image_source(path, max_source_bytes)?;
    prepare_image_source(&bytes, name)
}

fn read_image_source(path: &Path, max_source_bytes: u64) -> Result<Vec<u8>, ImageInputError> {
    let mut file = fs::File::open(path).map_err(|_| ImageInputError::Unreadable)?;
    read_image_source_file(&mut file, max_source_bytes)
}

fn read_image_source_file(
    file: &mut fs::File,
    max_source_bytes: u64,
) -> Result<Vec<u8>, ImageInputError> {
    let metadata = file.metadata().map_err(|_| ImageInputError::Unreadable)?;
    if !metadata.is_file() {
        return Err(ImageInputError::Unreadable);
    }
    if metadata.len() > max_source_bytes {
        return Err(ImageInputError::SourceTooLarge);
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_source_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ImageInputError::Unreadable)?;
    if bytes.len() as u64 > max_source_bytes {
        return Err(ImageInputError::SourceTooLarge);
    }
    Ok(bytes)
}

fn prepare_image_source(bytes: &[u8], name: String) -> Result<ImageAttachment, ImageInputError> {
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

/// 抓取用户屏幕，并返回适合多模态请求的有界图片。
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(super) async fn capture_primary_screen() -> Result<ImageAttachment, ImageInputError> {
    let permit = SCREEN_CAPTURE_GATE
        .try_acquire()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    tokio::task::spawn_blocking(move || {
        // 阻塞截图 API 无法中途取消；把 permit 留在线程内，确保迟到任务至多一个。
        let _permit = permit;
        capture_primary_screen_blocking()
    })
    .await
    .map_err(|_| ImageInputError::ScreenCapture)?
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn capture_primary_screen_blocking() -> Result<ImageAttachment, ImageInputError> {
    let monitors = xcap::Monitor::all().map_err(|_| ImageInputError::ScreenCapture)?;
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or(ImageInputError::ScreenCapture)?;
    let width = monitor
        .width()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    let height = monitor
        .height()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    validate_dimensions((width, height))?;
    let image = monitor
        .capture_image()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    prepare_dynamic_image(DynamicImage::ImageRgba8(image), "screenshot.jpg".to_owned())
}

/// Linux 使用桌面门户，由合成器负责权限确认以及 Wayland/X11 兼容。
#[cfg(target_os = "linux")]
pub(super) async fn capture_primary_screen() -> Result<ImageAttachment, ImageInputError> {
    let _capture_permit = SCREEN_CAPTURE_GATE
        .try_acquire()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    let request = ashpd::desktop::screenshot::Screenshot::request()
        .interactive(false)
        .modal(true)
        .send()
        .await
        .map_err(|_| ImageInputError::ScreenCapture)?;
    let response = request
        .response()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    let url =
        url::Url::parse(response.uri().as_str()).map_err(|_| ImageInputError::ScreenCapture)?;
    let path = url
        .to_file_path()
        .map_err(|()| ImageInputError::ScreenCapture)?;
    tokio::task::spawn_blocking(move || load_and_remove_portal_capture(&path))
        .await
        .map_err(|_| ImageInputError::ScreenCapture)?
}

#[cfg(target_os = "linux")]
fn load_and_remove_portal_capture(path: &Path) -> Result<ImageAttachment, ImageInputError> {
    let mut file = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(_) => {
            let _ = fs::remove_file(path);
            return Err(ImageInputError::ScreenCapture);
        }
    };
    let source = read_image_source_file(&mut file, MAX_CAPTURE_SOURCE_BYTES);
    // 无论路径是否被替换，都先清空已经打开的原始截图句柄，再尝试移除目录项。
    let truncated = file.set_len(0).and_then(|()| file.sync_all()).is_ok();
    let removed = fs::remove_file(path).is_ok();
    let cleaned = truncated || removed;
    if !cleaned {
        return Err(ImageInputError::ScreenCapture);
    }
    let bytes = source?;
    prepare_image_source(&bytes, "screenshot.jpg".to_owned())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub(super) async fn capture_primary_screen() -> Result<ImageAttachment, ImageInputError> {
    Err(ImageInputError::ScreenCapture)
}

pub(super) fn prepare_dynamic_image(
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
pub(super) enum ImageInputError {
    Unreadable,
    SourceTooLarge,
    UnsupportedFormat,
    DimensionsTooLarge,
    Decode,
    Encode,
    OutputTooLarge,
    ScreenCapture,
}

impl fmt::Display for ImageInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Unreadable => t!("chat.error.image_unreadable"),
            Self::SourceTooLarge => t!("chat.error.image_file_too_large"),
            Self::UnsupportedFormat => t!("chat.error.image_format"),
            Self::DimensionsTooLarge => t!("chat.error.image_dimensions"),
            Self::Decode => t!("chat.error.image_decode"),
            Self::Encode | Self::OutputTooLarge => t!("chat.error.image_prepare"),
            Self::ScreenCapture => t!("chat.error.screen_capture"),
        };
        formatter.write_str(&message)
    }
}

impl std::error::Error for ImageInputError {}
