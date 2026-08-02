use crate::{
    Error,
    json::{Motion3, MotionCurve},
    motion::MotionPlayer,
};
use std::{fs, sync::Arc};

const MAX_MOTION_CURVES: usize = 4_096;
const MAX_SEGMENTS_PER_CURVE: usize = 16_384;
const MAX_TOTAL_SEGMENTS: usize = 65_536;
const MAX_TOTAL_POINTS: usize = 131_072;

#[test]
fn rejects_metadata_counts_that_do_not_match_actual_data() {
    let curves = vec![curve("ParamAngleX", linear_segments(1))];

    for (reported, expected) in [
        ([0, 1, 2], "reported curve count 0"),
        ([1, 0, 2], "reported total segment count 0"),
        ([1, 1, 0], "reported total point count 0"),
    ] {
        let source = motion_source(curves.clone(), reported);
        assert_invalid_contains(&source, expected);
    }
}

#[test]
fn accepts_only_the_known_vtube_studio_version_zero_point_count() {
    let segments = "0,0,1,0.25,0,0.75,0,1,1";
    let source = raw_motion_source_with_version(0, "1", "60", segments, 1, 7);
    let motion = Motion3::from_json_str(&source)
        .expect("VTube Studio Version 0 的额外每曲线三点计数应被接受");
    assert_eq!(motion.curves()[0].segments().len(), 1);

    for source in [
        raw_motion_source_with_version(0, "1", "60", segments, 1, 8),
        raw_motion_source_with_version(3, "1", "60", segments, 1, 7),
    ] {
        assert_invalid_contains(&source, "reported total point count");
    }
}

#[test]
#[ignore = "需要通过 LUNAMATE_TEST_MOTION 提供用户自备 motion3.json"]
fn parses_user_supplied_motion_file() {
    let path = std::env::var_os("LUNAMATE_TEST_MOTION")
        .expect("手动动作解析测试必须设置 LUNAMATE_TEST_MOTION");
    let source = fs::read_to_string(&path).expect("用户自备 motion3.json 应可读取为 UTF-8");

    Motion3::from_json_str(&source).expect("用户自备 motion3.json 应可通过当前解析校验");
}

#[test]
fn rejects_actual_curve_count_over_limit_even_when_meta_claims_zero() {
    let curves = (0..=MAX_MOTION_CURVES)
        .map(|index| curve(&format!("Param{index}"), vec![0.0, 0.0]))
        .collect();
    let source = motion_source(curves, [0, 0, 0]);

    assert_invalid_contains(
        &source,
        &format!(
            "curve count {} exceeds limit {MAX_MOTION_CURVES}",
            MAX_MOTION_CURVES + 1
        ),
    );
}

#[test]
fn rejects_curve_segment_count_over_limit() {
    let segments = linear_segments(MAX_SEGMENTS_PER_CURVE + 1);
    let source = motion_source(vec![curve("ParamAngleX", segments)], [1, 0, 0]);

    assert_invalid_contains(
        &source,
        &format!("per-curve limit {MAX_SEGMENTS_PER_CURVE}"),
    );
}

#[test]
fn rejects_total_segment_count_over_limit() {
    let segment_count_per_curve = 13_108;
    let curves = (0..5)
        .map(|index| {
            curve(
                &format!("Param{index}"),
                linear_segments(segment_count_per_curve),
            )
        })
        .collect();
    let source = motion_source(curves, [0, 0, 0]);

    assert_invalid_contains(
        &source,
        &format!("total segment count 65540 exceeds limit {MAX_TOTAL_SEGMENTS}"),
    );
}

#[test]
fn rejects_total_point_count_over_limit() {
    let segment_count_per_curve = 11_000;
    let curves = (0..4)
        .map(|index| {
            curve(
                &format!("Param{index}"),
                bezier_segments(segment_count_per_curve),
            )
        })
        .collect();
    let source = motion_source(curves, [0, 0, 0]);

    assert_invalid_contains(
        &source,
        &format!("total point count 132004 exceeds limit {MAX_TOTAL_POINTS}"),
    );
}

