use std::{env, fs, process};

use flexi_logger::LogSpecification;
use rust_i18n::t;

use super::super::*;

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
fn log_messages_exist_in_every_supported_language() {
    let keys = [
        "log.apply_settings_failed",
        "log.file_fallback",
        "log.config_rebuilt",
        "log.gpu_device_lost",
        "log.gpu_worker_panicked",
        "log.model_capability_warning",
        "log.chat_close_save_failed",
        "log.chat_save_failed",
        "log.frame_render_stopped",
        "log.pet_move_unsupported",
        "log.startup_config_warning",
        "log.pet_window_config_failed",
        "log.exit_chat_save_failed",
        "log.exit_position_save_failed",
        "log.main_window_create_failed",
        "log.gpu_underlay_init_failed",
        "log.gpu_worker_exited",
        "log.gpu_model_cpu_fallback",
        "log.gpu_underlay_cpu_fallback",
        "log.settings_window_config_failed",
        "log.settings_window_create_failed",
        "log.image_release_failed",
        "log.logging_update_failed",
        "log.settings_move_unsupported",
    ];

    for locale in ["zh-CN", "zh-TW", "en", "ja"] {
        for key in keys {
            assert!(
                crate::_rust_i18n_try_translate(locale, key).is_some(),
                "{locale} 缺少日志翻译：{key}"
            );
        }
    }
    assert_eq!(
        t!("log.chat_save_failed", locale = "en", error = "disk full"),
        "Failed to save the chat session: disk full"
    );
}

#[test]
fn asynchronous_file_writer_can_be_reconfigured() {
    let directory = env::temp_dir().join(format!("lunamate-logging-reset-test-{}", process::id()));
    let _ = fs::remove_dir_all(&directory);
    let test_file_spec = || {
        FileSpec::default()
            .directory(directory.clone())
            .basename("lunamate-test")
    };
    {
        let logger = file_logger(LoggingSettings::default(), test_file_spec())
            .expect("测试日志器配置应当有效");
        let (_logger, handle) = logger.build().expect("测试日志器应当可以构建");
        let updated = LoggingSettings {
            max_size_mb: 25,
            keep_files: 5,
            ..LoggingSettings::default()
        };

        handle
            .reset_flw(&file_writer_builder(updated, test_file_spec()))
            .expect("启动和重配使用相同异步模式时应当允许重置");
        handle.shutdown();
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn only_rotation_fields_force_a_file_writer_rebuild() {
    let base = LoggingSettings::default();

    // 等级由日志过滤器单独应用，不应触发 writer 重建与轮转清理。
    assert!(file_writer_settings_match(
        base,
        LoggingSettings {
            level: LogLevel::Trace,
            ..base
        }
    ));
    assert!(file_writer_settings_match(base, base));

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
            !file_writer_settings_match(base, changed),
            "{changed:?} 应当触发 file writer 重建"
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

        handle
            .reset_flw(&file_writer_builder(settings, file_spec()))
            .expect("关闭轮转后仍应允许重置 writer");
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
}

#[test]
fn log_files_are_written_under_the_working_directory() {
    let spec = format!("{:?}", file_spec());

    assert!(spec.contains("logs"));
    assert!(spec.contains("lunamate"));
}

#[test]
fn settings_cannot_be_applied_before_the_logger_is_initialized() {
    // 本 crate 的测试从不调用 init()，因此该分支始终可观测。
    assert_eq!(apply_current_settings(), Err("日志器尚未初始化".to_owned()));
}

#[test]
fn shutting_down_an_uninitialized_logger_is_a_no_op() {
    shutdown();
    shutdown();
}
