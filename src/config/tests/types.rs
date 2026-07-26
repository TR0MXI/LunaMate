//! 验证配置领域值的稳定标识、原子编码往返与数值约束。

use std::{error::Error as _, num::NonZeroU16};

use crate::config::{
    ConfigWindow, ConfigWriteError, FrameRate, LOGGING_MAX_FILE_SIZE_MB, LOGGING_MAX_KEEP_FILES,
    LOGGING_MIN_FILE_SIZE_MB, LOGGING_MIN_KEEP_FILES, LogLevel, LoggingSettings, ModelWindowSize,
    WindowPosition,
};

fn all_frame_rates() -> Vec<FrameRate> {
    vec![
        FrameRate::Fps30,
        FrameRate::Fps60,
        FrameRate::Fps120,
        FrameRate::FollowDisplay,
        FrameRate::Unlimited,
        FrameRate::custom(1).expect("1 FPS 是合法的自定义帧率"),
        FrameRate::custom(144).expect("144 FPS 是合法的自定义帧率"),
        FrameRate::custom(u16::MAX).expect("u16 上限是合法的自定义帧率"),
    ]
}

#[test]
fn frame_rate_atomic_encoding_round_trips_for_every_mode() {
    for frame_rate in all_frame_rates() {
        assert_eq!(
            FrameRate::from_atomic_value(frame_rate.atomic_value()),
            frame_rate,
            "{frame_rate:?} 的原子编码应当可以还原"
        );
    }
}

#[test]
fn unknown_atomic_frame_rates_fall_back_to_the_default() {
    // 未知标记位、载荷为零的自定义标记，以及任意陌生值都不应产生无效调度参数。
    for value in [7, 1 << 16, 3 << 16, 9 << 20, u32::MAX] {
        assert_eq!(FrameRate::from_atomic_value(value), FrameRate::default());
    }
}

#[test]
fn frame_rate_limits_and_scheduling_traits_match_their_mode() {
    assert_eq!(FrameRate::Fps30.limit(), Some(30));
    assert_eq!(FrameRate::Fps60.limit(), Some(60));
    assert_eq!(FrameRate::Fps120.limit(), Some(120));
    assert_eq!(FrameRate::FollowDisplay.limit(), None);
    assert_eq!(FrameRate::Unlimited.limit(), None);
    assert_eq!(
        FrameRate::custom(90)
            .expect("90 FPS 是合法的自定义帧率")
            .limit(),
        Some(90)
    );

    assert!(FrameRate::FollowDisplay.follows_display());
    assert!(!FrameRate::Unlimited.follows_display());

    // 只有内置档位允许自动降级；用户显式指定的帧率不应被静默改写。
    for frame_rate in [FrameRate::Fps30, FrameRate::Fps60, FrameRate::Fps120] {
        assert!(frame_rate.allows_frame_rate_degradation());
        assert!(frame_rate.uses_vsync());
    }
    for frame_rate in [
        FrameRate::FollowDisplay,
        FrameRate::Unlimited,
        FrameRate::custom(75).expect("75 FPS 是合法的自定义帧率"),
    ] {
        assert!(!frame_rate.allows_frame_rate_degradation());
    }
    assert!(FrameRate::FollowDisplay.uses_vsync());
    assert!(!FrameRate::Unlimited.uses_vsync());
}

#[test]
fn frame_rate_conversion_maps_builtin_values_and_rejects_zero() {
    assert_eq!(FrameRate::try_from(30).ok(), Some(FrameRate::Fps30));
    assert_eq!(FrameRate::try_from(60).ok(), Some(FrameRate::Fps60));
    assert_eq!(FrameRate::try_from(120).ok(), Some(FrameRate::Fps120));
    assert_eq!(
        FrameRate::try_from(45).ok(),
        Some(FrameRate::Custom(NonZeroU16::new(45).expect("45 不为零")))
    );

    let error = FrameRate::try_from(0).expect_err("零帧率不能用于渲染调度");
    assert!(error.to_string().contains('0'));
    assert!(error.source().is_none());
    assert!(FrameRate::custom(0).is_err());
}

#[test]
fn frame_rate_display_names_are_unique_and_non_empty() {
    let mut names = Vec::new();
    for frame_rate in all_frame_rates() {
        let name = frame_rate.display_name();
        assert!(!name.is_empty(), "{frame_rate:?} 应当有可展示名称");
        assert!(!names.contains(&name), "{frame_rate:?} 的名称应当唯一");
        names.push(name);
    }
    assert!(FrameRate::Fps60.display_name().contains("60"));
}

#[test]
fn model_window_size_identifiers_and_atomic_values_round_trip() {
    let sizes = [
        ModelWindowSize::Auto,
        ModelWindowSize::Compact,
        ModelWindowSize::Standard,
        ModelWindowSize::Large,
        ModelWindowSize::ExtraLarge,
    ];

    for size in sizes {
        assert_eq!(ModelWindowSize::from_id(size.id()), Some(size));
        assert_eq!(
            ModelWindowSize::from_atomic_value(size.atomic_value()),
            size
        );
    }

    assert_eq!(ModelWindowSize::default(), ModelWindowSize::Auto);
    assert_eq!(ModelWindowSize::from_id("gigantic"), None);
    // 未知原子值回退到自动档位，避免损坏配置产生零宽窗口。
    assert_eq!(
        ModelWindowSize::from_atomic_value(9_999),
        ModelWindowSize::Auto
    );
}

