use std::{
    fs,
    path::{Path, PathBuf},
};

use mocari::load_model_runtime_from_assets;

use crate::model::live2d::resources::{
    read_bounded_file_for_test, snapshot_model_resources_with_open_hook_for_test,
    validate_model_resources,
};

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

/// 写入声明指定尺寸的 PNG；除 1x1 外只用于在完整解码前触发尺寸预算。
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

fn empty_physics_source() -> &'static str {
    r#"{
        "Version": 3,
        "Meta": {
            "PhysicsSettingCount": 0,
            "TotalInputCount": 0,
            "TotalOutputCount": 0,
            "VertexCount": 0,
            "Fps": 60,
            "EffectiveForces": {
                "Gravity": {"X": 0, "Y": -1},
                "Wind": {"X": 0, "Y": 0}
            },
            "PhysicsDictionary": []
        },
        "PhysicsSettings": []
    }"#
}

fn empty_pose_source() -> &'static str {
    r#"{"Type":"Live2D Pose","Groups":[]}"#
}

fn minimal_empty_moc() -> Vec<u8> {
    const OFFSET_TABLE_START: usize = 0x40;
    const OFFSET_COUNT: usize = 160;
    const COUNT_INFO_WORDS: usize = 35;
    const U32_SIZE: usize = 4;
    const COUNT_INFO_OFFSET: usize = OFFSET_TABLE_START + OFFSET_COUNT * U32_SIZE;
    const CANVAS_INFO_OFFSET: usize = COUNT_INFO_OFFSET + COUNT_INFO_WORDS * U32_SIZE;
    const CANVAS_INFO_SIZE: usize = 64;

    let mut bytes = vec![0_u8; CANVAS_INFO_OFFSET + CANVAS_INFO_SIZE];
    bytes[0..4].copy_from_slice(b"MOC3");
    bytes[4] = 6;
    bytes[5] = 0;
    bytes[OFFSET_TABLE_START..OFFSET_TABLE_START + U32_SIZE]
        .copy_from_slice(&(COUNT_INFO_OFFSET as u32).to_le_bytes());
    bytes[OFFSET_TABLE_START + U32_SIZE..OFFSET_TABLE_START + U32_SIZE * 2]
        .copy_from_slice(&(CANVAS_INFO_OFFSET as u32).to_le_bytes());
    bytes[CANVAS_INFO_OFFSET..CANVAS_INFO_OFFSET + U32_SIZE]
        .copy_from_slice(&1.0_f32.to_le_bytes());
    bytes[CANVAS_INFO_OFFSET + U32_SIZE * 3..CANVAS_INFO_OFFSET + U32_SIZE * 4]
        .copy_from_slice(&1.0_f32.to_le_bytes());
    bytes[CANVAS_INFO_OFFSET + U32_SIZE * 4..CANVAS_INFO_OFFSET + U32_SIZE * 5]
        .copy_from_slice(&1.0_f32.to_le_bytes());
    bytes
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
    assert!(error.contains("链接"));
}

#[cfg(unix)]
#[test]
fn manifest_replaced_by_external_symlink_before_open_is_rejected() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    fs::write(model_dir.join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    let manifest = write_manifest(&model_dir, "model.moc3");
    let outside = directory.path().join("outside-manifest.model3.json");
    fs::write(&outside, b"outside manifest").expect("目录外替代清单应当可以创建");
    let canonical_manifest = fs::canonicalize(&manifest).expect("清单规范路径应当可以取得");
    let mut hook_called = false;

    let error = snapshot_model_resources_with_open_hook_for_test(&manifest, |canonical_path| {
        assert_eq!(canonical_path, canonical_manifest);
        fs::remove_file(canonical_path).expect("已校验清单应当可以移除");
        symlink(&outside, canonical_path).expect("目录外清单符号链接应当可以创建");
        hook_called = true;
    })
    .expect_err("metadata 后、open 前替换的清单必须被拒绝")
    .to_string();

    assert!(hook_called, "测试替换必须发生在清单最终打开之前");
    assert!(error.contains("无法读取模型清单"));
    assert!(error.contains("打开期间发生变化"));
}

