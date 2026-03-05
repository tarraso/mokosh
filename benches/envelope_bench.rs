//! Benchmark for Envelope encode/decode operations

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use bytes::Bytes;
use mokosh_protocol::envelope::{Envelope, EnvelopeFlags};

fn bench_envelope_encode(c: &mut Criterion) {
    let envelope = Envelope::new(
        1,     // protocol_version
        1,     // codec_id
        0,     // schema_hash
        100,   // route_id
        42,    // msg_id
        0,     // correlation_id
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello, Godot! This is a test payload."),
    );

    c.bench_function("envelope_encode", |b| {
        b.iter(|| black_box(&envelope).to_bytes())
    });
}

fn bench_envelope_decode(c: &mut Criterion) {
    let envelope = Envelope::new(
        1,     // protocol_version
        1,     // codec_id
        0,     // schema_hash
        100,   // route_id
        42,    // msg_id
        0,     // correlation_id
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello, Godot! This is a test payload."),
    );

    let bytes = envelope.to_bytes();

    c.bench_function("envelope_decode", |b| {
        b.iter(|| {
            let decoded = Envelope::from_bytes(black_box(bytes.clone())).unwrap();
            black_box(decoded)
        })
    });
}

fn bench_envelope_roundtrip(c: &mut Criterion) {
    let envelope = Envelope::new(
        1,     // protocol_version
        1,     // codec_id
        0,     // schema_hash
        100,   // route_id
        42,    // msg_id
        0,     // correlation_id
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello, Godot! This is a test payload."),
    );

    c.bench_function("envelope_roundtrip", |b| {
        b.iter(|| {
            let bytes = black_box(&envelope).to_bytes();
            let decoded = Envelope::from_bytes(bytes.clone()).unwrap();
            black_box(decoded)
        })
    });
}

fn bench_envelope_size_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope_size_scaling");

    for size in [64, 256, 1024, 4096, 16384].iter() {
        group.throughput(Throughput::Bytes(*size as u64));

        let payload = vec![0xAB; *size];
        let envelope = Envelope::new(
            1,     // protocol_version
            1,     // codec_id
            0,     // schema_hash
            100,   // route_id
            42,    // msg_id
            0,     // correlation_id
            EnvelopeFlags::RELIABLE,
            Bytes::from(payload),
        );

        group.bench_with_input(BenchmarkId::new("encode", size), size, |b, _| {
            b.iter(|| black_box(&envelope).to_bytes())
        });

        let bytes = envelope.to_bytes();
        group.bench_with_input(BenchmarkId::new("decode", size), size, |b, _| {
            b.iter(|| {
                let decoded = Envelope::from_bytes(black_box(bytes.clone())).unwrap();
                black_box(decoded)
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_envelope_encode,
    bench_envelope_decode,
    bench_envelope_roundtrip,
    bench_envelope_size_scaling
);
criterion_main!(benches);
