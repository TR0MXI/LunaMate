use std::{sync::mpsc, thread, time::Duration};

use serde_json::{Value, json};

use crate::{Error, core::PhysicsRuntime, json::Physics3};

const MAX_PHYSICS_SETTINGS: usize = 256;
const MAX_INPUTS_PER_SETTING: usize = 256;
const MAX_OUTPUTS_PER_SETTING: usize = 256;
const MAX_VERTICES_PER_SETTING: usize = 256;
const MAX_TOTAL_ITEMS: usize = 4_096;

#[test]
fn accepts_representative_cubism_physics_fixture() {
    let source = normal_physics_document().to_string();
    let physics = Physics3::from_json_str(&source).expect("正常 Cubism Physics fixture 应可解析");

    assert_eq!(physics.meta().physics_setting_count(), 1);
    assert_eq!(physics.meta().total_input_count(), 2);
    assert_eq!(physics.meta().total_output_count(), 2);
    assert_eq!(physics.meta().vertex_count(), 3);
    assert_eq!(physics.settings()[0].outputs()[0].scale(), -2.5);
    assert_eq!(
        physics.settings()[0].normalization().angle().minimum(),
        -45.0
    );

    let parameter_ids = [
        "ParamAngleX".to_owned(),
        "ParamAngleZ".to_owned(),
        "ParamHairX".to_owned(),
        "ParamHairAngle".to_owned(),
    ];
    let mut runtime = PhysicsRuntime::new(&physics, &parameter_ids);
    let mut values = [15.0, -10.0, 0.0, 0.0];
    let minimums = [-30.0, -30.0, -100.0, -100.0];
    let maximums = [30.0, 30.0, 100.0, 100.0];
    let defaults = [0.0; 4];
    runtime.stabilize(&mut values, &minimums, &maximums, &defaults);
    runtime.evaluate(&mut values, &minimums, &maximums, &defaults, 1.0 / 60.0);

    assert!(values.iter().all(|value| value.is_finite()));
}

#[test]
fn accepts_each_per_setting_count_at_its_limit() {
    let settings = vec![physics_setting(
        0,
        MAX_INPUTS_PER_SETTING,
        MAX_OUTPUTS_PER_SETTING,
        MAX_VERTICES_PER_SETTING,
    )];
    let source = physics_document(settings).to_string();

    let physics = Physics3::from_json_str(&source).expect("单组 Physics 数量预算边界应可解析");

    assert_eq!(physics.settings()[0].inputs().len(), MAX_INPUTS_PER_SETTING);
    assert_eq!(
        physics.settings()[0].outputs().len(),
        MAX_OUTPUTS_PER_SETTING
    );
    assert_eq!(
        physics.settings()[0].vertices().len(),
        MAX_VERTICES_PER_SETTING
    );
}

#[test]
fn accepts_setting_and_total_count_limits() {
    let items_per_setting = MAX_TOTAL_ITEMS / MAX_PHYSICS_SETTINGS;
    let settings = (0..MAX_PHYSICS_SETTINGS)
        .map(|index| {
            physics_setting(
                index,
                items_per_setting,
                items_per_setting,
                items_per_setting,
            )
        })
        .collect();
    let source = physics_document(settings).to_string();

    let physics = Physics3::from_json_str(&source).expect("Physics 总数量预算边界应可解析");

    assert_eq!(physics.settings().len(), MAX_PHYSICS_SETTINGS);
    assert_eq!(physics.meta().total_input_count(), MAX_TOTAL_ITEMS as u32);
    assert_eq!(physics.meta().total_output_count(), MAX_TOTAL_ITEMS as u32);
    assert_eq!(physics.meta().vertex_count(), MAX_TOTAL_ITEMS as u32);
}

