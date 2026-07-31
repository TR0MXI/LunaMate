use std::hint::black_box;

use allocation_counter::measure;
use mocari::moc3::DeformerCompositionBenchmark;

const ITERATIONS: usize = 32;
const CASES: [(&str, usize, usize, usize); 2] = [
    ("8-deformers-8x8", 8, 8, 8),
    ("32-deformers-16x16", 32, 16, 16),
];

fn main() {
    // 排除 allocation-counter 自身的线程局部初始化。
    let _ = measure(|| {});

    for (name, chain_length, cols, rows) in CASES {
        let mut fixture = fixture(chain_length, cols, rows);
        black_box(fixture.compose());
        let allocations = measure(|| {
            for _ in 0..ITERATIONS {
                black_box(fixture.compose());
            }
        });

        assert_eq!(allocations.count_total, 0, "{name} 仍发生堆分配");
        assert_eq!(allocations.bytes_total, 0, "{name} 仍分配堆字节");
        assert_eq!(allocations.count_current, 0);
        assert_eq!(allocations.bytes_current, 0);

        println!("{name}: 0 allocations/frame, 0 bytes/frame");
    }
}

fn fixture(chain_length: usize, cols: usize, rows: usize) -> DeformerCompositionBenchmark {
    match DeformerCompositionBenchmark::new(chain_length, cols, rows) {
        Some(fixture) => fixture,
        None => panic!("固定基准参数应生成有效的 warp 链"),
    }
}
