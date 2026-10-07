# Mokosh

Mokosh is a Rust networking library for authoritative game servers, native clients and browser clients. The workspace includes protocol code, client and server event loops, simulation helpers, Godot bindings and runnable examples.

## Features

- Native WebSocket and UDP transports, plus browser WebSocket support.
- JSON, Postcard and raw-byte codecs.
- Type-safe messages with `#[derive(GameMessage)]` and schema validation.
- Authentication providers, replay protection, rate limiting, keepalive and RTT measurement.
- Reliability modes with acknowledgements, retransmission, sequencing and ordering.
- ChaCha20-Poly1305 encryption and optional Zstd/Lz4 compression.
- UDP address-validation cookies and authenticated records with session keys and replay protection.
- Shared simulation helpers for client prediction and server reconciliation.
- Godot 4 GDExtension client bindings and Bevy examples.

## Quick Start

Run commands from the repository root.

### Bevy Platformer over WebSocket

```bash
# Terminal 1
cargo run --example bevy_platformer_server

# Terminal 2
cargo run --example bevy_platformer_client
```

### UDP Platformer

```bash
# Terminal 1
cargo run --example platformer_server_udp

# Terminal 2
cargo run --example platformer_client_udp
```

For the graphical UDP client, use `bevy_platformer_server_udp` and `bevy_platformer_client_udp` instead. The UDP examples enable reliability and datagram encryption with a shared demonstration PSK. Use a separately generated secret key for your own deployment.

### Browser Client

```bash
rustup target add wasm32-unknown-unknown
cargo build --example bevy_platformer_client_wasm \
    --target wasm32-unknown-unknown --release \
    --no-default-features --features wasm
```

See the [Bevy example README](examples/platformer_bevy/README.md) for browser serving instructions and the [Godot example README](examples/platformer/README.md) for the GDExtension client.

## Workspace

| Package | Source | Responsibility |
|---------|--------|----------------|
| `mokosh-protocol` | [crates/protocol](crates/protocol) | Envelopes, codecs, authentication, encryption, reliability and transport traits |
| `mokosh-protocol-derive` | [crates/protocol-derive](crates/protocol-derive) | The `GameMessage` derive macro |
| `mokosh-server` | [crates/server](crates/server) | Authoritative server event loop and transports |
| `mokosh-client` | [crates/client](crates/client) | Client event loop, message handles and transports |
| `mokosh-simulation` | [crates/simulation](crates/simulation) | Prediction and reconciliation helpers |
| `mokosh-bindings` | [crates/godot-bindings](crates/godot-bindings) | Godot `NetClient` GDExtension |
| `mokosh-examples-shared` | [crates/examples-shared](crates/examples-shared) | Shared example messages and platformer physics |

The [protocol README](crates/protocol/README.md) describes the envelope format. The UDP record format, key schedule and replay checks are documented in [udp_record.rs](crates/protocol/src/udp_record.rs). Address-validation message layouts are in [udp_validation.rs](crates/protocol/src/udp_validation.rs).

## Usage

### Define a Game Message

```rust
use mokosh_protocol_derive::GameMessage;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 100]
struct PlayerInput {
    move_x: f32,
    jump: bool,
}
```

The [message registry demo](examples/message_registry_demo.rs) shows registration, encoding and schema validation. For complete transport and event-loop setup, use the [UDP client](examples/platformer/client_udp.rs) and [server](examples/platformer/server_udp.rs).

Godot message handling is demonstrated by [platformer_client.gd](examples/platformer/godot-client/scripts/platformer_client.gd). Shared authoritative physics is implemented in [platformer.rs](crates/examples-shared/src/platformer.rs), and [prediction_demo.rs](examples/prediction_demo.rs) demonstrates the simulation helpers.

## Examples

Each name below can be run with `cargo run --example NAME`, except the browser client, which targets WASM.

| Name | Source | Description |
|------|--------|-------------|
| `platformer_server` | [server.rs](examples/platformer/server.rs) | WebSocket server for the Godot client |
| `platformer_server_udp` | [server_udp.rs](examples/platformer/server_udp.rs) | UDP server with reliability and datagram encryption |
| `platformer_client_udp` | [client_udp.rs](examples/platformer/client_udp.rs) | Headless UDP client |
| `bevy_platformer_server` | [server.rs](examples/platformer_bevy/server.rs) | WebSocket platformer server |
| `bevy_platformer_client` | [client.rs](examples/platformer_bevy/client.rs) | Native Bevy WebSocket client |
| `bevy_platformer_server_udp` | [server_udp.rs](examples/platformer_bevy/server_udp.rs) | UDP platformer server |
| `bevy_platformer_client_udp` | [client_udp.rs](examples/platformer_bevy/client_udp.rs) | Native Bevy UDP client |
| `bevy_platformer_client_wasm` | [client_wasm.rs](examples/platformer_bevy/client_wasm.rs) | Browser WebSocket client |
| `codec_demo` | [codec_demo.rs](examples/codec_demo.rs) | Codec comparison |
| `auth_demo` | [auth_demo.rs](examples/auth_demo.rs) | Authentication flow |
| `encryption_compression_demo` | [encryption_compression_demo.rs](examples/encryption_compression_demo.rs) | Payload encryption and compression |
| `message_registry_demo` | [message_registry_demo.rs](examples/message_registry_demo.rs) | Type-safe messaging and schema validation |
| `prediction_demo` | [prediction_demo.rs](examples/prediction_demo.rs) | Prediction and reconciliation |

## Building

Use a current stable Rust toolchain. The Godot bindings are configured for the Godot 4.6 API.

```bash
# Native workspace
cargo build --workspace

# Godot client bindings
cargo build -p mokosh-bindings --release

# Generated API documentation
cargo doc --workspace --no-deps
```

The Godot library is generated under `target/release/`: `libmokosh_bindings.dylib` on macOS, `libmokosh_bindings.so` on Linux, or `mokosh_bindings.dll` on Windows. The example's [mokosh.gdextension](examples/platformer/godot-client/mokosh.gdextension) describes its library paths.

## Testing and Benchmarks

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Individual crates and integration tests
cargo test -p mokosh-protocol
cargo test -p mokosh-server
cargo test -p mokosh-client
cargo test --test udp_reliability_test
cargo test --test simulation_test

# Benchmarks
cargo bench --features native --bench envelope_bench
cargo bench --features native --bench codec_bench
cargo bench --features compression --bench compression_bench
```

Feature combinations checked by CI are defined in [features.yml](.github/workflows/features.yml).

## Contributing

Contributions are welcome. Include tests for behavior changes and update the relevant README or source documentation when public APIs change.

## License

MIT - see [LICENSE](LICENSE).

## Credits

- [Tokio](https://tokio.rs/) - Async runtime
- [tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) - WebSocket transport
- [Serde](https://serde.rs/) and [Postcard](https://github.com/jamesmunns/postcard) - Serialization
- [RustCrypto ChaCha20-Poly1305](https://docs.rs/chacha20poly1305/) - Authenticated encryption
- [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) - Key derivation and keyed hashes
- [Zstd](https://facebook.github.io/zstd/) and [Lz4](https://lz4.github.io/lz4/) - Compression
- [godot-rust](https://godot-rust.github.io/) - Godot bindings