#[test]
fn only_fixed_model_window_sizes_declare_a_target_width() {
    assert_eq!(ModelWindowSize::Auto.width(), None);
    assert_eq!(ModelWindowSize::Compact.width(), Some(240.0));
    assert_eq!(ModelWindowSize::Standard.width(), Some(300.0));
    assert_eq!(ModelWindowSize::Large.width(), Some(360.0));
    assert_eq!(ModelWindowSize::ExtraLarge.width(), Some(420.0));
}

#[test]
fn log_level_identifiers_round_trip_and_reject_unknown_input() {
    for level in [
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ] {
        assert_eq!(LogLevel::from_id(level.id()), Some(level));
    }

    assert_eq!(LogLevel::default(), LogLevel::Info);
    assert_eq!(LogLevel::from_id("verbose"), None);
    assert_eq!(LogLevel::from_id("ERROR"), None);
}

#[test]
fn logging_settings_accept_the_full_documented_range() {
    for max_size_mb in [LOGGING_MIN_FILE_SIZE_MB, 10, LOGGING_MAX_FILE_SIZE_MB] {
        for keep_files in [LOGGING_MIN_KEEP_FILES, 10, LOGGING_MAX_KEEP_FILES] {
            let settings = LoggingSettings {
                max_size_mb,
                keep_files,
                ..LoggingSettings::default()
            };
            assert_eq!(settings.normalized().as_ref(), Ok(&settings));
        }
    }

    assert_eq!(
        LoggingSettings::default()
            .normalized()
            .expect("默认日志配置必须有效"),
        LoggingSettings::default()
    );
}

#[test]
fn logging_settings_reject_out_of_range_rotation_parameters() {
    let too_small = LoggingSettings {
        max_size_mb: LOGGING_MIN_FILE_SIZE_MB - 1,
        ..LoggingSettings::default()
    };
    let too_large = LoggingSettings {
        max_size_mb: LOGGING_MAX_FILE_SIZE_MB + 1,
        ..LoggingSettings::default()
    };
    let too_few = LoggingSettings {
        keep_files: LOGGING_MIN_KEEP_FILES - 1,
        ..LoggingSettings::default()
    };
    let too_many = LoggingSettings {
        keep_files: LOGGING_MAX_KEEP_FILES + 1,
        ..LoggingSettings::default()
    };

    for settings in [too_small, too_large] {
        assert!(
            settings
                .normalized()
                .is_err_and(|message| message.contains("轮转大小"))
        );
    }
    for settings in [too_few, too_many] {
        assert!(
            settings
                .normalized()
                .is_err_and(|message| message.contains("保留数量"))
        );
    }
}

#[test]
fn logging_rotation_threshold_is_expressed_in_bytes() {
    assert_eq!(
        LoggingSettings {
            max_size_mb: 1,
            ..LoggingSettings::default()
        }
        .max_size_bytes(),
        1024 * 1024
    );
    assert_eq!(
        LoggingSettings {
            max_size_mb: LOGGING_MAX_FILE_SIZE_MB,
            ..LoggingSettings::default()
        }
        .max_size_bytes(),
        1024 * 1024 * 1024
    );
}

#[test]
fn window_positions_reject_non_finite_coordinates() {
    let position = WindowPosition::new(-12.5, 480.0).expect("有限坐标应当被接受");
    assert_eq!(position.x, -12.5);
    assert_eq!(position.y, 480.0);

    for (x, y) in [
        (f32::NAN, 0.0),
        (0.0, f32::NAN),
        (f32::INFINITY, 0.0),
        (0.0, f32::NEG_INFINITY),
    ] {
        assert!(
            WindowPosition::new(x, y).is_none(),
            "({x}, {y}) 不应传入窗口后端"
        );
    }
}

#[test]
fn each_window_persists_under_its_own_table() {
    assert_eq!(ConfigWindow::DesktopPet.table_name(), "desktop_pet");
    assert_eq!(ConfigWindow::Settings.table_name(), "settings");
    assert_ne!(
        ConfigWindow::DesktopPet.table_name(),
        ConfigWindow::Settings.table_name()
    );
}

#[test]
fn config_write_errors_report_operation_context_without_a_source_for_validation() {
    let invalid = ConfigWriteError::InvalidValue("帧率必须为正整数".to_owned());
    assert_eq!(invalid.to_string(), "帧率必须为正整数");
    assert!(invalid.source().is_none());

    let io = ConfigWriteError::Io {
        operation: "写入配置文件",
        path: std::path::PathBuf::from("/tmp/lunamate/config.toml"),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };
    let message = io.to_string();
    assert!(message.starts_with("写入配置文件 /tmp/lunamate/config.toml 失败："));
    assert!(io.source().is_some());
}
