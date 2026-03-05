//! Benchmark for compression algorithms (Zstd vs Lz4)

#[cfg(feature = "compression")]
use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};

#[cfg(feature = "compression")]
use mokosh_protocol::compression::{Compressor, ZstdCompressor, Lz4Compressor, NoCompressor};

#[cfg(feature = "compression")]
fn bench_zstd_compression(c: &mut Criterion) {
    let compressor = ZstdCompressor::new();

    // Highly compressible data (repeated pattern)
    let compressible_size = 10_000;
    let mut data = Vec::with_capacity(compressible_size);
    data.extend(std::iter::repeat_n(0xAB, compressible_size));

    let mut group = c.benchmark_group("zstd_compressible");
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_function("compress", |b| {
        b.iter(|| {
            let compressed = compressor.compress(black_box(&data)).unwrap();
            black_box(compressed)
        })
    });

    let compressed = compressor.compress(&data).unwrap();

    group.bench_function("decompress", |b| {
        b.iter(|| {
            let decompressed = compressor.decompress(black_box(&compressed)).unwrap();
            black_box(decompressed)
        })
    });

    group.finish();
}

#[cfg(feature = "compression")]
fn bench_lz4_compression(c: &mut Criterion) {
    let compressor = Lz4Compressor::new();

    // Highly compressible data (repeated pattern)
    let compressible_size = 10_000;
    let mut data = Vec::with_capacity(compressible_size);
    data.extend(std::iter::repeat_n(0xAB, compressible_size));

    let mut group = c.benchmark_group("lz4_compressible");
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_function("compress", |b| {
        b.iter(|| {
            let compressed = compressor.compress(black_box(&data)).unwrap();
            black_box(compressed)
        })
    });

    let compressed = compressor.compress(&data).unwrap();

    group.bench_function("decompress", |b| {
        b.iter(|| {
            let decompressed = compressor.decompress(black_box(&compressed)).unwrap();
            black_box(decompressed)
        })
    });

    group.finish();
}

#[cfg(feature = "compression")]
fn bench_zstd_random(c: &mut Criterion) {
    let compressor = ZstdCompressor::new();

    // Random data (incompressible)
    let data: Vec<u8> = (0..10_000).map(|i| (i * 17 + 13) as u8).collect();

    let mut group = c.benchmark_group("zstd_random");
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_function("compress", |b| {
        b.iter(|| {
            let compressed = compressor.compress(black_box(&data)).unwrap();
            black_box(compressed)
        })
    });

    let compressed = compressor.compress(&data).unwrap();

    group.bench_function("decompress", |b| {
        b.iter(|| {
            let decompressed = compressor.decompress(black_box(&compressed)).unwrap();
            black_box(decompressed)
        })
    });

    group.finish();
}

#[cfg(feature = "compression")]
fn bench_lz4_random(c: &mut Criterion) {
    let compressor = Lz4Compressor::new();

    // Random data (incompressible)
    let data: Vec<u8> = (0..10_000).map(|i| (i * 17 + 13) as u8).collect();

    let mut group = c.benchmark_group("lz4_random");
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_function("compress", |b| {
        b.iter(|| {
            let compressed = compressor.compress(black_box(&data)).unwrap();
            black_box(compressed)
        })
    });

    let compressed = compressor.compress(&data).unwrap();

    group.bench_function("decompress", |b| {
        b.iter(|| {
            let decompressed = compressor.decompress(black_box(&compressed)).unwrap();
            black_box(decompressed)
        })
    });

    group.finish();
}

#[cfg(feature = "compression")]
fn bench_no_compression(c: &mut Criterion) {
    let compressor = NoCompressor;
    let data = vec![0xAB; 10_000];

    let mut group = c.benchmark_group("no_compression");
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_function("compress", |b| {
        b.iter(|| {
            let compressed = compressor.compress(black_box(&data)).unwrap();
            black_box(compressed)
        })
    });

    let compressed = compressor.compress(&data).unwrap();

    group.bench_function("decompress", |b| {
        b.iter(|| {
            let decompressed = compressor.decompress(black_box(&compressed)).unwrap();
            black_box(decompressed)
        })
    });

    group.finish();
}

#[cfg(feature = "compression")]
fn bench_compression_comparison(c: &mut Criterion) {
    let zstd_compressor = ZstdCompressor::new();
    let lz4_compressor = Lz4Compressor::new();

    // Highly compressible data
    let compressible_size = 10_000;
    let mut data = Vec::with_capacity(compressible_size);
    data.extend(std::iter::repeat_n(0xAB, compressible_size));

    let mut group = c.benchmark_group("compression_comparison");
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_with_input(BenchmarkId::new("zstd", "compress"), &data, |b, d| {
        b.iter(|| {
            let compressed = zstd_compressor.compress(black_box(d)).unwrap();
            black_box(compressed)
        })
    });

    group.bench_with_input(BenchmarkId::new("lz4", "compress"), &data, |b, d| {
        b.iter(|| {
            let compressed = lz4_compressor.compress(black_box(d)).unwrap();
            black_box(compressed)
        })
    });

    group.finish();
}

#[cfg(feature = "compression")]
criterion_group!(
    benches,
    bench_zstd_compression,
    bench_lz4_compression,
    bench_zstd_random,
    bench_lz4_random,
    bench_no_compression,
    bench_compression_comparison
);

#[cfg(feature = "compression")]
criterion_main!(benches);

#[cfg(not(feature = "compression"))]
fn main() {
    println!("Compression benchmarks require the 'compression' feature");
}