#[test]
fn rejects_decoded_counts_above_per_setting_limits() {
    assert_invalid_contains(
        &physics_document(vec![physics_setting(0, MAX_INPUTS_PER_SETTING + 1, 0, 0)]).to_string(),
        "setting 0 input count 257 exceeds per-setting limit 256",
    );
    assert_invalid_contains(
        &physics_document(vec![physics_setting(0, 0, MAX_OUTPUTS_PER_SETTING + 1, 2)]).to_string(),
        "setting 0 output count 257 exceeds per-setting limit 256",
    );
    assert_invalid_contains(
        &physics_document(vec![physics_setting(0, 0, 0, MAX_VERTICES_PER_SETTING + 1)]).to_string(),
        "setting 0 vertex count 257 exceeds per-setting limit 256",
    );
}

#[test]
fn rejects_decoded_counts_above_total_limits() {
    let count_per_setting = 241;
    assert_invalid_contains(
        &physics_document(
            (0..17)
                .map(|index| physics_setting(index, count_per_setting, 0, 0))
                .collect(),
        )
        .to_string(),
        "total input count 4097 exceeds limit 4096",
    );
    assert_invalid_contains(
        &physics_document(
            (0..17)
                .map(|index| physics_setting(index, 0, count_per_setting, 2))
                .collect(),
        )
        .to_string(),
        "total output count 4097 exceeds limit 4096",
    );
    assert_invalid_contains(
        &physics_document(
            (0..17)
                .map(|index| physics_setting(index, 0, 0, count_per_setting))
                .collect(),
        )
        .to_string(),
        "total vertex count 4097 exceeds limit 4096",
    );
}

#[test]
fn rejects_actual_setting_count_over_limit_even_when_meta_claims_zero() {
    let settings = (0..=MAX_PHYSICS_SETTINGS)
        .map(|index| physics_setting(index, 0, 0, 0))
        .collect();
    let mut document = physics_document(settings);
    set_pointer(&mut document, "/Meta/PhysicsSettingCount", json!(0));

    assert_invalid_contains(
        &document.to_string(),
        "physics setting count 257 exceeds limit 256",
    );
}

#[test]
fn rejects_metadata_counts_that_do_not_match_decoded_data() {
    for (pointer, expected) in [
        (
            "/Meta/PhysicsSettingCount",
            "reported physics setting count 0 does not match actual count 1",
        ),
        (
            "/Meta/TotalInputCount",
            "reported total input count 0 does not match actual count 2",
        ),
        (
            "/Meta/TotalOutputCount",
            "reported total output count 0 does not match actual count 2",
        ),
        (
            "/Meta/VertexCount",
            "reported vertex count 0 does not match actual count 3",
        ),
    ] {
        let mut document = normal_physics_document();
        set_pointer(&mut document, pointer, json!(0));
        assert_invalid_contains(&document.to_string(), expected);
    }
}

#[test]
fn rejects_numbers_that_overflow_to_infinity() {
    for (pointer, expected) in [
        ("/Meta/Fps", "physics FPS must be finite"),
        (
            "/Meta/EffectiveForces/Gravity/X",
            "gravity X must be finite",
        ),
        ("/Meta/EffectiveForces/Wind/Y", "wind Y must be finite"),
        (
            "/PhysicsSettings/0/Input/0/Weight",
            "setting 0 input 0 weight must be finite",
        ),
        (
            "/PhysicsSettings/0/Output/0/Scale",
            "setting 0 output 0 scale must be finite",
        ),
        (
            "/PhysicsSettings/0/Output/0/Weight",
            "setting 0 output 0 weight must be finite",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Mobility",
            "setting 0 vertex 1 mobility must be finite",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Delay",
            "setting 0 vertex 1 delay must be finite",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Acceleration",
            "setting 0 vertex 1 acceleration must be finite",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Radius",
            "setting 0 vertex 1 radius must be finite",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Position/X",
            "setting 0 vertex 1 position X must be finite",
        ),
        (
            "/PhysicsSettings/0/Normalization/Position/Minimum",
            "setting 0 position normalization minimum must be finite",
        ),
        (
            "/PhysicsSettings/0/Normalization/Position/Default",
            "setting 0 position normalization default must be finite",
        ),
        (
            "/PhysicsSettings/0/Normalization/Position/Maximum",
            "setting 0 position normalization maximum must be finite",
        ),
        (
            "/PhysicsSettings/0/Normalization/Angle/Minimum",
            "setting 0 angle normalization minimum must be finite",
        ),
        (
            "/PhysicsSettings/0/Normalization/Angle/Default",
            "setting 0 angle normalization default must be finite",
        ),
        (
            "/PhysicsSettings/0/Normalization/Angle/Maximum",
            "setting 0 angle normalization maximum must be finite",
        ),
    ] {
        let mut document = normal_physics_document();
        set_pointer(&mut document, pointer, overflowing_f32());
        assert_invalid_contains(&document.to_string(), expected);
    }
}

