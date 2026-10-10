//! Logical guest lookup at varying server populations; transport identity changes
//! must not turn the per-input ownership check into a scan of retained guests.
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use mokosh_protocol::{
    messages::{routes, Hello},
    resume::ResumeRequest,
    CodecType, Envelope, EnvelopeFlags, SessionEnvelope, SessionId, CURRENT_PROTOCOL_VERSION,
    MIN_PROTOCOL_VERSION,
};
use mokosh_server::{Server, ServerConfig};
use tokio::sync::mpsc;

fn bench_session_routing(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("guest_transport_owner");
    for count in [1, 64, 1024] {
        let (server, target) = runtime.block_on(async {
            let (incoming, receiver) = mpsc::channel(16);
            // Keep replies buffered during setup; the measured lookup does no I/O.
            let (outgoing, _responses) = mpsc::channel(2 * count + 16);
            let json = CodecType::from_id(1).unwrap();
            let mut server = Server::with_full_config(
                receiver,
                outgoing,
                json,
                json,
                ServerConfig {
                    guest_resume: Some(Default::default()),
                    reliability: Some(Default::default()),
                    ..Default::default()
                },
                None,
                None,
                mokosh_protocol::compression::NoCompressor,
                mokosh_protocol::encryption::NoEncryptor,
            );
            let mut target = SessionId::nil();
            for index in 0..count {
                target = SessionId::new_v4();
                let hello = Hello {
                    guest_resume: true,
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    min_protocol_version: MIN_PROTOCOL_VERSION,
                    codec_id: 1,
                    schema_hash: 0,
                    reliability: true,
                };
                incoming
                    .send(SessionEnvelope::new(
                        target,
                        Envelope::new_simple(
                            CURRENT_PROTOCOL_VERSION,
                            1,
                            0,
                            routes::HELLO,
                            1,
                            EnvelopeFlags::RELIABLE,
                            json.encode(&hello).unwrap(),
                        ),
                    ))
                    .await
                    .unwrap();
                while !server
                    .get_session_state(target)
                    .is_some_and(|s| s.is_connected())
                {
                    server.tick().await.unwrap();
                }
                let mut operation = [0; 16];
                operation[..8].copy_from_slice(&(index as u64).to_le_bytes());
                incoming
                    .send(SessionEnvelope {
                        session_id: target,
                        protected_udp: true,
                        envelope: Envelope::new_simple(
                            CURRENT_PROTOCOL_VERSION,
                            1,
                            0,
                            routes::RESUME_REQUEST,
                            2,
                            EnvelopeFlags::RELIABLE,
                            json.encode(&ResumeRequest {
                                player_id: None,
                                token: None,
                                operation,
                            })
                            .unwrap(),
                        ),
                    })
                    .await
                    .unwrap();
                while server.player_id(target).is_none() {
                    server.tick().await.unwrap();
                }
            }
            (server, target)
        });
        group.bench_with_input(BenchmarkId::from_parameter(count), &target, |b, target| {
            b.iter(|| black_box(server.player_id(black_box(*target))))
        });
    }
    group.finish();
}
criterion_group!(benches, bench_session_routing);
criterion_main!(benches);
