use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mimalloc::MiMalloc;
use mocari::moc3::DeformerCompositionBenchmark;

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;

const CASES: [(&str, usize, usize, usize); 2] = [
    ("8-deformers-8x8", 8, 8, 8),
    ("32-deformers-16x16", 32, 16, 16),
];

fn deformer_composition(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("mocari/deformer-composition");
    for (name, chain_length, cols, rows) in CASES {
        let mut fixture = fixture(chain_length, cols, rows);
        group.throughput(Throughput::Elements(
            u64::try_from(fixture.points_per_update()).unwrap_or(u64::MAX),
        ));
        // 计时前达到所有 Vec 的容量高水位，模拟应用稳态帧。
        black_box(fixture.compose());
        group.bench_function(BenchmarkId::new("steady-state", name), |bencher| {
            bencher.iter(|| black_box(fixture.compose()));
        });
    }
    group.finish();
}

fn fixture(chain_length: usize, cols: usize, rows: usize) -> DeformerCompositionBenchmark {
    match DeformerCompositionBenchmark::new(chain_length, cols, rows) {
        Some(fixture) => fixture,
        None => panic!("固定基准参数应生成有效的 warp 链"),
    }
}

criterion_group!(benches, deformer_composition);
criterion_main!(benches);
