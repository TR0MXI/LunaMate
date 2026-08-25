//! 验证工具权限关闭语义、损坏配置备份与文件权限。

use std::{
    fs,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use toml_edit::DocumentMut;

use super::TestDirectory;
use crate::config::*;

#[test]
fn agent_screenshot_permission_is_explicit_and_round_trips() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    assert!(!config.allow_agent_screenshot());

    let enable_revision = config.reserve_allow_agent_screenshot_revision(true);
    assert!(
        !config.allow_agent_screenshot(),
        "授权写入完成前不得开放工具"
    );
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(true, enable_revision)
            .expect("Agent 截屏授权应当可以开启"),
        Some(())
    );
    assert!(config.allow_agent_screenshot());
    assert!(LunaConfig::load_from(directory.config_path()).allow_agent_screenshot());

    let disable_revision = config.reserve_allow_agent_screenshot_revision(false);
    assert!(
        !config.allow_agent_screenshot(),
        "关闭请求一经提交就必须立即撤销运行时授权"
    );
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(false, disable_revision)
            .expect("Agent 截屏授权应当可以关闭"),
        Some(())
    );
    assert!(!config.allow_agent_screenshot());
    let saved = fs::read_to_string(directory.config_path()).expect("工具配置应当可以读取");
    assert!(saved.contains("allow_agent_screenshot = false"));
}

#[test]
fn agent_outfit_tool_switch_defaults_to_enabled_and_round_trips() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    assert!(config.allow_agent_outfit_change());

    let revision = config.reserve_allow_agent_outfit_change_revision();
    assert_eq!(
        config
            .set_allow_agent_outfit_change_at_revision(false, revision)
            .expect("Agent 换装工具应当可以关闭"),
        Some(())
    );
    assert!(!config.allow_agent_outfit_change());
    assert!(!LunaConfig::load_from(directory.config_path()).allow_agent_outfit_change());
    let saved = fs::read_to_string(directory.config_path()).expect("工具配置应当可以读取");
    assert!(saved.contains("allow_agent_outfit_change = false"));
}

#[test]
fn agent_screenshot_permission_revision_notifies_subscribers() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let mut revisions = config.subscribe_agent_screenshot_permission_revision();
    assert_eq!(*revisions.borrow_and_update(), 0);

    let revision = config.reserve_allow_agent_screenshot_revision(true);

    assert!(
        revisions
            .has_changed()
            .expect("本地授权 revision channel 应当保持开放")
    );
    assert_eq!(*revisions.borrow_and_update(), revision);
}

#[test]
fn screenshot_revocation_waits_for_an_in_flight_task_start() {
    let directory = TestDirectory::new();
    let config = Arc::new(LunaConfig::load_from(directory.config_path()));
    let enable = config.reserve_allow_agent_screenshot_revision(true);
    config
        .set_allow_agent_screenshot_at_revision(true, enable)
        .expect("测试截图授权应当可以持久化")
        .expect("最新截图授权应当发布");
    let authorization = config
        .begin_agent_screenshot_capture(enable)
        .expect("当前授权应当可以启动截图任务");
    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let (finished_tx, finished_rx) = mpsc::sync_channel(0);
    let config_for_revocation = Arc::clone(&config);
    let revoke = thread::spawn(move || {
        started_tx.send(()).expect("测试线程应当报告已开始");
        let revision = config_for_revocation.reserve_allow_agent_screenshot_revision(false);
        finished_tx
            .send(revision)
            .expect("测试线程应当报告撤权完成");
    });

    started_rx.recv().expect("撤权线程应当启动");
    assert!(
        finished_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "截图启动租约释放前撤权不得完成"
    );
    drop(authorization);
    let disable = finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("释放租约后撤权应当完成");
    revoke.join().expect("撤权线程不应 panic");

    assert!(!config.agent_screenshot_permission_is_current(enable));
    assert!(
        config.begin_agent_screenshot_capture(disable).is_none(),
        "关闭 revision 不得取得截图启动租约"
    );
}

#[test]
fn invalid_tool_switches_use_their_defaults() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[tools]
allow_agent_screenshot = "yes"
allow_agent_outfit_change = "yes"

