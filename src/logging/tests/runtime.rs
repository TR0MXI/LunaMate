use std::{
    env, fs,
    path::PathBuf,
    process::{self, Command},
};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

use flexi_logger::LogSpecification;

use super::super::crash::{
    CRASH_LOG_BASENAME, CrashBacktraceStatus, CrashContext, CrashLocation, MAX_CRASH_RECORD_BYTES,
    format_crash_record, is_lunamate_diagnostic_name_for_test, persist_crash_record_at,
    prepare_log_directory_at,
};
use super::super::*;

const PANIC_HOOK_CHILD_ENV: &str = "LUNAMATE_TEST_PANIC_HOOK_CHILD";
const PANIC_TEST_PAYLOAD: &str = "panic payload must stay out of crash records";
#[cfg(unix)]
const PERMISSION_CHILD_ENV: &str = "LUNAMATE_TEST_LOG_PERMISSION_CHILD";
#[cfg(unix)]
const UNSAFE_PATH_CHILD_ENV: &str = "LUNAMATE_TEST_LOG_UNSAFE_PATH_CHILD";

#[test]
fn every_application_log_level_produces_a_valid_spec() {
    for level in [
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ] {
        assert!(LogSpecification::parse(application_log_spec(level)).is_ok());
    }
}

#[test]
fn asynchronous_file_writer_can_be_built_from_startup_settings() {
    let directory = env::temp_dir().join(format!("lunamate-logging-build-test-{}", process::id()));
    let _ = fs::remove_dir_all(&directory);
    {
        let settings = LoggingSettings {
            max_size_mb: 25,
            keep_files: 5,
            ..LoggingSettings::default()
        };
        let logger = file_logger(
            settings,
            FileSpec::default()
                .directory(directory.clone())
                .basename("lunamate-test"),
        )
        .expect("测试日志器配置应当有效");
        let (_logger, handle) = logger.build().expect("测试日志器应当可以构建");
        handle.shutdown();
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn file_setting_changes_are_deferred_until_restart() {
    let base = LoggingSettings::default();

    assert_eq!(
        settings_apply_outcome(
            Some(base),
            LoggingSettings {
                level: LogLevel::Trace,
                ..base
            }
        ),
        ApplyLoggingSettingsOutcome::LevelApplied
    );
    assert_eq!(
        settings_apply_outcome(Some(base), base),
        ApplyLoggingSettingsOutcome::LevelApplied
    );
    assert_eq!(
        settings_apply_outcome(None, base),
        ApplyLoggingSettingsOutcome::FilePolicyDeferredUntilRestart
    );

    for changed in [
        LoggingSettings {
            rotation: !base.rotation,
            ..base
        },
        LoggingSettings {
            compression: !base.compression,
            ..base
        },
        LoggingSettings {
            max_size_mb: base.max_size_mb + 1,
            ..base
        },
        LoggingSettings {
            keep_files: base.keep_files + 1,
            ..base
        },
    ] {
        assert!(
            settings_apply_outcome(Some(base), changed)
                == ApplyLoggingSettingsOutcome::FilePolicyDeferredUntilRestart,
            "{changed:?} 应当延后到下次启动生效"
        );
    }
}

#[test]
fn cleanup_policy_follows_the_compression_setting() {
    let compressed = cleanup(LoggingSettings {
        compression: true,
        keep_files: 7,
        ..LoggingSettings::default()
    });
    let plain = cleanup(LoggingSettings {
        compression: false,
        keep_files: 3,
        ..LoggingSettings::default()
    });

    assert!(matches!(compressed, Cleanup::KeepCompressedFiles(7)));
    assert!(matches!(plain, Cleanup::KeepLogFiles(3)));
}

#[test]
fn disabling_rotation_still_produces_a_usable_logger_configuration() {
    let directory = env::temp_dir().join(format!(
        "lunamate-logging-no-rotation-test-{}",
        process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    let file_spec = || {
        FileSpec::default()
            .directory(directory.clone())
            .basename("lunamate-test")
    };
    let settings = LoggingSettings {
        rotation: false,
        ..LoggingSettings::default()
    };
    {
        let (_logger, handle) = file_logger(settings, file_spec())
            .expect("关闭轮转的日志器配置应当有效")
            .build()
            .expect("关闭轮转的日志器应当可以构建");
        handle.shutdown();
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn application_log_spec_silences_third_party_targets() {
    let spec = application_log_spec(LogLevel::Debug);

    assert!(spec.starts_with("off,"));
    assert!(spec.ends_with("=debug"));
    assert!(spec.contains(env!("CARGO_PKG_NAME")));
    let parsed = LogSpecification::parse(&spec).expect("应用日志过滤应当可以解析");
    for target in ["lunamate::app", "lunamate_agent::provider"] {
        assert!(parsed.enabled(log::Level::Debug, target));
    }
    for target in ["genai", "hyper::client", "reqwest::connect", "surrealdb"] {
        assert!(!parsed.enabled(log::Level::Error, target));
    }
}

#[test]
fn log_files_are_written_under_the_working_directory() {
    assert_eq!(
        file_spec().used_directory(),
        PathBuf::from("./logs/lunamate")
    );
}

#[test]
fn only_reserved_diagnostic_names_are_owned_by_lunamate() {
    for owned in [
        "lunamate.log",
        "lunamate_2026-08-02_12-00-00.log",
        "lunamate_rCURRENT.log",
        "lunamate_r2026-08-02_12-00-00.log.gz",
        CRASH_LOG_BASENAME,
    ] {
        assert!(is_lunamate_diagnostic_name_for_test(std::ffi::OsStr::new(
            owned
        )));
    }
    for unrelated in [
        "other.log",
        "lunamate-notes.log",
        "lunamate_backup.txt",
        "lunamate_rCURRENT.log.tmp",
    ] {
        assert!(!is_lunamate_diagnostic_name_for_test(std::ffi::OsStr::new(
            unrelated
        )));
    }
}

#[test]
fn settings_cannot_be_applied_before_the_logger_is_initialized() {
    // 本 crate 的测试从不调用 init()，因此该分支始终可观测。
    assert_eq!(
        apply_current_settings(),
        Err("logger is not initialized".to_owned())
    );
}

#[test]
fn shutting_down_an_uninitialized_logger_is_a_no_op() {
    shutdown();
    shutdown();
}

#[test]
fn logger_guard_shutdown_decision_is_idempotent() {
    let mut guard = LoggerGuard::new(true);

    assert!(guard.take_shutdown());
    assert!(!guard.take_shutdown());
}

#[test]
fn log_field_sanitization_is_bounded_and_single_line() {
    let sanitized = sanitize_log_field(format!(
        "safe value\r\n\x1b[31m\u{7}中\\tail{}",
        "x".repeat(MAX_LOG_FIELD_BYTES * 2)
    ));

    assert!(sanitized.len() <= MAX_LOG_FIELD_BYTES);
    assert!(sanitized.is_char_boundary(sanitized.len()));
    assert!(sanitized.contains("safe\\x20value\\r\\n\\u{1b}[31m\\u{7}中\\\\tail"));
    assert!(
        sanitized
            .chars()
            .all(|character| !matches!(character, '\r' | '\n' | '\x1b' | '\u{7}'))
    );
}

#[test]
fn stable_formatter_includes_all_metadata_sentinels() {
    let arguments = format_args!("event=formatter_sentinel field=value");
    let record = log::Record::builder()
        .args(arguments)
        .level(log::Level::Info)
        .target("formatter_sentinel_target")
        .build();
    let mut now = DeferredNow::new();
    let mut rendered = Vec::new();

    stable_log_format(&mut rendered, &mut now, &record).expect("稳定 formatter 应可写入内存");
    let rendered = String::from_utf8(rendered).expect("formatter 只应输出有效 UTF-8");

    assert!(rendered.starts_with("timestamp="));
    assert!(rendered.contains('Z'));
    assert!(rendered.contains(" thread="));
    assert!(rendered.contains(" level=INFO"));
    assert!(rendered.contains(" target=formatter_sentinel_target"));
    assert!(rendered.ends_with("event=formatter_sentinel field=value"));
}

#[test]
fn crash_record_is_stable_bounded_and_redacts_paths() {
    let long_backtrace = format!(
        "0: lunamate::worker\n   at /private/build/src/main.rs:7:9\n{}",
        "x".repeat(MAX_CRASH_RECORD_BYTES * 2)
    );
    let record = format_crash_record(
        CrashContext {
            unix_time_seconds: 1_723_000_000,
            version: "0.1.0",
            pid: 42,
            thread_name: Some("worker/private"),
            location: Some(CrashLocation {
                file: "/private/build/src/main.rs",
                line: 7,
                column: 9,
            }),
            backtrace_status: CrashBacktraceStatus::Captured,
        },
        Some(&long_backtrace),
    );

    assert!(record.starts_with("event=process_panic\n"));
    assert!(record.contains("unix_time_seconds=1723000000\n"));
    assert!(record.contains("version=0.1.0\n"));
    assert!(record.contains("pid=42\n"));
    assert!(record.contains("thread_name=redacted\n"));
    assert!(record.contains("location=main.rs:7:9\n"));
    assert!(record.contains("backtrace_status=captured\n"));
    assert!(record.contains("backtrace_truncated=true\n"));
    assert!(record.contains("at <path>\n"));
    assert!(!record.contains("/private"));
    assert!(record.len() <= MAX_CRASH_RECORD_BYTES);
}

#[test]
fn panic_hook_persists_a_private_record_and_calls_the_previous_hook() {
    let working_directory =
        tempfile::tempdir().expect("panic hook 子进程需要一个独立且可写的工作目录");
    let output = Command::new(env::current_exe().expect("测试运行时应提供当前测试可执行文件路径"))
        .current_dir(working_directory.path())
        .arg("--exact")
        .arg("logging::tests::runtime::panic_hook_child")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(PANIC_HOOK_CHILD_ENV, "1")
        .env("RUST_BACKTRACE", "0")
        .output()
        .expect("panic hook 子进程应当可以启动");

    assert!(!output.status.success(), "触发 panic 的子进程不应成功退出");
    let crash_path = working_directory
        .path()
        .join("logs")
        .join("lunamate")
        .join(CRASH_LOG_BASENAME);
    let record = fs::read_to_string(&crash_path)
        .expect("panic hook 应在固定 logs/lunamate 目录同步写入 crash 记录");
    assert!(record.contains("event=process_panic\n"));
    assert!(record.contains(concat!("version=", env!("CARGO_PKG_VERSION"), "\n")));
    assert!(record.contains("thread_name=logging::tests::runtime::panic_hook_child\n"));
    assert!(record.contains("location=runtime.rs:"));
    assert!(record.contains("backtrace_status=captured\n"));
    assert!(record.contains("backtrace_begin\n"));
    assert!(record.contains("backtrace_end\n"));
    assert!(record.len() <= MAX_CRASH_RECORD_BYTES);
    assert!(!record.contains(PANIC_TEST_PAYLOAD));
    assert!(record.lines().any(|line| {
        line.strip_prefix("unix_time_seconds=")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|value| value > 0)
    }));
    assert!(record.lines().any(|line| {
        line.strip_prefix("pid=")
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|value| value > 0)
    }));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(PANIC_TEST_PAYLOAD),
        "安装前的默认 hook 应继续接收原始 panic"
    );
    #[cfg(unix)]
    assert_eq!(unix_mode(&crash_path), 0o600);
}

#[test]
fn panic_hook_child() {
    if env::var_os(PANIC_HOOK_CHILD_ENV).is_some() {
        install_panic_hook();
        panic!("{PANIC_TEST_PAYLOAD}");
    }
}

#[cfg(unix)]
#[test]
fn unix_log_permissions_are_private_in_an_isolated_process() {
    let root = tempfile::tempdir().expect("权限测试需要一个独立且可写的临时目录");
    let logs_root = root.path().join("logs");
    fs::create_dir(&logs_root).expect("权限测试应能建立共享 logs 根目录");
    fs::set_permissions(&logs_root, fs::Permissions::from_mode(0o751))
        .expect("权限测试应能设置共享 logs 根目录的初始权限");
    let root_unrelated = logs_root.join("shared-diagnostic.txt");
    fs::write(&root_unrelated, b"not owned by LunaMate\n")
        .expect("权限测试应能建立 logs 根目录无关文件");
    fs::set_permissions(&root_unrelated, fs::Permissions::from_mode(0o640))
        .expect("权限测试应能设置无关文件初始权限");

    let directory = logs_root.join("lunamate");
    fs::create_dir(&directory).expect("权限测试应能建立日志目录");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777))
        .expect("权限测试应能设置初始目录权限");
    let existing_log = directory.join("lunamate_existing.log");
    fs::write(&existing_log, b"existing diagnostic\n").expect("权限测试应能建立已有日志文件");
    fs::set_permissions(&existing_log, fs::Permissions::from_mode(0o666))
        .expect("权限测试应能设置初始文件权限");
    let child_unrelated = directory.join("notes.txt");
    fs::write(&child_unrelated, b"not a LunaMate diagnostic\n")
        .expect("权限测试应能建立专属目录内的无关文件");
    fs::set_permissions(&child_unrelated, fs::Permissions::from_mode(0o646))
        .expect("权限测试应能设置专属目录无关文件的初始权限");

    let output = Command::new(env::current_exe().expect("测试运行时应提供当前测试可执行文件路径"))
        .arg("--exact")
        .arg("logging::tests::runtime::unix_log_permissions_child")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(PERMISSION_CHILD_ENV, &directory)
        .output()
        .expect("Unix 权限子进程应当可以启动");

    assert!(
        output.status.success(),
        "Unix 权限子进程失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(unix_mode(&logs_root), 0o751);
    assert_eq!(unix_mode(&root_unrelated), 0o640);
    assert_eq!(unix_mode(&child_unrelated), 0o646);
    assert_eq!(unix_mode(&directory), 0o700);
    assert_eq!(unix_mode(&existing_log), 0o600);
}

#[cfg(unix)]
#[test]
fn unix_log_permissions_child() {
    let Some(directory) = env::var_os(PERMISSION_CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    prepare_log_directory_at(&directory).expect("日志目录应能被收紧为私有权限");
    assert_eq!(unix_mode(&directory), 0o700);
    assert_eq!(unix_mode(&directory.join("lunamate_existing.log")), 0o600);
    assert_eq!(unix_mode(&directory.join("notes.txt")), 0o646);

    let file_spec = FileSpec::default()
        .directory(directory.clone())
        .basename("lunamate_permission_test");
    let (logger, handle) = file_logger(LoggingSettings::default(), file_spec)
        .expect("权限测试日志器配置应当有效")
        .build()
        .expect("权限测试日志器应当可以构建");
    let record = log::Record::builder()
        .args(format_args!("event=permission_test"))
        .level(log::Level::Info)
        .target(APPLICATION_LOG_TARGET)
        .build();
    logger.log(&record);
    handle.shutdown();

    persist_crash_record_at(&directory, "event=process_panic\nrecord_end\n")
        .expect("crash 记录应能同步写入私有文件");
    let future_file = directory.join("lunamate_future.log");
    fs::File::create(&future_file).expect("收紧 umask 后仍应能建立后续日志文件");

    let mut generated_log_count = 0;
    for entry in fs::read_dir(&directory).expect("权限测试应能枚举日志目录") {
        let entry = entry.expect("日志目录项应当可读");
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("lunamate_permission_test"))
        {
            generated_log_count += 1;
            assert_eq!(unix_mode(&entry.path()), 0o600);
        }
    }
    assert!(generated_log_count > 0, "flexi_logger 应实际创建日志文件");
    assert_eq!(unix_mode(&directory.join(CRASH_LOG_BASENAME)), 0o600);
    assert_eq!(unix_mode(&future_file), 0o600);
}

#[cfg(unix)]
#[test]
fn unix_owned_links_fail_closed_without_changing_external_permissions() {
    if let Some(directory) = env::var_os(UNSAFE_PATH_CHILD_ENV).map(PathBuf::from) {
        assert!(
            prepare_log_directory_at(&directory).is_err(),
            "owned link path 必须拒绝而不是跟随"
        );
        return;
    }

    let root = tempfile::tempdir().expect("链接测试需要独立临时目录");
    let target = root.path().join("external.log");
    fs::write(&target, b"external\n").expect("链接测试应能建立外部目标");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o664))
        .expect("链接测试应能设置外部目标权限");

    for link_kind in ["symbolic", "hard"] {
        let directory = root.path().join(format!("{link_kind}-logs"));
        fs::create_dir(&directory).expect("链接测试应能建立专属目录");
        let owned_path = directory.join("lunamate_rCURRENT.log");
        if link_kind == "symbolic" {
            symlink(&target, &owned_path).expect("链接测试应能建立符号链接");
        } else {
            fs::hard_link(&target, &owned_path).expect("链接测试应能建立硬链接");
        }
        let output = Command::new(
            env::current_exe().expect("链接测试运行时应提供当前测试可执行文件路径"),
        )
        .arg("--exact")
        .arg("logging::tests::runtime::unix_owned_links_fail_closed_without_changing_external_permissions")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(UNSAFE_PATH_CHILD_ENV, &directory)
        .output()
        .expect("链接测试子进程应当可以启动");
        assert!(
            output.status.success(),
            "{link_kind} link 子进程失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(unix_mode(&target), 0o664);
    }
}

#[cfg(unix)]
fn unix_mode(path: &std::path::Path) -> u32 {
    fs::metadata(path)
        .expect("权限断言目标应当存在")
        .permissions()
        .mode()
        & 0o777
}