#[test]
fn rejects_invalid_weights_and_dynamics_values() {
    for (pointer, value, expected) in [
        (
            "/PhysicsSettings/0/Input/0/Weight",
            -0.1,
            "input 0 weight must be between 0 and 100",
        ),
        (
            "/PhysicsSettings/0/Input/0/Weight",
            100.1,
            "input 0 weight must be between 0 and 100",
        ),
        (
            "/PhysicsSettings/0/Output/0/Weight",
            -0.1,
            "output 0 weight must be between 0 and 100",
        ),
        (
            "/PhysicsSettings/0/Output/0/Weight",
            100.1,
            "output 0 weight must be between 0 and 100",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Mobility",
            -0.1,
            "vertex 1 mobility must be between 0 and 1000",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Delay",
            -0.1,
            "vertex 1 delay must be between 0 and 1000",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Delay",
            0.00001,
            "vertex 1 delay must be zero or at least 0.0001",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Delay",
            1000.1,
            "vertex 1 delay must be between 0 and 1000",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Acceleration",
            -0.1,
            "vertex 1 acceleration must be between 0 and 1000",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Acceleration",
            1000.1,
            "vertex 1 acceleration must be between 0 and 1000",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Radius",
            -0.1,
            "vertex 1 radius must be between 0 and 1000000",
        ),
        (
            "/PhysicsSettings/0/Vertices/1/Radius",
            1_000_001.0,
            "vertex 1 radius must be between 0 and 1000000",
        ),
    ] {
        let mut document = normal_physics_document();
        set_pointer(&mut document, pointer, json!(value));
        assert_invalid_contains(&document.to_string(), expected);
    }
}

#[test]
fn accepts_numeric_limits_without_non_finite_runtime_state() {
    let mut document = normal_physics_document();
    for (pointer, value) in [
        ("/Meta/Fps", 240.0),
        ("/Meta/EffectiveForces/Gravity/X", -1_000_000.0),
        ("/Meta/EffectiveForces/Wind/Y", 1_000_000.0),
        ("/PhysicsSettings/0/Input/0/Weight", 100.0),
        ("/PhysicsSettings/0/Output/0/Scale", -1_000_000.0),
        ("/PhysicsSettings/0/Vertices/1/Mobility", 1_000.0),
        ("/PhysicsSettings/0/Vertices/1/Delay", 0.0001),
        ("/PhysicsSettings/0/Vertices/1/Acceleration", 1_000.0),
        ("/PhysicsSettings/0/Vertices/1/Radius", 1_000_000.0),
        ("/PhysicsSettings/0/Vertices/1/Position/X", -1_000_000.0),
        ("/PhysicsSettings/0/Vertices/2/Mobility", 1_000.0),
        ("/PhysicsSettings/0/Vertices/2/Delay", 1_000.0),
        ("/PhysicsSettings/0/Vertices/2/Acceleration", 1_000.0),
        ("/PhysicsSettings/0/Vertices/2/Radius", 1_000_000.0),
        ("/PhysicsSettings/0/Vertices/2/Position/Y", 1_000_000.0),
        (
            "/PhysicsSettings/0/Normalization/Position/Minimum",
            -1_000_000.0,
        ),
        (
            "/PhysicsSettings/0/Normalization/Position/Maximum",
            1_000_000.0,
        ),
        (
            "/PhysicsSettings/0/Normalization/Angle/Minimum",
            -1_000_000.0,
        ),
        (
            "/PhysicsSettings/0/Normalization/Angle/Maximum",
            1_000_000.0,
        ),
    ] {
        set_pointer(&mut document, pointer, json!(value));
    }
    let physics =
        Physics3::from_json_str(&document.to_string()).expect("Physics 数值预算边界应可解析");
    let parameter_ids = [
        "ParamAngleX".to_owned(),
        "ParamAngleZ".to_owned(),
        "ParamHairX".to_owned(),
        "ParamHairAngle".to_owned(),
    ];
    let mut runtime = PhysicsRuntime::new(&physics, &parameter_ids);
    let minimums = [-30.0, -30.0, -100.0, -100.0];
    let maximums = [30.0, 30.0, 100.0, 100.0];
    let defaults = [0.0; 4];
    let mut values = defaults;
    runtime.stabilize(&mut values, &minimums, &maximums, &defaults);
    values[0] = maximums[0];
    values[1] = minimums[1];

    runtime.evaluate(&mut values, &minimums, &maximums, &defaults, 1.0 / 240.0);

    assert!(values.iter().all(|value| value.is_finite()));
}

