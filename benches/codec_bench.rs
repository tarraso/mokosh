//! Benchmark for codec comparison (JSON, Postcard, Raw)

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use bytes::Bytes;
use serde::{Serialize, Deserialize};
use mokosh_protocol::codec::{Codec, JsonCodec, PostcardCodec, RawCodec};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SimpleMessage {
    id: u64,
    name: String,
    value: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ComplexMessage {
    id: u64,
    session_id: String,
    position: Position,
    velocity: Velocity,
    health: f32,
    mana: f32,
    inventory: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Position {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Velocity {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Item {
    item_id: u32,
    quantity: u32,
    durability: f32,
}

fn bench_json_simple(c: &mut Criterion) {
    let message = SimpleMessage {
        id: 12345,
        name: "test_player".to_string(),
        value: 42.5,
    };

    let mut group = c.benchmark_group("json_simple");
    let codec = JsonCodec;

    group.bench_function("encode", |b| {
        b.iter(|| {
            let bytes: Bytes = codec.encode(black_box(&message)).unwrap();
            black_box(bytes)
        })
    });

    let encoded = codec.encode(&message).unwrap();

    group.bench_function("decode", |b| {
        b.iter(|| {
            let decoded: SimpleMessage = codec.decode(black_box(&encoded)).unwrap();
            black_box(decoded)
        })
    });

    group.finish();
}

fn bench_postcard_simple(c: &mut Criterion) {
    let message = SimpleMessage {
        id: 12345,
        name: "test_player".to_string(),
        value: 42.5,
    };

    let mut group = c.benchmark_group("postcard_simple");
    let codec = PostcardCodec;

    group.bench_function("encode", |b| {
        b.iter(|| {
            let bytes: Bytes = codec.encode(black_box(&message)).unwrap();
            black_box(bytes)
        })
    });

    let encoded = codec.encode(&message).unwrap();

    group.bench_function("decode", |b| {
        b.iter(|| {
            let decoded: SimpleMessage = codec.decode(black_box(&encoded)).unwrap();
            black_box(decoded)
        })
    });

    group.finish();
}

fn bench_json_complex(c: &mut Criterion) {
    let message = ComplexMessage {
        id: 12345,
        session_id: "session-abc-123".to_string(),
        position: Position { x: 100.0, y: 200.0, z: 50.0 },
        velocity: Velocity { x: 10.0, y: -5.0, z: 0.0 },
        health: 85.5,
        mana: 120.0,
        inventory: vec![
            Item { item_id: 1001, quantity: 5, durability: 0.9 },
            Item { item_id: 2002, quantity: 1, durability: 1.0 },
            Item { item_id: 3003, quantity: 20, durability: 0.5 },
        ],
    };

    let mut group = c.benchmark_group("json_complex");
    let codec = JsonCodec;

    group.bench_function("encode", |b| {
        b.iter(|| {
            let bytes: Bytes = codec.encode(black_box(&message)).unwrap();
            black_box(bytes)
        })
    });

    let encoded = codec.encode(&message).unwrap();

    group.bench_function("decode", |b| {
        b.iter(|| {
            let decoded: ComplexMessage = codec.decode(black_box(&encoded)).unwrap();
            black_box(decoded)
        })
    });

    group.finish();
}

fn bench_postcard_complex(c: &mut Criterion) {
    let message = ComplexMessage {
        id: 12345,
        session_id: "session-abc-123".to_string(),
        position: Position { x: 100.0, y: 200.0, z: 50.0 },
        velocity: Velocity { x: 10.0, y: -5.0, z: 0.0 },
        health: 85.5,
        mana: 120.0,
        inventory: vec![
            Item { item_id: 1001, quantity: 5, durability: 0.9 },
            Item { item_id: 2002, quantity: 1, durability: 1.0 },
            Item { item_id: 3003, quantity: 20, durability: 0.5 },
        ],
    };

    let mut group = c.benchmark_group("postcard_complex");
    let codec = PostcardCodec;

    group.bench_function("encode", |b| {
        b.iter(|| {
            let bytes: Bytes = codec.encode(black_box(&message)).unwrap();
            black_box(bytes)
        })
    });

    let encoded = codec.encode(&message).unwrap();

    group.bench_function("decode", |b| {
        b.iter(|| {
            let decoded: ComplexMessage = codec.decode(black_box(&encoded)).unwrap();
            black_box(decoded)
        })
    });

    group.finish();
}

fn bench_raw_codec(c: &mut Criterion) {
    let data = vec![0xAB, 0xCD, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x05];
    let mut group = c.benchmark_group("raw_codec");

    group.bench_function("encode", |b| {
        let codec = RawCodec;
        b.iter(|| {
            let bytes: Bytes = codec.encode(black_box(&data)).unwrap();
            black_box(bytes)
        })
    });

    let codec = RawCodec;
    let encoded = codec.encode(&data).unwrap();

    group.bench_function("decode", |b| {
        b.iter(|| {
            let decoded: Vec<u8> = codec.decode(black_box(&encoded)).unwrap();
            black_box(decoded)
        })
    });

    group.finish();
}

fn bench_codec_comparison(c: &mut Criterion) {
    let message = ComplexMessage {
        id: 12345,
        session_id: "session-abc-123".to_string(),
        position: Position { x: 100.0, y: 200.0, z: 50.0 },
        velocity: Velocity { x: 10.0, y: -5.0, z: 0.0 },
        health: 85.5,
        mana: 120.0,
        inventory: vec![
            Item { item_id: 1001, quantity: 5, durability: 0.9 },
            Item { item_id: 2002, quantity: 1, durability: 1.0 },
            Item { item_id: 3003, quantity: 20, durability: 0.5 },
        ],
    };

    let json_codec = JsonCodec;
    let postcard_codec = PostcardCodec;

    let json_bytes = json_codec.encode(&message).unwrap();

    let mut group = c.benchmark_group("codec_comparison");
    group.throughput(Throughput::Bytes(json_bytes.len() as u64));

    group.bench_with_input(BenchmarkId::new("json", "encode"), &message, |b, msg| {
        b.iter(|| {
            let bytes: Bytes = json_codec.encode(black_box(msg)).unwrap();
            black_box(bytes)
        })
    });

    group.bench_with_input(BenchmarkId::new("postcard", "encode"), &message, |b, msg| {
        b.iter(|| {
            let bytes: Bytes = postcard_codec.encode(black_box(msg)).unwrap();
            black_box(bytes)
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_json_simple,
    bench_postcard_simple,
    bench_json_complex,
    bench_postcard_complex,
    bench_raw_codec,
    bench_codec_comparison
);
criterion_main!(benches);
