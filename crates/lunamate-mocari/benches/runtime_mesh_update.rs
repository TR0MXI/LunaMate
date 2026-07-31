use std::{env, hint::black_box, path::PathBuf};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mimalloc::MiMalloc;
use mocari::{ModelRuntime, assets::load_model_runtime};

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;

const MODEL_ENV: &str = "LUNAMATE_BENCH_MODEL";
const STATES_ENV: &str = "LUNAMATE_BENCH_STATES";
const DRIVE_PARAMETER_ENV: &str = "LUNAMATE_BENCH_DRIVE_PARAMETER";

struct BenchState {
    name: String,
    parameters: Vec<(String, f32)>,
}

struct MeshUpdateFixture {
    runtime: ModelRuntime,
    drive_parameter_index: usize,
    high: bool,
    unpruned: bool,
}

impl MeshUpdateFixture {
    fn new(base: &ModelRuntime, state: &BenchState, drive_parameter: &str, unpruned: bool) -> Self {
        let mut runtime = base.clone();
        for (id, value) in &state.parameters {
            assert!(
                runtime.set_parameter(id, *value),
                "基准状态 {} 引用了模型中不存在的参数 {id}",
                state.name
            );
        }
        assert!(runtime.update_meshes().is_some(), "基准状态初始化失败");
        let drive_parameter_index = runtime
            .parameter_index(drive_parameter)
            .unwrap_or_else(|| panic!("模型中不存在基准驱动参数 {drive_parameter}"));
        verify_against_unpruned(&mut runtime, drive_parameter_index, &state.name);
        Self {
            runtime,
            drive_parameter_index,
            high: false,
            unpruned,
        }
    }

    fn update(&mut self) -> usize {
        self.high = !self.high;
        let value = if self.high { 0.55 } else { 0.45 };
        assert!(
            self.runtime
                .set_parameter_normalized_by_index(self.drive_parameter_index, value),
            "已解析的基准驱动参数索引应始终有效"
        );
        let updated = if self.unpruned {
            self.runtime.update_meshes_unpruned_for_benchmark()
        } else {
            self.runtime.update_meshes()
        };
        assert!(updated.is_some(), "基准帧 mesh update 失败");
        self.runtime.meshes().len()
    }
}

fn verify_against_unpruned(runtime: &mut ModelRuntime, drive_parameter_index: usize, state: &str) {
    let mut reference = runtime.clone();
    for value in [0.45, 0.55, 0.5] {
        assert!(
            runtime.set_parameter_normalized_by_index(drive_parameter_index, value)
                && reference.set_parameter_normalized_by_index(drive_parameter_index, value),
            "基准驱动参数索引应在两个 runtime 中保持有效"
        );
        assert!(runtime.update_meshes().is_some(), "优化路径更新失败");
        assert!(
            reference.update_meshes_unpruned_for_benchmark().is_some(),
            "参考路径更新失败"
        );
        assert_render_equivalent(runtime, &reference, state, value);
    }
}

fn assert_render_equivalent(
    optimized: &ModelRuntime,
    reference: &ModelRuntime,
    state: &str,
    value: f32,
) {
    let optimized_meshes = optimized.meshes();
    let reference_meshes = reference.meshes();
    assert_eq!(optimized_meshes.len(), reference_meshes.len());
    let mut geometry_consumed = reference_meshes
        .iter()
        .map(|mesh| mesh.opacity() > 0.0)
        .collect::<Vec<_>>();
    for mesh in reference_meshes.iter().filter(|mesh| mesh.opacity() > 0.0) {
        for &mask_index in mesh.masks() {
            let mask_index = usize::try_from(mask_index)
                .unwrap_or_else(|_| panic!("参考模型包含负蒙版索引 {mask_index}"));
            *geometry_consumed
                .get_mut(mask_index)
                .unwrap_or_else(|| panic!("参考模型包含越界蒙版索引 {mask_index}")) = true;
        }
    }

    for (index, (actual, expected)) in optimized_meshes.iter().zip(reference_meshes).enumerate() {
        assert_eq!(
            actual.opacity(),
            expected.opacity(),
            "{state}@{value}: opacity {index}"
        );
        assert_eq!(
            actual.draw_order(),
            expected.draw_order(),
            "{state}@{value}: draw order {index}"
        );
        assert_eq!(
            actual.render_order(),
            expected.render_order(),
            "{state}@{value}: render order {index}"
        );
        assert_eq!(
            actual.multiply_color(),
            expected.multiply_color(),
            "{state}@{value}: multiply color {index}"
        );
        assert_eq!(
            actual.screen_color(),
            expected.screen_color(),
            "{state}@{value}: screen color {index}"
        );
        if geometry_consumed[index] {
            assert_eq!(actual, expected, "{state}@{value}: consumed mesh {index}");
        }
    }
}

