use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::model::live2d::resources::validate_model_resources;

use std::time::{SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-resource-validation-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试资源目录应当可以创建");
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

fn write_manifest(directory: &Path, moc: &str) -> PathBuf {
    let path = directory.join("model.model3.json");
    fs::write(
        &path,
        format!(r#"{{"Version":3,"FileReferences":{{"Moc":"{moc}","Textures":[]}}}}"#),
    )
    .expect("测试模型清单应当可以创建");
    path
}

/// 写入声明指定尺寸的合法 PNG 头；纹理预检只读文件头，无需真实像素。
fn write_texture(path: &Path, width: u32, height: u32) {
    let mut bytes = {
        let mut buffer = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut buffer, image::ImageFormat::Png)
            .expect("测试纹理应当可以编码");
        buffer.into_inner()
    };
    // PNG 布局固定：8 字节签名 + 4 字节长度后紧跟 IHDR 块类型、13 字节数据与 CRC。
    const CHUNK: std::ops::Range<usize> = 12..29;
    bytes[16..20].copy_from_slice(&width.to_be_bytes());
    bytes[20..24].copy_from_slice(&height.to_be_bytes());
    let checksum = png_crc32(&bytes[CHUNK]).to_be_bytes();
    bytes[CHUNK.end..CHUNK.end + 4].copy_from_slice(&checksum);
    fs::write(path, bytes).expect("测试纹理应当可以写入");
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

fn write_textured_manifest(directory: &Path, textures: &[&str]) -> PathBuf {
    let path = directory.join("model.model3.json");
    let textures = textures
        .iter()
        .map(|texture| format!("\"{texture}\""))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        &path,
        format!(
            r#"{{"Version":3,"FileReferences":{{"Moc":"model.moc3","Textures":[{textures}]}}}}"#
        ),
    )
    .expect("测试模型清单应当可以创建");
    path
}

#[test]
fn accepts_regular_resources_inside_manifest_directory() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    let manifest = write_manifest(directory.path(), "model.moc3");

    let _resolver = validate_model_resources(&manifest).expect("目录内普通资源应当通过预检");
}

#[test]
fn rejects_parent_directory_references() {
    let directory = TestDirectory::new();
    let runtime = directory.path().join("runtime");
    fs::create_dir(&runtime).expect("测试运行时目录应当可以创建");
    fs::write(directory.path().join("outside.moc3"), []).expect("测试越界资源应当可以创建");
    let manifest = write_manifest(&runtime, "../outside.moc3");

    let error = validate_model_resources(&manifest)
        .expect_err("父目录引用必须被拒绝")
        .to_string();
    assert!(error.contains("相对路径"));
}

#[cfg(unix)]
#[test]
fn rejects_symbolic_links_outside_manifest_directory() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let runtime = directory.path().join("runtime");
    fs::create_dir(&runtime).expect("测试运行时目录应当可以创建");
    let outside = directory.path().join("outside.moc3");
    fs::write(&outside, []).expect("测试越界资源应当可以创建");
    symlink(&outside, runtime.join("linked.moc3")).expect("测试符号链接应当可以创建");
    let manifest = write_manifest(&runtime, "linked.moc3");

    let error = validate_model_resources(&manifest)
        .expect_err("指向目录外的符号链接必须被拒绝")
        .to_string();
    assert!(error.contains("越出模型目录"));
}

#[test]
fn optional_resources_do_not_fail_required_preflight() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    let manifest = directory.path().join("model.model3.json");
    fs::write(
        &manifest,
        r#"{
                "Version": 3,
                "FileReferences": {
                    "Moc": "model.moc3",
                    "Textures": [],
                    "DisplayInfo": "../outside.cdi3.json",
                    "Motions": {"Tap": [{"File": "missing.motion3.json"}]},
                    "Expressions": [{"Name": "Broken", "File": "missing.exp3.json"}]
                }
            }"#,
    )
    .expect("测试模型清单应当可以创建");

    let _resolver =
        validate_model_resources(&manifest).expect("可选资源损坏不应阻止主体必需资源预检");
}

#[test]
fn missing_or_non_regular_manifests_are_reported_before_parsing() {
    let directory = TestDirectory::new();

    let missing = validate_model_resources(&directory.path().join("absent.model3.json"))
        .expect_err("缺失清单必须被拒绝")
        .to_string();
    assert!(missing.contains("无法读取模型清单"));

    let as_directory = directory.path().join("model.model3.json");
    fs::create_dir(&as_directory).expect("测试目录应当可以创建");
    let error = validate_model_resources(&as_directory)
        .expect_err("目录形式的清单必须被拒绝")
        .to_string();
    assert!(error.contains("不是普通文件"));
}

#[test]
fn oversized_manifests_are_rejected_without_reading_them() {
    let directory = TestDirectory::new();
    let manifest = directory.path().join("model.model3.json");
    let file = fs::File::create(&manifest).expect("测试大清单应当可以创建");
    // 稀疏文件只声明长度，用于验证上限在读取内容之前生效。
    file.set_len(1024 * 1024 + 1)
        .expect("测试大清单应当可以设置长度");
    drop(file);

    let error = validate_model_resources(&manifest)
        .expect_err("超限清单必须被拒绝")
        .to_string();
    assert!(error.contains("超过上限"));
}