#[cfg(unix)]
#[test]
fn required_resource_replaced_by_external_symlink_before_open_is_rejected() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    let resource = model_dir.join("model.moc3");
    let outside = directory.path().join("outside-model.moc3");
    fs::write(&resource, b"inside moc").expect("模型内 MOC 应当可以创建");
    fs::write(&outside, b"outside moc").expect("目录外替代 MOC 应当可以创建");
    let manifest = write_manifest(&model_dir, "model.moc3");
    let canonical_resource = fs::canonicalize(&resource).expect("MOC 规范路径应当可以取得");
    let mut hook_called = false;

    let error = snapshot_model_resources_with_open_hook_for_test(&manifest, |canonical_path| {
        if canonical_path == canonical_resource {
            fs::remove_file(canonical_path).expect("已校验 MOC 应当可以移除");
            symlink(&outside, canonical_path).expect("目录外 MOC 符号链接应当可以创建");
            hook_called = true;
        }
    })
    .expect_err("metadata 后、open 前替换的必需资源必须被拒绝")
    .to_string();

    assert!(hook_called, "测试替换必须发生在 MOC 最终打开之前");
    assert!(error.starts_with("MOC 引用 model.moc3 无效"));
    assert!(error.contains("打开期间发生变化"));
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
fn growth_after_opened_handle_metadata_is_rejected_by_actual_read_size() {
    let directory = TestDirectory::new();
    let path = directory.path().join("growing.bin");
    fs::write(&path, b"ok").expect("测试增长文件应当可以创建");
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("测试增长文件应当可以再次打开");
    let mut checkpoints = 0;

    let error = read_bounded_file_for_test(&path, 2, || {
        checkpoints += 1;
        if checkpoints == 2 {
            writer
                .set_len(3)
                .expect("句柄元数据复核后应当可以模拟文件增长");
        }
        false
    })
    .expect_err("句柄元数据检查后增长到上限外的文件必须被拒绝")
    .to_string();

    assert!(error.contains("实际读取大小 3 字节超过上限 2"));
}

#[test]
fn bounded_required_resource_read_honors_cancellation_between_chunks() {
    let directory = TestDirectory::new();
    let path = directory.path().join("cancelled.bin");
    fs::write(&path, vec![0_u8; 128 * 1024]).expect("测试取消文件应当可以创建");
    let mut checkpoints = 0;

    let error = read_bounded_file_for_test(&path, 128 * 1024, || {
        checkpoints += 1;
        checkpoints == 4
    })
    .expect_err("分块读取期间的取消必须停止必需资源快照")
    .to_string();

    assert_eq!(error, "模型资源读取已取消");
}

#[test]
fn oversized_moc_is_rejected_before_runtime_construction() {
    let directory = TestDirectory::new();
    let moc =
        fs::File::create(directory.path().join("model.moc3")).expect("测试大 MOC 应当可以创建");
    moc.set_len(128 * 1024 * 1024 + 1)
        .expect("测试大 MOC 应当可以设置长度");
    drop(moc);
    let manifest = write_manifest(directory.path(), "model.moc3");

    let error = validate_model_resources(&manifest)
        .expect_err("超过 128 MiB 的 MOC 必须被拒绝")
        .to_string();

    assert!(error.starts_with("MOC 引用"));
    assert!(error.contains("上限"));
}