#[test]
fn rejects_numbers_that_overflow_to_infinity() {
    let linear = "0,0,0,1,1";
    let bezier = "0,0,1,0.25,0,0.75,0,1,1";
    let cases = [
        (
            raw_motion_source("3.5e38", "60", linear, 1, 2),
            "motion duration must be finite",
        ),
        (
            raw_motion_source("1", "-3.5e38", linear, 1, 2),
            "motion FPS must be finite",
        ),
        (
            raw_motion_source("1", "60", "3.5e38,0,0,1,1", 1, 2),
            "first point time must be finite",
        ),
        (
            raw_motion_source("1", "60", "0,-3.5e38,0,1,1", 1, 2),
            "first point value must be finite",
        ),
        (
            raw_motion_source("1", "60", "0,0,0,3.5e38,1", 1, 2),
            "end point time must be finite",
        ),
        (
            raw_motion_source("1", "60", "0,0,0,1,-3.5e38", 1, 2),
            "end point value must be finite",
        ),
        (
            raw_motion_source("1", "60", "0,0,1,3.5e38,0,0.75,0,1,1", 1, 4),
            "Bezier control point 1 time must be finite",
        ),
        (
            raw_motion_source("1", "60", "0,0,1,0.25,-3.5e38,0.75,0,1,1", 1, 4),
            "Bezier control point 1 value must be finite",
        ),
        (
            raw_motion_source("1", "60", "0,0,1,0.25,0,3.5e38,0,1,1", 1, 4),
            "Bezier control point 2 time must be finite",
        ),
        (
            raw_motion_source("1", "60", "0,0,1,0.25,0,0.75,-3.5e38,1,1", 1, 4),
            "Bezier control point 2 value must be finite",
        ),
    ];

    for (source, expected) in cases {
        assert_invalid_contains(&source, expected);
    }

    let motion = Motion3::from_json_str(&raw_motion_source("1", "60", bezier, 1, 4))
        .expect("固定有限 Bezier 动作应可解析");
    assert_eq!(motion.curves()[0].segments().len(), 1);
}

#[test]
fn rejects_curve_fade_times_that_overflow_to_infinity() {
    for (field, expected) in [
        ("FadeInTime", "curve 0 fade-in time must be finite"),
        ("FadeOutTime", "curve 0 fade-out time must be finite"),
    ] {
        let source = raw_motion_source_with_curve_field(field, "3.5e38");
        assert_invalid_contains(&source, expected);
    }
}

#[test]
fn rejects_non_monotonic_segment_times() {
    let cases = [
        (
            "0,0,0,0,1",
            1,
            2,
            "end time must be greater than its start time",
        ),
        (
            "0,0,0,2,1,2,1,2",
            2,
            3,
            "end time must be greater than its start time",
        ),
        (
            "0,0,1,-0.25,0,0.75,0,1,1",
            1,
            4,
            "start <= control1 <= control2 <= end",
        ),
        (
            "0,0,1,0.75,0,0.25,0,1,1",
            1,
            4,
            "start <= control1 <= control2 <= end",
        ),
        (
            "0,0,1,0.25,0,1.25,0,1,1",
            1,
            4,
            "start <= control1 <= control2 <= end",
        ),
    ];

    for (segments, segment_count, point_count, expected) in cases {
        let source = raw_motion_source("2", "60", segments, segment_count, point_count);
        assert_invalid_contains(&source, expected);
    }
}

#[test]
fn samples_segment_boundaries_with_existing_semantics() {
    let segments = vec![
        0.0, 10.0, 0.0, 1.0, 20.0, 2.0, 2.0, 30.0, 3.0, 3.0, 40.0, 0.0, 4.0, 50.0,
    ];
    let source = motion_source(vec![curve("ParamAngleX", segments)], [1, 4, 5]);
    let motion = Motion3::from_json_str(&source).expect("固定边界动作应可解析");
    let curve = motion.curves().first().expect("动作应包含一条曲线");

    for (time, expected) in [
        (-1.0, 10.0),
        (0.0, 10.0),
        (0.5, 15.0),
        (1.0, 20.0),
        (1.999, 20.0),
        (2.0, 40.0),
        (2.5, 40.0),
        (3.0, 40.0),
        (3.5, 45.0),
        (4.0, 50.0),
        (5.0, 50.0),
    ] {
        assert_sample(curve, time, expected);
    }
}

#[test]
fn accepts_and_samples_a_large_curve_at_the_limit() {
    let segments = linear_segments(MAX_SEGMENTS_PER_CURVE);
    let source = motion_source(
        vec![curve("ParamAngleX", segments)],
        [1, MAX_SEGMENTS_PER_CURVE, MAX_SEGMENTS_PER_CURVE + 1],
    );
    let motion = Motion3::from_json_str(&source).expect("上限内的大曲线应可解析");
    let curve = motion.curves().first().expect("动作应包含一条曲线");

    assert_eq!(curve.segments().len(), MAX_SEGMENTS_PER_CURVE);
    assert_sample(curve, MAX_SEGMENTS_PER_CURVE as f32 - 0.5, 16_383.5);
    assert_sample(
        curve,
        MAX_SEGMENTS_PER_CURVE as f32,
        MAX_SEGMENTS_PER_CURVE as f32,
    );
}

