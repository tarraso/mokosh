use bytes::Bytes;
use godot_netlink_protocol::{Envelope, EnvelopeFlags, ENVELOPE_HEADER_SIZE};

#[test]
fn test_envelope_serialization_roundtrip() {
    let original = Envelope::new_simple(
        256,
        1,
        0x1234567890ABCDEF,
        100,
        42,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"test payload"),
    );

    let bytes = original.to_bytes();
    let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}

#[test]
fn test_envelope_header_size() {
    let envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::empty(),
        Bytes::new(),
    );

    let bytes = envelope.to_bytes();
    assert_eq!(bytes.len(), ENVELOPE_HEADER_SIZE);
}

#[test]
fn test_envelope_total_size_calculation() {
    let payload_size = 100;
    let payload = vec![0xAB; payload_size];

    let envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::empty(),
        Bytes::from(payload),
    );

    assert_eq!(envelope.total_size(), ENVELOPE_HEADER_SIZE + payload_size);

    let bytes = envelope.to_bytes();
    assert_eq!(bytes.len(), ENVELOPE_HEADER_SIZE + payload_size);
}

#[test]
fn test_different_payload_sizes() {
    let test_cases = vec![0, 1, 10, 100, 1000, 10000, 65535];

    for size in test_cases {
        let payload = vec![0x42; size];
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from(payload.clone()),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect(&format!("Failed at size {}", size));

        assert_eq!(deserialized.payload.len(), size);
        assert_eq!(deserialized.payload_len, size as u32);
        assert_eq!(deserialized.payload, Bytes::from(payload));
    }
}

#[test]
fn test_all_flag_combinations() {
    let flag_combinations = vec![
        EnvelopeFlags::empty(),
        EnvelopeFlags::RELIABLE,
        EnvelopeFlags::ENCRYPTED,
        EnvelopeFlags::COMPRESSED,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::ENCRYPTED,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED,
        EnvelopeFlags::ENCRYPTED | EnvelopeFlags::COMPRESSED,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::ENCRYPTED | EnvelopeFlags::COMPRESSED,
    ];

    for flags in flag_combinations {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            flags,
            Bytes::from_static(b"test"),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.flags, flags);
        assert_eq!(deserialized.is_reliable(), flags.contains(EnvelopeFlags::RELIABLE));
        assert_eq!(
            deserialized.is_encrypted(),
            flags.contains(EnvelopeFlags::ENCRYPTED)
        );
        assert_eq!(
            deserialized.is_compressed(),
            flags.contains(EnvelopeFlags::COMPRESSED)
        );
    }
}

#[test]
fn test_envelope_with_correlation_id() {
    let rpc_envelope = Envelope::new(
        1,
        1,
        0,
        100,
        1,
        12345,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"rpc request"),
    );

    assert!(rpc_envelope.is_rpc());
    assert_eq!(rpc_envelope.correlation_id, 12345);

    let bytes = rpc_envelope.to_bytes();
    let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

    assert_eq!(deserialized.correlation_id, 12345);
    assert!(deserialized.is_rpc());
}

#[test]
fn test_protocol_version_field() {
    let versions = vec![0, 1, 256, 512, 65535];

    for version in versions {
        let envelope = Envelope::new_simple(
            version,
            1,
            0,
            100,
            1,
            EnvelopeFlags::empty(),
            Bytes::from_static(b"test"),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.protocol_version, version);
    }
}

#[test]
fn test_codec_id_field() {
    let codec_ids = vec![0, 1, 2, 3, 255];

    for codec_id in codec_ids {
        let envelope = Envelope::new_simple(
            1,
            codec_id,
            0,
            100,
            1,
            EnvelopeFlags::empty(),
            Bytes::from_static(b"test"),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.codec_id, codec_id);
    }
}

#[test]
fn test_schema_hash_field() {
    let hashes = vec![0, 1, 0x1234567890ABCDEF, u64::MAX];

    for hash in hashes {
        let envelope = Envelope::new_simple(
            1,
            1,
            hash,
            100,
            1,
            EnvelopeFlags::empty(),
            Bytes::from_static(b"test"),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.schema_hash, hash);
    }
}

#[test]
fn test_route_id_field() {
    let route_ids = vec![0, 1, 100, 1000, 65535];

    for route_id in route_ids {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            route_id,
            1,
            EnvelopeFlags::empty(),
            Bytes::from_static(b"test"),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.route_id, route_id);
    }
}

#[test]
fn test_msg_id_field() {
    let msg_ids = vec![0, 1, 100, 1000, u64::MAX];

    for msg_id in msg_ids {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            msg_id,
            EnvelopeFlags::empty(),
            Bytes::from_static(b"test"),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.msg_id, msg_id);
    }
}

#[test]
fn test_binary_data_payload() {
    let binary_payloads = vec![
        vec![0x00, 0x01, 0x02, 0x03],
        vec![0xFF, 0xFE, 0xFD, 0xFC],
        vec![0xDE, 0xAD, 0xBE, 0xEF],
        (0u8..=255u8).collect::<Vec<u8>>(),
    ];

    for payload_data in binary_payloads {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::empty(),
            Bytes::from(payload_data.clone()),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.payload, Bytes::from(payload_data));
    }
}

#[test]
fn test_multiple_envelopes_in_sequence() {
    let mut all_bytes = Vec::new();

    for i in 0..10 {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            100 + i,
            i as u64,
            EnvelopeFlags::RELIABLE,
            Bytes::from(format!("message {}", i)),
        );

        all_bytes.extend_from_slice(&envelope.to_bytes());
    }

    let mut cursor = Bytes::from(all_bytes);

    for i in 0..10 {
        let envelope_bytes = cursor.split_to(
            ENVELOPE_HEADER_SIZE + format!("message {}", i).len(),
        );

        let envelope = Envelope::from_bytes(envelope_bytes).expect("Failed to deserialize");

        assert_eq!(envelope.route_id, 100 + i);
        assert_eq!(envelope.msg_id, i as u64);
        assert_eq!(
            envelope.payload,
            Bytes::from(format!("message {}", i))
        );
    }
}