[debug]
show_fps = true
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());
    assert!(!config.allow_agent_screenshot());
    assert!(config.allow_agent_outfit_change());
    assert!(config.show_fps());
    assert!(config.startup_warning().is_some());
}

#[test]
fn stale_screenshot_enable_cannot_override_newer_disable() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let stale_enable = config.reserve_allow_agent_screenshot_revision(true);
    let current_disable = config.reserve_allow_agent_screenshot_revision(false);

    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(false, current_disable)
            .expect("最新关闭请求应当可以保存"),
        Some(())
    );
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(true, stale_enable)
            .expect("迟到开启请求应当被无害丢弃"),
        None
    );
    assert!(!config.allow_agent_screenshot());
    assert!(!LunaConfig::load_from(directory.config_path()).allow_agent_screenshot());
}

#[test]
fn failed_screenshot_disable_stays_closed_when_config_path_becomes_unreadable() {
    let directory = TestDirectory::new();
    directory.write("[tools]\nallow_agent_screenshot = true\n");
    let config_path = directory.config_path();
    let config = LunaConfig::load_from(config_path.clone());
    assert!(config.allow_agent_screenshot());
    fs::remove_file(&config_path).expect("测试配置文件应当可以移除");
    fs::create_dir(&config_path).expect("冲突目标目录应当可以创建");

    let revision = config.reserve_allow_agent_screenshot_revision(false);
    assert!(!config.allow_agent_screenshot());
    let result = config.set_allow_agent_screenshot_at_revision(false, revision);

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert!(
        !config.allow_agent_screenshot(),
        "配置路径已不可读时必须保持截屏权限关闭"
    );
    assert!(!config.requested_allow_agent_screenshot());
    assert!(config.agent_screenshot_permission_retry_required());

    fs::remove_dir(&config_path).expect("冲突目标目录应当可以移除");
    let retry_revision = config.reserve_allow_agent_screenshot_revision(false);
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(false, retry_revision)
            .expect("关闭状态应当可以安全重试"),
        Some(())
    );
    assert!(!config.agent_screenshot_permission_retry_required());
    assert!(!LunaConfig::load_from(config_path).allow_agent_screenshot());
}

#[cfg(target_os = "linux")]
#[test]
fn failed_screenshot_disable_does_not_reopen_permission_from_readable_old_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TestDirectory::new();
    let read_only_directory = directory.0.join("read-only");
    fs::create_dir(&read_only_directory).expect("只读测试目录应当可以创建");
    let config_path = read_only_directory.join("config.toml");
    fs::write(&config_path, "[tools]\nallow_agent_screenshot = true\n")
        .expect("旧授权配置应当可以写入");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("测试配置权限应当可以设置");
    fs::set_permissions(&read_only_directory, fs::Permissions::from_mode(0o500))
        .expect("测试目录应当可以设为只读");
    let config = LunaConfig::load_from(config_path.clone());
    assert!(config.allow_agent_screenshot());

    let revision = config.reserve_allow_agent_screenshot_revision(false);
    let result = config.set_allow_agent_screenshot_at_revision(false, revision);
    fs::set_permissions(&read_only_directory, fs::Permissions::from_mode(0o700))
        .expect("测试目录权限应当可以恢复");

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert!(!config.allow_agent_screenshot());
    assert!(!config.requested_allow_agent_screenshot());
    assert!(config.agent_screenshot_permission_retry_required());
    assert!(
        LunaConfig::load_from(config_path).allow_agent_screenshot(),
        "回归前置条件要求旧磁盘授权仍然可读"
    );
}

#[test]
fn failed_screenshot_enable_rolls_back_without_requesting_disable_retry() {
    let directory = TestDirectory::new();
    let config_path = directory.config_path();
    let config = LunaConfig::load_from(config_path.clone());
    fs::create_dir(&config_path).expect("冲突目标目录应当可以创建");

    let revision = config.reserve_allow_agent_screenshot_revision(true);
    let result = config.set_allow_agent_screenshot_at_revision(true, revision);

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert!(!config.allow_agent_screenshot());
    assert!(!config.requested_allow_agent_screenshot());
    assert!(!config.agent_screenshot_permission_retry_required());
}

