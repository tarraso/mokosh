//! Benchmarks for Envelope encode/decode/validation
//!
//! Target: < 50 ns for encode/decode operations

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use mokosh_protocol::{Envelope, EnvelopeFlags, CURRENT_PROTOCOL_VERSION};

/// Benchmark: Envelope encoding (header serialization to 34 bytes)
fn bench_envelope_encode(c: &mut Criterion) {
    let payload = Bytes::from_static(b"test payload");

    c.bench_function("envelope_encode", |b| {
        b.iter(|| {
            let envelope = Envelope::new_simple(
                black_box(CURRENT_PROTOCOL_VERSION),
                black_box(1), // JSON codec
                black_box(0xABCD),
                black_box(100),
                black_box(42),
                black_box(EnvelopeFlags::RELIABLE),
                black_box(payload.clone()),
            );

            black_box(envelope.to_bytes())
        });
    });
}

/// Benchmark: Envelope decoding (header deserialization from 34 bytes)
fn bench_envelope_decode(c: &mut Criterion) {
    let envelope = Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0xABCD,
        100,
        42,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"test payload"),
    );

    let bytes = envelope.to_bytes();

    c.bench_function("envelope_decode", |b| {
        b.iter(|| {
            let result = Envelope::from_bytes(black_box(bytes.clone()));
            black_box(result)
        });
    });
}

/// Benchmark: Envelope roundtrip (encode → decode)
fn bench_envelope_roundtrip(c: &mut Criterion) {
    let payload = Bytes::from_static(b"test payload");

    c.bench_function("envelope_roundtrip", |b| {
        b.iter(|| {
            let envelope = Envelope::new_simple(
                black_box(CURRENT_PROTOCOL_VERSION),
                black_box(1),
                black_box(0xABCD),
                black_box(100),
                black_box(42),
                black_box(EnvelopeFlags::RELIABLE),
                black_box(payload.clone()),
            );

            let bytes = envelope.to_bytes();
            let decoded = Envelope::from_bytes(bytes).unwrap();
            black_box(decoded)
        });
    });
}

/// Benchmark: Envelope validation
fn bench_envelope_validation(c: &mut Criterion) {
    let envelope = Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0xABCD,
        100,
        42,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"test payload"),
    );

    c.bench_function("envelope_validation", |b| {
        b.iter(|| {
            let result = black_box(&envelope).validate();
            black_box(result)
        });
    });
}

/// Benchmark: Envelope operations with varying payload sizes
fn bench_envelope_by_payload_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope_by_payload_size");

    for size in [1, 10, 100, 1_000, 10_000, 100_000].iter() {
        group.throughput(Throughput::Bytes(*size as u64));

        let payload = Bytes::from(vec![0xAB; *size]);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let envelope = Envelope::new_simple(
                    black_box(CURRENT_PROTOCOL_VERSION),
                    black_box(1),
                    black_box(0xABCD),
                    black_box(100),
                    black_box(42),
                    black_box(EnvelopeFlags::RELIABLE),
                    black_box(payload.clone()),
                );

                let bytes = envelope.to_bytes();
                let decoded = Envelope::from_bytes(bytes).unwrap();
                black_box(decoded)
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_envelope_encode,
    bench_envelope_decode,
    bench_envelope_roundtrip,
    bench_envelope_validation,
    bench_envelope_by_payload_size
);
criterion_main!(benches);
