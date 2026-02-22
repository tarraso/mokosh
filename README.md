# Mokosh

**Mokosh** is a high-performance, production-ready networking library for Godot 4, written in Rust. It provides an authoritative server architecture with client-side prediction, encryption, compression, and a flexible message protocol.

## Features

- **🎮 Godot 4 Integration** - Native GDExtension bindings (`NetClient`, `NetServer`)
- **🔒 Secure by Design** - ChaCha20-Poly1305 encryption, replay protection, rate limiting
- **⚡ Client-Side Prediction** - Zero-latency feel with authoritative server reconciliation
- **📦 Efficient Protocol** - Multiple codecs (JSON, Postcard, Raw), compression (Zstd, Lz4)
- **🛠️ Type-Safe Messaging** - `#[derive(GameMessage)]` with automatic schema validation
- **🌐 Flexible Transport** - WebSocket support with pluggable transport layer
- **📊 Built-in Monitoring** - RTT measurement, keepalive, connection health

## Quick Start

### Rust Examples

```bash
# Run the test server
cargo run --example test_server

# In another terminal, run a client
cargo run --example codec_demo
```

### Godot Demo

```bash
# Start the Rust server
cargo run --example test_server

# Open the Godot demo
godot --path examples/godot-demo
```

Or open `examples/godot-demo` in the Godot editor and press F5.

## Architecture

Mokosh is organized as a Rust workspace with six crates:

- **`mokosh-protocol`** - Core protocol (envelope, transports, codecs, encryption, compression)
- **`mokosh-protocol-derive`** - `#[derive(GameMessage)]` procedural macro
- **`mokosh-server`** - Authoritative server with multi-client support
- **`mokosh-client`** - Client event loop with prediction support
- **`mokosh-simulation`** - Shared simulation layer for client/server
- **`mokosh-bindings`** - Godot 4 GDExtension (NetClient/NetServer)

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

The repository includes 9 working examples:

1. **`codec_demo`** - Codec negotiation (JSON, Postcard, Raw)
2. **`websocket_echo_test`** - WebSocket transport basics
3. **`keepalive_demo`** - PING/PONG and RTT measurement
4. **`encryption_compression_demo`** - Security features
5. **`message_registry_test`** - Type-safe message handling
6. **`multiclient_demo`** - Multi-client server
7. **`memory_transport_test`** - In-memory transport (testing)
8. **`test_server`** - Production-like server for Godot demo
9. **`test_client`** - Standalone Rust client

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
# Run all tests (186 total)
cargo test --workspace

# Run specific crate tests
cargo test -p mokosh-protocol

# Run integration tests only
cargo test --test integration_test
```

## Performance

Codec comparison (1000 messages):

| Codec    | Size (bytes) | Reduction |
|----------|--------------|-----------|
| JSON     | 34,567       | baseline  |
| Postcard | 4,829        | -86%      |
| Raw      | 4,000        | -88%      |

With Zstd compression: ~72% additional reduction
With Lz4 compression: ~55% additional reduction

## Roadmap

### ✅ Completed (Phases 1-6)

- Core protocol and transports
- Message registry with schema validation
- Authentication system
- Keepalive, replay protection, rate limiting
- Encryption and compression
- Client prediction and server simulation
- Godot 4 GDExtension bindings (MVP)

### 📋 Future Work (Phase 7+)

- Performance benchmarking suite
- API documentation (`cargo doc`)
- Production deployment guides
- Advanced Godot networking features
- WebRTC transport
- Metrics and observability

## License

MIT License - see [LICENSE](LICENSE) file for details.

## Contributing

[Add contribution guidelines here]

## Credits

Built with:
- [tokio](https://tokio.rs/) - Async runtime
- [tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) - WebSocket
- [serde](https://serde.rs/) - Serialization
- [postcard](https://github.com/jamesmunns/postcard) - Compact binary format
- [chacha20poly1305](https://docs.rs/chacha20poly1305/) - Encryption
- [zstd](https://facebook.github.io/zstd/) / [lz4](https://lz4.github.io/lz4/) - Compression
- [godot-rust](https://godot-rust.github.io/) - Godot bindings