#[cfg(unix)]
#[test]
fn corrupt_config_backup_is_complete_and_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TestDirectory::new();
    let original = b"[render\nframe_rate = 30";
    directory.write_bytes(original);
    let config = LunaConfig::load_from(directory.config_path());

    config
        .set_remember_window_positions(false)
        .expect("损坏配置备份成功后应当可以重建");

    assert_eq!(
        fs::read(directory.corrupt_backup_path()).expect("损坏配置备份应当可读"),
        original
    );
    let mode = fs::metadata(directory.corrupt_backup_path())
        .expect("损坏配置备份元数据应当可读")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn failed_corrupt_backup_keeps_the_original_file_and_runtime_value() {
    let directory = TestDirectory::new();
    let original = b"[render\nframe_rate = 30";
    directory.write_bytes(original);
    fs::create_dir(directory.corrupt_backup_path()).expect("冲突备份目录应当可以创建");
    let config = LunaConfig::load_from(directory.config_path());

    let error = config
        .set_remember_window_positions(false)
        .expect_err("备份失败必须阻止配置重建");

    match error {
        ConfigWriteError::Io {
            operation, path, ..
        } => {
            assert_eq!(operation, "提交损坏配置备份");
            assert_eq!(path, directory.corrupt_backup_path());
        }
        other => panic!("备份失败应映射为 I/O 错误，实际为：{other}"),
    }
    assert_eq!(
        fs::read(directory.config_path()).expect("原配置文件应当保持可读"),
        original
    );
    assert!(directory.corrupt_backup_path().is_dir());
    assert!(config.remember_window_positions());
}

#[test]
fn non_utf8_config_is_backed_up_as_bounded_raw_bytes_before_rebuild() {
    let directory = TestDirectory::new();
    let original = b"[render]\nframe_rate = 30\n# \xff\n";
    directory.write_bytes(original);
    let config = LunaConfig::load_from(directory.config_path());
    assert!(
        config
            .startup_warning()
            .is_some_and(|warning| warning.contains("UTF-8"))
    );

    config
        .set_frame_rate(FrameRate::Fps60)
        .expect("非 UTF-8 配置备份后应当可以重建");

    assert_eq!(
        fs::read(directory.corrupt_backup_path()).expect("原始字节备份应当可读"),
        original
    );
    let rebuilt = fs::read_to_string(directory.config_path()).expect("重建配置必须是 UTF-8");
    rebuilt
        .parse::<DocumentMut>()
        .expect("重建配置必须是完整 TOML");
    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::Fps60
    );
}

#[cfg(unix)]
#[test]
fn existing_wide_config_permissions_are_tightened_before_reading() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TestDirectory::new();
    directory.write("[render]\nframe_rate = 60\n");
    fs::set_permissions(directory.config_path(), fs::Permissions::from_mode(0o666))
        .expect("测试配置应当可以设置宽权限");

    let config = LunaConfig::load_from(directory.config_path());

    assert_eq!(config.frame_rate(), FrameRate::Fps60);
    let mode = fs::metadata(directory.config_path())
        .expect("配置元数据应当可读")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn config_symlink_is_not_followed_for_read_or_write() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let directory = TestDirectory::new();
    let target = directory.0.join("target.toml");
    let secret = "symlink-secret-must-not-appear";
    fs::write(&target, format!("[render]\nframe_rate = 120\n# {secret}\n"))
        .expect("符号链接目标应当可以写入");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).expect("目标权限应当可以设置");
    symlink(&target, directory.config_path()).expect("配置符号链接应当可以创建");

    let config = LunaConfig::load_from(directory.config_path());

    assert_eq!(config.frame_rate(), FrameRate::Fps30);
    let warning = config
        .startup_warning()
        .expect("配置符号链接必须产生启动诊断");
    assert!(warning.contains("符号链接"));
    assert!(!warning.contains(secret));
    let error = config
        .set_frame_rate(FrameRate::Fps60)
        .expect_err("保存也不得跟随配置符号链接");
    assert!(matches!(error, ConfigWriteError::Io { .. }));
    assert!(
        fs::symlink_metadata(directory.config_path())
            .expect("配置符号链接元数据应当可读")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(&target)
            .expect("目标元数据应当可读")
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert!(
        fs::read_to_string(target)
            .expect("符号链接目标应保持可读")
            .contains("frame_rate = 120")
    );
}