#[test]
fn rejects_excessive_magnitudes_but_accepts_negative_output_scale() {
    for value in [-1_000_001.0, 1_000_001.0] {
        let mut document = normal_physics_document();
        set_pointer(
            &mut document,
            "/PhysicsSettings/0/Output/0/Scale",
            json!(value),
        );
        assert_invalid_contains(
            &document.to_string(),
            "output 0 scale magnitude must not exceed 1000000",
        );
    }

    let physics = Physics3::from_json_str(&normal_physics_document().to_string())
        .expect("预算内的负 output scale 应可解析");
    assert_eq!(physics.settings()[0].outputs()[0].scale(), -2.5);
}

#[test]
fn rejects_invalid_normalization_ranges() {
    let mut collapsed = normal_physics_document();
    set_pointer(
        &mut collapsed,
        "/PhysicsSettings/0/Normalization/Position/Minimum",
        json!(10.0),
    );
    assert_invalid_contains(
        &collapsed.to_string(),
        "position normalization minimum must be less than maximum",
    );

    let mut reversed = normal_physics_document();
    set_pointer(
        &mut reversed,
        "/PhysicsSettings/0/Normalization/Angle/Minimum",
        json!(31.0),
    );
    assert_invalid_contains(
        &reversed.to_string(),
        "angle normalization minimum must be less than maximum",
    );

    let mut default_outside = normal_physics_document();
    set_pointer(
        &mut default_outside,
        "/PhysicsSettings/0/Normalization/Angle/Default",
        json!(31.0),
    );
    assert_invalid_contains(
        &default_outside.to_string(),
        "angle normalization default must be between minimum and maximum",
    );
}

#[test]
fn rejects_output_vertex_without_a_current_and_parent_vertex() {
    for vertex_index in [0_u32, 3, u32::MAX] {
        let mut document = normal_physics_document();
        set_pointer(
            &mut document,
            "/PhysicsSettings/0/Output/0/VertexIndex",
            json!(vertex_index),
        );
        assert_invalid_contains(&document.to_string(), "vertex index");
        assert_invalid_contains(&document.to_string(), "out of range for 3 vertices");
    }

    let source = physics_document(vec![physics_setting(0, 0, 1, 1)]).to_string();
    assert_invalid_contains(&source, "out of range for 1 vertices");
}

#[test]
fn accepts_zero_vertex_dynamics_that_runtime_handles_explicitly() {
    let mut document = normal_physics_document();
    for field in ["Mobility", "Delay", "Acceleration", "Radius"] {
        set_pointer(
            &mut document,
            &format!("/PhysicsSettings/0/Vertices/1/{field}"),
            json!(0.0),
        );
    }

    Physics3::from_json_str(&document.to_string())
        .expect("零 delay 和零长度粒子已有显式运行时保护，应保持可解析");
}

#[test]
fn rejects_negative_physics_fps() {
    let mut document = normal_physics_document();
    set_pointer(&mut document, "/Meta/Fps", json!(-1.0));

    assert_invalid_contains(&document.to_string(), "physics FPS must be non-negative");
}

