# Bevy 2D Platformer Example

Multiplayer 2D platformer built with **Bevy** game engine and **Mokosh** networking library.

## Features

- **Bevy ECS Architecture**: Proper Entity-Component-System design
- **2D Rendering**: Sprite-based visualization with color-coded players
- **Server-Authoritative Physics**: Shared simulation between client and server
- **Real-time Networking**: WebSocket-based communication
- **Keyboard Controls**: WASD/Arrow keys + Space for jump
- **Multi-player Support**: Multiple clients can connect simultaneously

## Architecture

### Client (`client.rs`)
- **Bevy App** with ECS systems
- **Components**: `PlayerEntity`, `BoxEntity`, `LocalPlayerMarker`, `GroundLine`
- **Resources**: `NetworkClient`, `GameEntities`, `LocalSessionId`, `MessageCounter`
- **Systems**:
  - `setup_system` - Initialize camera and ground
  - `input_system` - Capture keyboard input and send `PlayerInput`
  - `network_receive_system` - Receive `GameState` and update sprites

### Server (`server.rs`)
- **WebSocket Server** on port 8080
- **Physics Simulation** at 60 FPS
- **Event-based API** (connect/disconnect/messages)
- **Shared Simulation** (reuses `mokosh-examples-shared` crate)

### Simulation (`mokosh-examples-shared::platformer`)
- **PlatformerSimulation**: Shared physics logic (WASM-compatible)
- **Messages**:
  - `PlayerInput` (route_id=100): Client → Server
  - `GameState` (route_id=101): Server → Client
- **Physics**: Gravity, jumping, box pushing, collision detection

## Quick Start

### Native Clients

**Terminal 1: Start Server**
```bash
cargo run --example bevy_platformer_server --features native
```

**Terminal 2: Start Native Client**
```bash
cargo run --example bevy_platformer_client --features native
```

**Terminal 3: Start Second Client (Optional)**
```bash
cargo run --example bevy_platformer_client --features native
```

### Web Client (WASM)

**Terminal 1: Start Server**
```bash
cargo run --example bevy_platformer_server --features native
```

**Terminal 2: Build and Serve WASM Client (Trunk)**
```bash
trunk serve --example bevy_platformer_client_wasm --no-default-features --features wasm
```

Then open **http://localhost:8000** in your browser!

Install Trunk with `cargo install trunk` and add the WASM target with
`rustup target add wasm32-unknown-unknown`. Run Trunk from the repository root;
[Trunk.toml](../../Trunk.toml) points to [web/index.html](web/index.html).

### UDP Clients

```bash
# Terminal 1
cargo run --example bevy_platformer_server_udp

# Terminal 2
cargo run --example bevy_platformer_client_udp
```

The UDP pair enables reliability and authenticated datagrams using the shared
demonstration PSK in [examples-shared](../../crates/examples-shared/src/lib.rs).

## Controls

- **Arrow Keys** / **WASD**: Move left/right
- **Space**: Jump

## Visual Guide

- **Blue Square**: Your player (local)
- **Red Squares**: Other players
- **Brown Squares**: Pushable boxes
- **Dark Gray Line**: Ground

## Implementation Details

### Networking Flow

1. **Connection**:
   - Client connects to `ws://127.0.0.1:8080`
   - Receives `SessionId` from server
   - Player spawns at default position

2. **Input → Server**:
   - `input_system` captures keyboard input
   - `input_system` sends `PlayerInput` (JSON codec)
   - Server applies input to player

3. **Server → Visuals**:
   - Server broadcasts `GameState` at 60 FPS
   - `network_receive_system` spawns/despawns entities
   - `network_receive_system` updates sprite positions

### ECS Design

**Components**:
- `PlayerEntity` - Player marker; IDs live in the `GameEntities` resource
- `BoxEntity` - Box marker; IDs live in the `GameEntities` resource
- `LocalPlayerMarker` - Tag for local player (blue color)
- `GroundLine` - Ground marker

**Resources**:
- `NetworkClient` - WebSocket channels
- `GameEntities` - HashMap of entity IDs for players/boxes
- `LocalSessionId` - Session ID assigned by the server
- `MessageCounter` - Counter for outgoing message IDs

### Physics Constants

- **Gravity**: 980 pixels/s²
- **Jump Velocity**: -400 pixels/s
- **Move Speed**: 200 pixels/s
- **Ground Level**: Y=500

## Comparison with Godot Example

| Feature | Godot Example | Bevy Example |
|---------|---------------|--------------|
| **Client** | GDScript + GDExtension | Rust (Bevy ECS) |
| **Rendering** | Godot 4 | Bevy 2D sprites |
| **Input** | Godot Input API | Bevy `ButtonInput<KeyCode>` |
| **Server** | [platformer/server.rs](../platformer/server.rs) | [server.rs](server.rs) |
| **Simulation** | [Shared platformer simulation](../../crates/examples-shared/src/platformer.rs) | Same shared simulation |

## Dependencies

- `bevy = "0.18"` - Game engine
- `mokosh-client` - Networking client
- `mokosh-server` - Networking server
- `mokosh-simulation` - Shared simulation trait
- `tokio` - Async runtime
- `serde_json` - JSON serialization

## Troubleshooting

**Server not starting?**
- Check port 8080 is not in use: `lsof -i :8080`
- Kill existing process: `kill -9 <PID>`

**Client can't connect?**
- Ensure server is running first
- Check firewall settings
- Verify `ws://127.0.0.1:8080` is accessible

**Visual glitches?**
- Server sends snapshots at 60 FPS
- Check the received snapshots and sprite updates in `network_receive_system`

## License

MIT
