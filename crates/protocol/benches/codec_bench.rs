//! Benchmarks for Codec performance (JSON vs Postcard vs Raw)
//!
//! Goal: Verify 86% bandwidth reduction (Postcard vs JSON)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use mokosh_protocol::CodecType;
use serde::{Deserialize, Serialize};

/// Test message structure
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestMessage {
    id: u64,
    x: f32,
    y: f32,
    health: u32,
    name: String,
    active: bool,
}

impl TestMessage {
    fn new() -> Self {
        Self {
            id: 12345,
            x: 100.5,
            y: 200.75,
            health: 95,
            name: "Player1".to_string(),
            active: true,
        }
    }
}

/// Benchmark: JSON encoding
fn bench_json_encode(c: &mut Criterion) {
    let codec = CodecType::from_id(1).unwrap(); // JSON
    let message = TestMessage::new();

    c.bench_function("codec_json_encode", |b| {
        b.iter(|| {
            let result = codec.encode(black_box(&message));
            black_box(result)
        });
    });
}

/// Benchmark: JSON decoding
fn bench_json_decode(c: &mut Criterion) {
    let codec = CodecType::from_id(1).unwrap(); // JSON
    let message = TestMessage::new();
    let encoded = codec.encode(&message).unwrap();

    c.bench_function("codec_json_decode", |b| {
        b.iter(|| {
            let result: Result<TestMessage, _> = codec.decode(black_box(&encoded));
            black_box(result)
        });
    });
}

/// Benchmark: Postcard encoding
fn bench_postcard_encode(c: &mut Criterion) {
    let codec = CodecType::from_id(2).unwrap(); // Postcard
    let message = TestMessage::new();

    c.bench_function("codec_postcard_encode", |b| {
        b.iter(|| {
            let result = codec.encode(black_box(&message));
            black_box(result)
        });
    });
}

/// Benchmark: Postcard decoding
fn bench_postcard_decode(c: &mut Criterion) {
    let codec = CodecType::from_id(2).unwrap(); // Postcard
    let message = TestMessage::new();
    let encoded = codec.encode(&message).unwrap();

    c.bench_function("codec_postcard_decode", |b| {
        b.iter(|| {
            let result: Result<TestMessage, _> = codec.decode(black_box(&encoded));
            black_box(result)
        });
    });
}

/// Benchmark: Raw encoding (no-op)
fn bench_raw_encode(c: &mut Criterion) {
    let codec = CodecType::from_id(3).unwrap(); // Raw
    let data = vec![0xAB; 100];

    c.bench_function("codec_raw_encode", |b| {
        b.iter(|| {
            let result = codec.encode(black_box(&data));
            black_box(result)
        });
    });
}

/// Benchmark: Codec comparison across payload sizes
fn bench_codec_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec_comparison");

    // Create test data of varying sizes
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct BulkMessage {
        items: Vec<TestMessage>,
    }

    for count in [1, 10, 100].iter() {
        let data = BulkMessage {
            items: vec![TestMessage::new(); *count],
        };

        // JSON
        let json_codec = CodecType::from_id(1).unwrap();
        let json_encoded = json_codec.encode(&data).unwrap();
        group.throughput(Throughput::Bytes(json_encoded.len() as u64));

        group.bench_with_input(
            BenchmarkId::new("json_roundtrip", count),
            &data,
            |b, data| {
                b.iter(|| {
                    let encoded = json_codec.encode(black_box(data)).unwrap();
                    let decoded: BulkMessage = json_codec.decode(&encoded).unwrap();
                    black_box(decoded)
                });
            },
        );

        // Postcard
        let postcard_codec = CodecType::from_id(2).unwrap();
        let postcard_encoded = postcard_codec.encode(&data).unwrap();

        group.bench_with_input(
            BenchmarkId::new("postcard_roundtrip", count),
            &data,
            |b, data| {
                b.iter(|| {
                    let encoded = postcard_codec.encode(black_box(data)).unwrap();
                    let decoded: BulkMessage = postcard_codec.decode(&encoded).unwrap();
                    black_box(decoded)
                });
            },
        );

        // Print size comparison
        let json_size = json_encoded.len();
        let postcard_size = postcard_encoded.len();
        let reduction = ((json_size - postcard_size) as f64 / json_size as f64) * 100.0;

        println!(
            "\n[count={}] JSON: {} bytes, Postcard: {} bytes, Reduction: {:.1}%",
            count, json_size, postcard_size, reduction
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_json_encode,
    bench_json_decode,
    bench_postcard_encode,
    bench_postcard_decode,
    bench_raw_encode,
    bench_codec_comparison
);
criterion_main!(benches);
