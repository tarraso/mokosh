//! Benchmarks for Compression (Zstd vs Lz4)
//!
//! Goal: Verify 72% reduction (Zstd) and 55% (Lz4)

#[cfg(feature = "compression")]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[cfg(feature = "compression")]
use mokosh_protocol::compression::{Compressor, Lz4Compressor, ZstdCompressor};

/// Helper: Create compressible test data
#[cfg(feature = "compression")]
fn create_test_data(size: usize, compressibility: f32) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);

    // Mix of repeated (compressible) and random (incompressible) data
    let compressible_size = (size as f32 * compressibility) as usize;
    let random_size = size - compressible_size;

    // Compressible part (repeated pattern)
    data.extend(std::iter::repeat_n(0xAB, compressible_size));

    // Random part (less compressible)
    data.extend((0..random_size).map(|i| (i % 256) as u8));

    data
}

/// Benchmark: Zstd compression
#[cfg(feature = "compression")]
fn bench_zstd_compress(c: &mut Criterion) {
    let mut group = c.benchmark_group("zstd_compress");

    for size in [1_000, 10_000, 100_000].iter() {
        let data = create_test_data(*size, 0.7); // 70% compressible
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            let compressor = ZstdCompressor::new();
            b.iter(|| {
                let result = compressor.compress(black_box(&data));
                black_box(result)
            });
        });
    }

    group.finish();
}

/// Benchmark: Zstd decompression
#[cfg(feature = "compression")]
fn bench_zstd_decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("zstd_decompress");

    for size in [1_000, 10_000, 100_000].iter() {
        let data = create_test_data(*size, 0.7);
        let compressor = ZstdCompressor::new();
        let compressed = compressor.compress(&data).unwrap();

        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let result = compressor.decompress(black_box(&compressed));
                black_box(result)
            });
        });
    }

    group.finish();
}

/// Benchmark: Lz4 compression
#[cfg(feature = "compression")]
fn bench_lz4_compress(c: &mut Criterion) {
    let mut group = c.benchmark_group("lz4_compress");

    for size in [1_000, 10_000, 100_000].iter() {
        let data = create_test_data(*size, 0.7);
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            let compressor = Lz4Compressor::new();
            b.iter(|| {
                let result = compressor.compress(black_box(&data));
                black_box(result)
            });
        });
    }

    group.finish();
}

/// Benchmark: Lz4 decompression
#[cfg(feature = "compression")]
fn bench_lz4_decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("lz4_decompress");

    for size in [1_000, 10_000, 100_000].iter() {
        let data = create_test_data(*size, 0.7);
        let compressor = Lz4Compressor::new();
        let compressed = compressor.compress(&data).unwrap();

        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let result = compressor.decompress(black_box(&compressed));
                black_box(result)
            });
        });
    }

    group.finish();
}

/// Benchmark: Compression ratio comparison
#[cfg(feature = "compression")]
fn bench_compression_ratio(c: &mut Criterion) {
    let mut group = c.benchmark_group("compression_ratio");

    for size in [10_000, 100_000].iter() {
        let data = create_test_data(*size, 0.7);

        // Zstd
        let zstd_compressor = ZstdCompressor::new();
        let zstd_compressed = zstd_compressor.compress(&data).unwrap();
        let zstd_ratio =
            ((data.len() - zstd_compressed.len()) as f64 / data.len() as f64) * 100.0;

        // Lz4
        let lz4_compressor = Lz4Compressor::new();
        let lz4_compressed = lz4_compressor.compress(&data).unwrap();
        let lz4_ratio = ((data.len() - lz4_compressed.len()) as f64 / data.len() as f64) * 100.0;

        println!(
            "\n[size={}] Original: {} bytes, Zstd: {} bytes ({:.1}%), Lz4: {} bytes ({:.1}%)",
            size,
            data.len(),
            zstd_compressed.len(),
            zstd_ratio,
            lz4_compressed.len(),
            lz4_ratio
        );

        group.bench_with_input(
            BenchmarkId::new("zstd_roundtrip", size),
            size,
            |b, _| {
                b.iter(|| {
                    let compressed = zstd_compressor.compress(black_box(&data)).unwrap();
                    let decompressed = zstd_compressor.decompress(&compressed).unwrap();
                    black_box(decompressed)
                });
            },
        );

        group.bench_with_input(BenchmarkId::new("lz4_roundtrip", size), size, |b, _| {
            b.iter(|| {
                let compressed = lz4_compressor.compress(black_box(&data)).unwrap();
                let decompressed = lz4_compressor.decompress(&compressed).unwrap();
                black_box(decompressed)
            });
        });
    }

    group.finish();
}

#[cfg(feature = "compression")]
criterion_group!(
    benches,
    bench_zstd_compress,
    bench_zstd_decompress,
    bench_lz4_compress,
    bench_lz4_decompress,
    bench_compression_ratio
);

#[cfg(feature = "compression")]
criterion_main!(benches);

#[cfg(not(feature = "compression"))]
fn main() {
    println!("Compression benchmarks require the 'compression' feature");
    println!("Run with: cargo bench --features compression --bench compression_bench");
}
