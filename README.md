# Mokosh

**Mokosh** is a production-ready networking library written in Rust, providing an authoritative server architecture with client-side prediction, security features, and WASM support.

## Features

- **🎮 Multi-Platform** - Native (Godot, Bevy, Rust)
- **🔒 Secure by Design** - ChaCha20-Poly1305 encryption, replay protection, rate limiting
- **⚡ Client-Side Prediction** - Zero-latency feel with authoritative reconciliation
- **📦 Efficient Protocol** - Multiple codecs (JSON, Postcard, Raw), compression (Zstd, Lz4)
- **🛠️ Type-Safe Messaging** - `#[derive(GameMessage)]` with automatic schema validation
- **🌐 Flexible Transport** - WebSocket (native) + BrowserWebSocket (WASM)
- **📊 Built-in Monitoring** - RTT measurement, keepalive, connection health

## Quick Start

### Bevy Platformer Demo (Native)

```bash
# Terminal 1: Run the server
cargo run --example bevy_platformer_server --features native

# Terminal 2: Run the client
cargo run --example bevy_platformer_client --features native
```

### Bevy Platformer Demo (WASM)

```bash
# Terminal 1: Run the server
cargo run --example bevy_platformer_server --features native

# Terminal 2: Build and serve WASM client
cargo build --example bevy_platformer_client_wasm --target wasm32-unknown-unknown --release --features wasm
wasm-bindgen --out-dir web/pkg --out-name bevy_platformer_client_wasm --target web target/wasm32-unknown-unknown/release/bevy_platformer_wasm.wasm
# Serve with any HTTP server, e.g.: python3 -m http.server 8000
```

### Other Examples

```bash
# Codec comparison
cargo run --example codec_demo

# Authentication flow
cargo run --example auth_demo

# Encryption + Compression
cargo run --example encryption_compression_demo

# Type-safe messaging
cargo run --example message_registry_demo

# Client-side prediction
cargo run --example prediction_demo
```

## Architecture

Mokosh is organized as a Rust workspace with seven crates:

- **`mokosh-protocol`** - Core protocol (envelope, transports, codecs, encryption, compression)
- **`mokosh-protocol-derive`** - `#[derive(GameMessage)]` procedural macro
- **`mokosh-server`** - Authoritative server with multi-client support
- **`mokosh-client`** - Client event loop with WebSocket (native) and BrowserWebSocket (WASM)
- **`mokosh-simulation`** - Shared simulation layer for client/server (WASM-compatible)
- **`mokosh-godot-bindings`** - Godot 4 GDExtension (NetClient/NetServer)
- **`mokosh-examples-shared`** - Shared game logic for examples (WASM-compatible)

### Protocol Design

The core is a **envelope** containing:
- Protocol version, codec ID, schema hash
- Route ID, message ID, correlation ID
- Flags (compression, encryption)
- Payload length

This allows:
- **Codec negotiation** - Server/client agree on serialization format
- **Schema validation** - Automatic hash verification prevents version mismatches
- **Zero-copy optimization** - Envelope-first design enables efficient parsing

### Security Features

- **Authentication** - Pluggable `AuthProvider` trait (mock, token, custom)
- **Replay Protection** - Hybrid approach with tolerance window (default: 10 messages)
- **Rate Limiting** - Token bucket per session (100 msg/s, burst: 150)
- **Encryption** - Optional ChaCha20-Poly1305 AEAD
- **Compression** - Optional Zstd (~72% reduction) or Lz4 (~55% reduction)

### Game Simulation

```rust
// Server: Authoritative simulation
let mut server_sim = ServerSimulation::new(MyGameState::default());
server_sim.apply_client_input(session_id, input);
let snapshot = server_sim.snapshot();

// Client: Predictive simulation with reconciliation
let mut client_pred = ClientPredictor::new(MyGameState::default());
client_pred.predict(input); // Instant local feedback
client_pred.reconcile(server_snapshot); // Sync with authority
```

## Usage

### Define Game Messages

```rust
use mokosh_protocol_derive::GameMessage;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 100]
struct PlayerInput {
    x: f32,
    y: f32,
    action: PlayerAction,
}

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 101]
struct GameState {
    tick: u64,
    players: Vec<PlayerData>,
}
```

### Client Side

```rust
use mokosh_client::Client;

// Create client
let mut client = Client::new(
    transport,
    codec_preference,
    auth_provider,
    registry,
);

// Connect
client.connect().await?;

// Send message
client.send_message(PlayerInput { x: 10.0, y: 20.0, action: Jump }).await?;

// Receive messages
while let Some(envelope) = client.recv().await {
    let msg: GameState = client.deserialize_message(&envelope)?;
    // Handle game state update
}
```

### Server Side

```rust
use mokosh_server::Server;

// Create server
let mut server = Server::new(auth_provider, codec_preference, registry);

// Accept connections
let listener = TcpListener::bind("127.0.0.1:8080").await?;
loop {
    let (stream, _) = listener.accept().await?;
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let transport = WebSocketTransport::new(ws);
    server.add_client(transport).await?;
}

// Process events
while let Some(event) = server.next_event().await {
    match event {
        ServerEvent::ClientConnected(session_id) => { /* ... */ }
        ServerEvent::MessageReceived(session_id, envelope) => {
            let input: PlayerInput = server.deserialize_message(&envelope)?;
            // Process input, update simulation, broadcast state
        }
        ServerEvent::ClientDisconnected(session_id) => { /* ... */ }
    }
}
```