#[cfg(unix)]
#[test]
fn fifo_required_resource_is_rejected_without_opening_it_for_reading() {
    let directory = TestDirectory::new();
    let fifo = directory.path().join("model.moc3");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("测试环境应当提供 mkfifo");
    assert!(status.success(), "测试 FIFO 应当可以创建");
    let manifest = write_manifest(directory.path(), "model.moc3");

    let error = validate_model_resources(&manifest)
        .expect_err("FIFO 不得作为 MOC 进入有界读取")
        .to_string();

    assert!(error.contains("不是普通文件"));
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
fn oversized_texture_file_is_rejected_before_decode() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    let texture =
        fs::File::create(directory.path().join("texture.png")).expect("测试大纹理文件应当可以创建");
    texture
        .set_len(64 * 1024 * 1024 + 1)
        .expect("测试大纹理文件应当可以设置长度");
    drop(texture);
    let manifest = write_textured_manifest(directory.path(), &["texture.png"]);

    let error = validate_model_resources(&manifest)
        .expect_err("超过 64 MiB 的纹理文件必须被拒绝")
        .to_string();

    assert!(error.starts_with("纹理 0 引用"));
    assert!(error.contains("上限"));
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
    // 第一张是可完整解码纹理；第二张单独处于预算边界，但累计后多出一个像素。
    write_texture(&directory.path().join("texture-0.png"), 1, 1);
    write_texture(&directory.path().join("texture-1.png"), 8_192, 8_192);
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
fn oversized_physics_and_pose_files_are_rejected_at_eight_mib() {
    for (field, file_name) in [
        ("Physics", "model.physics3.json"),
        ("Pose", "model.pose3.json"),
    ] {
        let directory = TestDirectory::new();
        fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
        let sidecar =
            fs::File::create(directory.path().join(file_name)).expect("测试大辅助文件应当可以创建");
        sidecar
            .set_len(8 * 1024 * 1024 + 1)
            .expect("测试大辅助文件应当可以设置长度");
        drop(sidecar);
        let manifest = directory.path().join("model.model3.json");
        fs::write(
            &manifest,
            format!(
                r#"{{"Version":3,"FileReferences":{{"Moc":"model.moc3","Textures":[],"{field}":"{file_name}"}}}}"#
            ),
        )
        .expect("测试辅助资源清单应当可以创建");

        let error = validate_model_resources(&manifest)
            .expect_err("超过 8 MiB 的 Physics 或 Pose 必须被拒绝")
            .to_string();

        assert!(error.starts_with(&format!("{field} 引用")));
        assert!(error.contains("上限"));
    }
}

#[test]
fn physics_and_pose_references_are_validated_with_the_model_body() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    fs::write(
        directory.path().join("model.physics3.json"),
        empty_physics_source(),
    )
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

    fs::write(
        directory.path().join("model.pose3.json"),
        empty_pose_source(),
    )
    .expect("测试 Pose 应当可以创建");
    write("model.physics3.json", "model.pose3.json");
    let _resolver =
        validate_model_resources(&manifest).expect("目录内的 Physics 与 Pose 应当通过预检");
}

#[test]
fn runtime_construction_uses_snapshot_after_required_paths_are_replaced() {
    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    let archived_dir = directory.path().join("archived");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    fs::write(model_dir.join("model.moc3"), minimal_empty_moc()).expect("最小 MOC 应当可以创建");
    write_texture(&model_dir.join("texture.png"), 1, 1);
    fs::write(
        model_dir.join("model.physics3.json"),
        empty_physics_source(),
    )
    .expect("测试 Physics 应当可以创建");
    fs::write(model_dir.join("model.pose3.json"), empty_pose_source())
        .expect("测试 Pose 应当可以创建");
    let manifest = model_dir.join("model.model3.json");
    fs::write(
        &manifest,
        r#"{
            "Version": 3,
            "FileReferences": {
                "Moc": "model.moc3",
                "Textures": ["texture.png"],
                "Physics": "model.physics3.json",
                "Pose": "model.pose3.json"
            }
        }"#,
    )
    .expect("完整快照清单应当可以创建");

    let snapshot = validate_model_resources(&manifest).expect("完整必需资源应当可以冻结");
    fs::rename(&model_dir, &archived_dir).expect("已验证模型目录应当可以移走");
    fs::create_dir(&model_dir).expect("原路径应当可以放回恶意替代目录");
    for file_name in [
        "model.model3.json",
        "model.moc3",
        "texture.png",
        "model.physics3.json",
        "model.pose3.json",
    ] {
        fs::write(model_dir.join(file_name), b"replaced after validation")
            .expect("恶意同名替代文件应当可以创建");
    }

    let (_resolver, assets) = snapshot.into_parts();
    let runtime = load_model_runtime_from_assets(assets)
        .expect("Mocari 运行时构造不得重新打开已替换的必需资源路径");

    assert_eq!(runtime.runtime().model().moc(), "model.moc3");
    assert_eq!(
        runtime.runtime().model().physics(),
        Some("model.physics3.json")
    );
    assert_eq!(runtime.runtime().model().pose(), Some("model.pose3.json"));
    assert!(runtime.runtime().physics().is_some());
    assert_eq!(runtime.textures().len(), 1);
    assert_eq!(runtime.textures()[0].rgba(), [0, 0, 0, 255]);
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