#[test]
fn extreme_fixed_fps_is_clamped_and_evaluation_finishes() {
    let physics = empty_physics(1.0e30);
    let mut runtime = PhysicsRuntime::new(&physics, &[]);
    let (completed_tx, completed_rx) = mpsc::channel();

    let worker = thread::spawn(move || {
        runtime.evaluate(&mut [], &[], &[], &[], 1.0);
        let _ = completed_tx.send(runtime.fixed_fps_for_test());
    });

    let fixed_fps = completed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("极端 Physics FPS 的单帧求值必须在严格步骤预算内结束");
    worker.join().expect("Physics 求值测试线程不应 panic");
    assert_eq!(fixed_fps, 240.0);
}

#[test]
fn catch_up_budget_discards_excess_accumulated_time() {
    let physics = empty_physics(240.0);
    let mut runtime = PhysicsRuntime::new(&physics, &[]);

    runtime.evaluate(&mut [], &[], &[], &[], 5.0);

    assert_eq!(runtime.remaining_time_for_test(), 0.0);
}

#[test]
fn zero_fixed_fps_uses_the_frame_delta() {
    let physics = empty_physics(0.0);
    let mut runtime = PhysicsRuntime::new(&physics, &[]);

    runtime.evaluate(&mut [], &[], &[], &[], 0.25);

    assert_eq!(runtime.fixed_fps_for_test(), 0.0);
    assert_eq!(runtime.remaining_time_for_test(), 0.0);
}

fn empty_physics(fps: f32) -> Physics3 {
    let source = json!({
        "Version": 3,
        "Meta": {
            "PhysicsSettingCount": 0,
            "TotalInputCount": 0,
            "TotalOutputCount": 0,
            "VertexCount": 0,
            "Fps": fps,
            "EffectiveForces": {
                "Gravity": { "X": 0.0, "Y": -1.0 },
                "Wind": { "X": 0.0, "Y": 0.0 }
            },
            "PhysicsDictionary": []
        },
        "PhysicsSettings": []
    });
    Physics3::from_json_str(&source.to_string()).expect("固定空 Physics JSON 应可解析")
}

fn normal_physics_document() -> Value {
    json!({
        "Version": 3,
        "Meta": {
            "PhysicsSettingCount": 1,
            "TotalInputCount": 2,
            "TotalOutputCount": 2,
            "VertexCount": 3,
            "Fps": 60.0,
            "EffectiveForces": {
                "Gravity": { "X": 0.0, "Y": -1.0 },
                "Wind": { "X": -0.25, "Y": 0.5 }
            },
            "PhysicsDictionary": [
                { "Id": "PhysicsSetting1", "Name": "Hair" }
            ]
        },
        "PhysicsSettings": [
            {
                "Id": "PhysicsSetting1",
                "Input": [
                    {
                        "Source": { "Target": "Parameter", "Id": "ParamAngleX" },
                        "Weight": 60.0,
                        "Type": "X",
                        "Reflect": false
                    },
                    {
                        "Source": { "Target": "Parameter", "Id": "ParamAngleZ" },
                        "Weight": 40.0,
                        "Type": "Angle",
                        "Reflect": true
                    }
                ],
                "Output": [
                    {
                        "Destination": { "Target": "Parameter", "Id": "ParamHairX" },
                        "VertexIndex": 1,
                        "Scale": -2.5,
                        "Weight": 100.0,
                        "Type": "X",
                        "Reflect": false
                    },
                    {
                        "Destination": { "Target": "Parameter", "Id": "ParamHairAngle" },
                        "VertexIndex": 2,
                        "Scale": 30.0,
                        "Weight": 75.0,
                        "Type": "Angle",
                        "Reflect": true
                    }
                ],
                "Vertices": [
                    {
                        "Mobility": 1.0,
                        "Delay": 1.0,
                        "Acceleration": 1.0,
                        "Radius": 0.0,
                        "Position": { "X": 0.0, "Y": 0.0 }
                    },
                    {
                        "Mobility": 0.9,
                        "Delay": 0.8,
                        "Acceleration": 1.5,
                        "Radius": 10.0,
                        "Position": { "X": 0.0, "Y": 10.0 }
                    },
                    {
                        "Mobility": 0.85,
                        "Delay": 1.3,
                        "Acceleration": 10.0,
                        "Radius": 8.0,
                        "Position": { "X": -1.0, "Y": 18.0 }
                    }
                ],
                "Normalization": {
                    "Position": { "Minimum": -10.0, "Default": 0.0, "Maximum": 10.0 },
                    "Angle": { "Minimum": -45.0, "Default": -5.0, "Maximum": 30.0 }
                }
            }
        ]
    })
}

