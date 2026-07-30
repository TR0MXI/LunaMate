//! 负责 Agent 用户图片文件读取与跨平台屏幕捕获，图片规范化留给 Agent 核心。

use std::{fs, io::Read as _, path::Path};

use lunamate_agent::media::{ImageAttachment, ImageInputError};

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
static SCREEN_CAPTURE_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

const MAX_USER_SOURCE_BYTES: u64 = 20 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_CAPTURE_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
/// 门户无响应时释放捕获闸门的上限，需大于工具侧的截图超时以免抢先失败。
#[cfg(target_os = "linux")]
const PORTAL_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// 从用户明确选择的常规文件读取并规范化一张 Agent 图片。
pub(crate) fn load_agent_image(path: &Path) -> Result<ImageAttachment, ImageInputError> {
    let bytes = read_image_source(path, MAX_USER_SOURCE_BYTES)?;
    // 本地文件名可能包含身份或文档内容，不得进入 Provider 请求或会话状态。
    lunamate_agent::media::prepare_image(&bytes, "image.jpg".to_owned())
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

/// 抓取用户主屏幕，并返回适合多模态请求的有界图片。
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) async fn capture_primary_screen() -> Result<ImageAttachment, ImageInputError> {
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
    let image = monitor
        .capture_image()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    lunamate_agent::media::prepare_dynamic_image(
        image::DynamicImage::ImageRgba8(image),
        "screenshot.jpg".to_owned(),
    )
}

/// Linux 使用桌面门户，由合成器负责权限确认以及 Wayland/X11 兼容。
#[cfg(target_os = "linux")]
pub(crate) async fn capture_primary_screen() -> Result<ImageAttachment, ImageInputError> {
    let _capture_permit = SCREEN_CAPTURE_GATE
        .try_acquire()
        .map_err(|_| ImageInputError::ScreenCapture)?;
    // 调用方不会中止本任务（需保留临时文件清理），因此在此约束等待，避免 permit 永久占用。
    let request = tokio::time::timeout(
        PORTAL_RESPONSE_TIMEOUT,
        ashpd::desktop::screenshot::Screenshot::request()
            .interactive(false)
            .modal(true)
            .send(),
    )
    .await
    .map_err(|_| ImageInputError::ScreenCapture)?
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
        log::error!(
            "截图门户临时文件清理失败：truncated={truncated}, removed={removed}, source_read={}",
            source.is_ok()
        );
        return Err(ImageInputError::ScreenCapture);
    }
    log::debug!(
        "截图门户临时文件已清理：truncated={truncated}, removed={removed}, source_read={}",
        source.is_ok()
    );
    let bytes = source?;
    lunamate_agent::media::prepare_image(&bytes, "screenshot.jpg".to_owned())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub(crate) async fn capture_primary_screen() -> Result<ImageAttachment, ImageInputError> {
    Err(ImageInputError::ScreenCapture)
}