### Godot (GDScript)

```gdscript
# Create client
var client = NetClient.new()
add_child(client)

# Connect signals
client.connected.connect(_on_connected)
client.message_received.connect(_on_message)
client.disconnected.connect(_on_disconnected)

# Connect to server
client.connect_to_server("ws://127.0.0.1:8080")

# Send message
var input = {"x": 10.0, "y": 20.0, "action": "jump"}
client.send_json(100, input)  # route_id = 100

func _on_message(route_id: int, data: Dictionary):
    if route_id == 101:  # GameState
        update_game(data)
```

## Examples

The repository includes 9 production-ready examples:

1. **`platformer_server`** - Simple event-based server (for Godot clients)
2. **`bevy_platformer_server`** - Bevy-based server with physics simulation
3. **`bevy_platformer_client`** - Bevy native desktop client
4. **`bevy_platformer_client_wasm`** - Bevy WASM browser client
5. **`codec_demo`** - Demonstrates codec comparison (JSON vs Postcard vs Raw)
6. **`auth_demo`** - Shows authentication flow with AuthProvider
7. **`encryption_compression_demo`** - Encryption + compression features
8. **`message_registry_demo`** - Type-safe messaging with schema validation
9. **`prediction_demo`** - Client-side prediction + server reconciliation

See `examples/platformer_bevy/README.md` for the full Bevy demo setup instructions.

## Building

### Prerequisites

- Rust 1.70+ (`rustup`)
- Godot 4.2+ (for Godot bindings)

### Build Everything

```bash
cargo build --workspace
```

### Build Godot Bindings

```bash
cargo build -p mokosh-bindings --release
```

The compiled library will be at:
- macOS: `target/release/libmokosh_bindings.dylib`
- Linux: `target/release/libmokosh_bindings.so`
- Windows: `target/release/mokosh_bindings.dll`

Copy to your Godot project's `bin/` directory.

## Testing

```bash
# Run all tests (214 total: 145 unit + 28 doc + 5 integration)
cargo test --workspace

# Run specific crate tests
cargo test -p mokosh-protocol  # Protocol crate
cargo test -p mokosh-server    # Server crate
cargo test -p mokosh-client    # Client crate

# Run integration tests
cargo test --test auth_flow_test
cargo test --test encryption_compression_test
cargo test --test simulation_test

# Run benchmarks
cargo bench --features native --bench envelope_bench
cargo bench --features native --bench codec_bench
cargo bench --features compression --bench compression_bench
```

**Test breakdown:**
- `mokosh-protocol`: 95 unit tests
- `mokosh-server`: 11 unit tests
- `mokosh-client`: 9 unit tests
- `mokosh-simulation`: 7 unit tests
- `mokosh-protocol-derive`: 5 unit tests
- Doc tests: 28 (inline documentation examples)
- Integration tests: 5 (auth flow, encryption, simulation, etc.)

## Performance

Codec comparison (1000 messages):

| Codec    | Size (bytes) | Reduction |
|----------|--------------|-----------|
| JSON     | 34,567       | baseline  |
| Postcard | 4,829        | -86%      |
| Raw      | 4,000        | -88%      |

With Zstd compression: ~72% additional reduction
With Lz4 compression: ~55% additional reduction

## Implementation Status

### ✅ Complete (Production-Ready)

**Phase 1-6** - Fully implemented and tested:
- Core protocol (envelope, transport, state machine, codecs)
- Message registry with schema validation
- `#[derive(GameMessage)]` macro
- Authentication system (AuthProvider trait)
- Keepalive & RTT measurement
- Replay protection & rate limiting
- Encryption (ChaCha20-Poly1305) & compression (Zstd, Lz4)
- Client-side prediction + server reconciliation
- Godot 4 GDExtension bindings

### ❌ Not Implemented (Design Only)

**Phase 7: Scalability** - Spatial filtering for 1000+ players
- Geohash spatial indexing
- Three-tier relevancy system (proximity, visibility)
- Design doc: `/Users/taras/projects/gdrust/doc/camera-system-concept.md`

**Phase 8: Rooms** - Isolated game sessions
- Single-server rooms, multi-threaded rooms
- Multi-server routing, regional servers
- Design doc: `/Users/taras/projects/gdrust/doc/rooms-*.md`

**Phase 9: Production** - Advanced features
- Monitoring, deployment, tooling

## License

MIT License - see [LICENSE](LICENSE) file for details.

## Contributing

Contributions welcome! See design documentation in `/Users/taras/projects/gdrust/doc/` for future features.

## Credits

Built with:
- [tokio](https://tokio.rs/) - Async runtime
- [tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) - WebSocket
- [serde](https://serde.rs/) - Serialization
- [postcard](https://github.com/jamesmunns/postcard) - Compact binary format
- [chacha20poly1305](https://docs.rs/chacha20poly1305/) - Encryption
- [zstd](https://facebook.github.io/zstd/) / [lz4](https://lz4.github.io/lz4/) - Compression
- [godot-rust](https://godot-rust.github.io/) - Godot bindings