#[test]
fn manifests_that_are_not_valid_utf8_json_are_rejected() {
    let directory = TestDirectory::new();
    let invalid_utf8 = directory.path().join("model.model3.json");
    fs::write(&invalid_utf8, [0xFF, 0xFE, 0xFD]).expect("测试非 UTF-8 清单应当可以创建");
    assert!(
        validate_model_resources(&invalid_utf8)
            .expect_err("非 UTF-8 清单必须被拒绝")
            .to_string()
            .contains("UTF-8")
    );

    fs::write(&invalid_utf8, "{ not json ").expect("测试损坏清单应当可以创建");
    assert!(
        validate_model_resources(&invalid_utf8)
            .expect_err("损坏 JSON 清单必须被拒绝")
            .to_string()
            .contains("无法解析模型清单")
    );
}

#[test]
fn manifests_referencing_a_missing_moc_are_rejected() {
    let directory = TestDirectory::new();
    let manifest = write_manifest(directory.path(), "absent.moc3");

    let error = validate_model_resources(&manifest)
        .expect_err("缺失 MOC 必须被拒绝")
        .to_string();
    assert!(error.starts_with("MOC 引用 absent.moc3 无效"));
}

#[test]
fn texture_count_is_capped_before_any_texture_is_decoded() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    let textures = (0..17).map(|_| "texture.png").collect::<Vec<_>>();
    let manifest = write_textured_manifest(directory.path(), &textures);

    let error = validate_model_resources(&manifest)
        .expect_err("超量纹理必须被拒绝")
        .to_string();
    assert!(error.contains("纹理数量"));
}

#[test]
fn oversized_texture_dimensions_are_rejected() {
    for (width, height) in [(8_193_u32, 1_u32), (1, 8_193), (8_193, 8_193)] {
        let directory = TestDirectory::new();
        fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
        write_texture(&directory.path().join("texture.png"), width, height);
        let manifest = write_textured_manifest(directory.path(), &["texture.png"]);

        let error = validate_model_resources(&manifest)
            .expect_err("异常纹理尺寸必须被拒绝")
            .to_string();
        assert!(
            error.contains("单边上限"),
            "{width}x{height} 应当因单边上限被拒绝，实际：{error}"
        );
    }
}

#[test]
fn total_texture_pixels_are_capped_across_all_declared_textures() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    // 单张纹理都在单边上限内，但累计像素数超过整体预算。
    write_texture(&directory.path().join("texture-0.png"), 8_192, 4_096);
    write_texture(&directory.path().join("texture-1.png"), 8_192, 4_097);
    let manifest = write_textured_manifest(directory.path(), &["texture-0.png", "texture-1.png"]);

    let error = validate_model_resources(&manifest)
        .expect_err("纹理总像素超限必须被拒绝")
        .to_string();
    assert!(error.contains("纹理总像素数量"));
}

#[test]
fn textures_that_cannot_be_decoded_report_a_dimension_failure() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    fs::write(directory.path().join("texture.png"), b"not an image")
        .expect("测试损坏纹理应当可以创建");
    let manifest = write_textured_manifest(directory.path(), &["texture.png"]);

    let error = validate_model_resources(&manifest)
        .expect_err("无法解码的纹理必须被拒绝")
        .to_string();
    assert!(error.contains("无法读取纹理尺寸"));
}

#[test]
fn physics_and_pose_references_are_validated_with_the_model_body() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    fs::write(directory.path().join("model.physics3.json"), "{}")
        .expect("测试 Physics 应当可以创建");
    let manifest = directory.path().join("model.model3.json");
    let write = |physics: &str, pose: &str| {
        fs::write(
            &manifest,
            format!(
                r#"{{"Version":3,"FileReferences":{{"Moc":"model.moc3","Textures":[],"Physics":"{physics}","Pose":"{pose}"}}}}"#
            ),
        )
        .expect("测试模型清单应当可以创建");
    };

    write("model.physics3.json", "../outside.pose3.json");
    assert!(
        validate_model_resources(&manifest)
            .expect_err("越界 Pose 必须被拒绝")
            .to_string()
            .starts_with("Pose 引用")
    );

    write("../outside.physics3.json", "model.pose3.json");
    assert!(
        validate_model_resources(&manifest)
            .expect_err("越界 Physics 必须被拒绝")
            .to_string()
            .starts_with("Physics 引用")
    );

    fs::write(directory.path().join("model.pose3.json"), "{}").expect("测试 Pose 应当可以创建");
    write("model.physics3.json", "model.pose3.json");
    let _resolver =
        validate_model_resources(&manifest).expect("目录内的 Physics 与 Pose 应当通过预检");
}

/// 真实模型体积、纹理数量与 MOC 结构无法用最小 fixture 覆盖；仓库不分发 Live2D 模型。
/// 请把自备模型放入 `models/` 后手动运行，验证预检不会误拒合法模型。
#[test]
#[ignore = "需要自备完整 Live2D 模型验证预检不会误拒，请在本地放置模型后手动运行"]
fn real_model_directories_pass_resource_preflight() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models/hiyori_free/runtime/hiyori_free_t08.model3.json");

    let _resolver = validate_model_resources(&manifest).expect("自备的完整模型应当通过预检");
}