#[test]
fn players_share_parsed_motion_but_keep_independent_playback_state() {
    let motion = Arc::new(
        Motion3::from_json_str(&raw_motion_source("1", "60", "0,0,0,1,1", 1, 2))
            .expect("固定共享动作应可解析"),
    );
    let mut first = MotionPlayer::with_looping(Arc::clone(&motion), false);
    let second = MotionPlayer::with_looping(Arc::clone(&motion), false);

    assert_eq!(
        first.parsed_identity_for_test(),
        second.parsed_identity_for_test(),
        "播放器必须共享不可变解析曲线"
    );
    first.tick(0.5);
    first.set_weight(0.25);
    assert_eq!(first.time(), 0.5);
    assert_eq!(second.time(), 0.0);
    assert_eq!(first.weight(), 0.25);
    assert_eq!(second.weight(), 1.0);
}

fn motion_source(curves: Vec<serde_json::Value>, reported: [usize; 3]) -> String {
    serde_json::json!({
        "Version": 3,
        "Meta": {
            "Duration": 100_000.0,
            "Fps": 60.0,
            "Loop": false,
            "AreBeziersRestricted": false,
            "CurveCount": reported[0],
            "TotalSegmentCount": reported[1],
            "TotalPointCount": reported[2],
            "UserDataCount": 0,
            "TotalUserDataSize": 0
        },
        "Curves": curves
    })
    .to_string()
}

fn raw_motion_source(
    duration: &str,
    fps: &str,
    segments: &str,
    segment_count: usize,
    point_count: usize,
) -> String {
    raw_motion_source_with_version(3, duration, fps, segments, segment_count, point_count)
}

fn raw_motion_source_with_version(
    version: u32,
    duration: &str,
    fps: &str,
    segments: &str,
    segment_count: usize,
    point_count: usize,
) -> String {
    format!(
        r#"{{"Version":{version},"Meta":{{"Duration":{duration},"Fps":{fps},"Loop":false,"AreBeziersRestricted":false,"CurveCount":1,"TotalSegmentCount":{segment_count},"TotalPointCount":{point_count},"UserDataCount":0,"TotalUserDataSize":0}},"Curves":[{{"Target":"Parameter","Id":"ParamAngleX","Segments":[{segments}]}}]}}"#
    )
}

fn raw_motion_source_with_curve_field(field: &str, value: &str) -> String {
    format!(
        r#"{{"Version":3,"Meta":{{"Duration":1,"Fps":60,"Loop":false,"AreBeziersRestricted":false,"CurveCount":1,"TotalSegmentCount":1,"TotalPointCount":2,"UserDataCount":0,"TotalUserDataSize":0}},"Curves":[{{"Target":"Parameter","Id":"ParamAngleX","Segments":[0,0,0,1,1],"{field}":{value}}}]}}"#
    )
}

fn curve(id: &str, segments: Vec<f32>) -> serde_json::Value {
    serde_json::json!({
        "Target": "Parameter",
        "Id": id,
        "Segments": segments
    })
}

fn linear_segments(segment_count: usize) -> Vec<f32> {
    let mut segments = Vec::with_capacity(2 + segment_count * 3);
    segments.extend([0.0, 0.0]);
    for index in 1..=segment_count {
        let end = index as f32;
        segments.extend([0.0, end, end]);
    }
    segments
}

fn bezier_segments(segment_count: usize) -> Vec<f32> {
    let mut segments = Vec::with_capacity(2 + segment_count * 7);
    segments.extend([0.0, 0.0]);
    for index in 0..segment_count {
        let start = index as f32;
        let end = start + 1.0;
        segments.extend([1.0, start + 0.25, 0.0, start + 0.75, 0.0, end, 0.0]);
    }
    segments
}

fn assert_invalid_contains(source: &str, expected: &str) {
    let error = Motion3::from_json_str(source).expect_err("恶意 motion3 输入必须被拒绝");
    match error {
        Error::InvalidJson { format, message } => {
            assert_eq!(format, "motion3.json");
            assert!(
                message.contains(expected),
                "错误 {message:?} 应包含 {expected:?}"
            );
        }
        other => panic!("预期 InvalidJson，实际为 {other:?}"),
    }
}

fn assert_sample(curve: &MotionCurve, time: f32, expected: f32) {
    let sampled = curve.sample(time).expect("已解析曲线必须可采样");
    assert!(
        (sampled - expected).abs() <= 0.0001,
        "时间 {time} 的采样值 {sampled} 应为 {expected}"
    );
}
