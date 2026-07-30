//! 验证宿主图片文件边界，并保留真实桌面截图的手动验收入口。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use image::{DynamicImage, ImageFormat};
use lunamate_agent::media::ImageInputError;

use super::super::{capture_primary_screen, load_agent_image};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-platform-screenshot-{}-{nonce}",
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

#[test]
fn selected_image_file_is_bounded_and_normalized() {
    let directory = TestDirectory::new();
    let source = directory.path().join("头像.png");
    DynamicImage::new_rgb8(40, 20)
        .save_with_format(&source, ImageFormat::Png)
        .expect("测试 PNG 应当可以写入");

    let attachment = load_agent_image(&source).expect("有效图片文件应当可以加载");

    assert_eq!(attachment.name(), "image.jpg");
    assert_eq!((attachment.width(), attachment.height()), (40, 20));
    assert!(attachment.has_safe_metadata());
}

#[test]
fn missing_directories_and_oversized_files_are_rejected_before_decode() {
    let directory = TestDirectory::new();
    assert_eq!(
        load_agent_image(&directory.path().join("missing.png")),
        Err(ImageInputError::Unreadable)
    );
    assert_eq!(
        load_agent_image(directory.path()),
        Err(ImageInputError::Unreadable)
    );

    let oversized = directory.path().join("oversized.png");
    let file = fs::File::create(&oversized).expect("测试文件应当可以创建");
    file.set_len(20 * 1024 * 1024 + 1)
        .expect("测试文件应当可以设置长度");
    drop(file);
    assert_eq!(
        load_agent_image(&oversized),
        Err(ImageInputError::SourceTooLarge)
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
        .block_on(capture_primary_screen())
        .expect("授权后的截屏应当成功");

    assert!(attachment.has_safe_metadata());
    assert_eq!(attachment.name(), "screenshot.jpg");
    assert!(attachment.byte_len() <= 4 * 1024 * 1024);
}