fn physics_document(settings: Vec<Value>) -> Value {
    let setting_count = settings.len();
    let input_count = total_array_len(&settings, "Input");
    let output_count = total_array_len(&settings, "Output");
    let vertex_count = total_array_len(&settings, "Vertices");

    json!({
        "Version": 3,
        "Meta": {
            "PhysicsSettingCount": setting_count,
            "TotalInputCount": input_count,
            "TotalOutputCount": output_count,
            "VertexCount": vertex_count,
            "Fps": 60.0,
            "EffectiveForces": {
                "Gravity": { "X": 0.0, "Y": -1.0 },
                "Wind": { "X": 0.0, "Y": 0.0 }
            },
            "PhysicsDictionary": []
        },
        "PhysicsSettings": settings
    })
}

fn total_array_len(settings: &[Value], field: &str) -> usize {
    settings
        .iter()
        .map(|setting| {
            setting[field]
                .as_array()
                .expect("测试 Physics setting 字段必须是数组")
                .len()
        })
        .sum()
}

fn physics_setting(
    setting_index: usize,
    input_count: usize,
    output_count: usize,
    vertex_count: usize,
) -> Value {
    let inputs = (0..input_count)
        .map(|input_index| {
            json!({
                "Source": {
                    "Target": "Parameter",
                    "Id": format!("ParamInput{setting_index}_{input_index}")
                },
                "Weight": 100.0,
                "Type": "X",
                "Reflect": false
            })
        })
        .collect::<Vec<_>>();
    let outputs = (0..output_count)
        .map(|output_index| {
            let vertex_index = if vertex_count > 1 {
                output_index % (vertex_count - 1) + 1
            } else {
                1
            };
            json!({
                "Destination": {
                    "Target": "Parameter",
                    "Id": format!("ParamOutput{setting_index}_{output_index}")
                },
                "VertexIndex": vertex_index,
                "Scale": 1.0,
                "Weight": 100.0,
                "Type": "Angle",
                "Reflect": false
            })
        })
        .collect::<Vec<_>>();
    let vertices = (0..vertex_count)
        .map(|vertex_index| {
            json!({
                "Mobility": 1.0,
                "Delay": 1.0,
                "Acceleration": 1.0,
                "Radius": if vertex_index == 0 { 0.0 } else { 1.0 },
                "Position": { "X": 0.0, "Y": vertex_index as f32 }
            })
        })
        .collect::<Vec<_>>();

    json!({
        "Id": format!("PhysicsSetting{setting_index}"),
        "Input": inputs,
        "Output": outputs,
        "Vertices": vertices,
        "Normalization": {
            "Position": { "Minimum": -10.0, "Default": 0.0, "Maximum": 10.0 },
            "Angle": { "Minimum": -30.0, "Default": 0.0, "Maximum": 30.0 }
        }
    })
}

fn overflowing_f32() -> Value {
    serde_json::from_str("3.5e38").expect("3.5e38 应是有效 JSON number")
}

fn set_pointer(document: &mut Value, pointer: &str, value: Value) {
    *document
        .pointer_mut(pointer)
        .expect("测试 JSON pointer 必须指向固定 fixture 字段") = value;
}

fn assert_invalid_contains(source: &str, expected: &str) {
    let error = Physics3::from_json_str(source).expect_err("恶意 physics3 输入必须被拒绝");
    match error {
        Error::InvalidJson { format, message } => {
            assert_eq!(format, "physics3.json");
            assert!(
                message.contains(expected),
                "错误 {message:?} 应包含 {expected:?}"
            );
        }
        other => panic!("预期 InvalidJson，实际为 {other:?}"),
    }
}