fn runtime_mesh_update(criterion: &mut Criterion) {
    let model_path = env::var_os(MODEL_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("必须通过 {MODEL_ENV} 指定用户模型的 model3.json 路径"));
    let states = parse_states(&env::var(STATES_ENV).unwrap_or_else(|_| "default".to_owned()));
    let drive_parameter =
        env::var(DRIVE_PARAMETER_ENV).unwrap_or_else(|_| "ParamAngleX".to_owned());
    let loaded = load_model_runtime(&model_path)
        .unwrap_or_else(|error| panic!("无法加载基准模型 {}：{error}", model_path.display()));
    let base = loaded.runtime().clone();
    drop(loaded);
    verify_state_transitions(&base, &states, &drive_parameter);

    let mut group = criterion.benchmark_group("mocari/runtime-mesh-update");
    for state in states {
        let mut optimized = MeshUpdateFixture::new(&base, &state, &drive_parameter, false);
        group.throughput(Throughput::Elements(
            u64::try_from(optimized.runtime.meshes().len()).unwrap_or(u64::MAX),
        ));
        black_box(optimized.update());
        group.bench_function(BenchmarkId::new("optimized", &state.name), |bencher| {
            bencher.iter(|| black_box(optimized.update()));
        });

        let mut unpruned = MeshUpdateFixture::new(&base, &state, &drive_parameter, true);
        black_box(unpruned.update());
        group.bench_function(BenchmarkId::new("unpruned", &state.name), |bencher| {
            bencher.iter(|| black_box(unpruned.update()));
        });
    }
    group.finish();
}

fn verify_state_transitions(base: &ModelRuntime, states: &[BenchState], drive_parameter: &str) {
    let mut optimized = base.clone();
    let mut reference = base.clone();
    let drive_parameter_index = optimized
        .parameter_index(drive_parameter)
        .unwrap_or_else(|| panic!("模型中不存在基准驱动参数 {drive_parameter}"));
    for state in states {
        optimized.reset_parameters();
        reference.reset_parameters();
        for (id, value) in &state.parameters {
            assert!(
                optimized.set_parameter(id, *value) && reference.set_parameter(id, *value),
                "基准状态 {} 引用了模型中不存在的参数 {id}",
                state.name
            );
        }
        assert!(
            optimized.set_parameter_normalized_by_index(drive_parameter_index, 0.5)
                && reference.set_parameter_normalized_by_index(drive_parameter_index, 0.5)
        );
        assert!(optimized.update_meshes().is_some(), "状态切换优化路径失败");
        assert!(
            reference.update_meshes_unpruned_for_benchmark().is_some(),
            "状态切换参考路径失败"
        );
        assert_render_equivalent(&optimized, &reference, &state.name, 0.5);
    }
}

fn parse_states(value: &str) -> Vec<BenchState> {
    let states = value
        .split(';')
        .filter(|state| !state.trim().is_empty())
        .map(parse_state)
        .collect::<Vec<_>>();
    assert!(!states.is_empty(), "{STATES_ENV} 至少需要一个状态");
    states
}

fn parse_state(value: &str) -> BenchState {
    let (name, assignments) = value.split_once(':').unwrap_or((value, ""));
    assert!(!name.is_empty(), "基准状态名称不能为空");
    let parameters = assignments
        .split(',')
        .filter(|assignment| !assignment.is_empty())
        .map(|assignment| {
            let (id, value) = assignment
                .split_once('=')
                .unwrap_or_else(|| panic!("无效的基准参数赋值 {assignment}"));
            let value = value
                .parse::<f32>()
                .unwrap_or_else(|error| panic!("无效的基准参数值 {assignment}：{error}"));
            (id.to_owned(), value)
        })
        .collect();
    BenchState {
        name: name.to_owned(),
        parameters,
    }
}

criterion_group!(benches, runtime_mesh_update);
criterion_main!(benches);
