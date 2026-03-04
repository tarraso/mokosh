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
- **Components**: `PlayerEntity`, `BoxEntity`, `LocalPlayer`
- **Resources**: `NetworkClient`, `GameEntities`, `InputState`
- **Systems**:
  - `setup_system` - Initialize camera and ground
  - `input_system` - Capture keyboard input
  - `network_send_system` - Send `PlayerInput` to server
  - `network_receive_system` - Receive `GameState` from server
  - `update_visual_system` - Update sprite positions

### Server (`server.rs`)
- **WebSocket Server** on port 8080
- **Physics Simulation** at 60 FPS
- **Event-based API** (connect/disconnect/messages)
- **Shared Simulation** (reuses `../platformer/simulation.rs`)

### Simulation (`../platformer/simulation.rs`)
- **PlatformerSimulation**: Shared physics logic
- **Messages**:
  - `PlayerInput` (route_id=100): Client → Server
  - `GameState` (route_id=101): Server → Client
- **Physics**: Gravity, jumping, box pushing, collision detection

## Quick Start

### Terminal 1: Start Server
```bash
cargo run --example bevy_platformer_server
```

### Terminal 2: Start Client
```bash
cargo run --example bevy_platformer_client
```

### Terminal 3: Start Second Client (Optional)
```bash
cargo run --example bevy_platformer_client
```

## Controls

- **Arrow Keys** / **WASD**: Move left/right
- **Space**: Jump
- **ESC**: Close window

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
   - `network_send_system` sends `PlayerInput` (JSON codec)
   - Server applies input to player

3. **Server → Visuals**:
   - Server broadcasts `GameState` at 60 FPS
   - `network_receive_system` spawns/despawns entities
   - `update_visual_system` updates sprite positions

### ECS Design

**Components**:
- `PlayerEntity { session_id: String }` - Player marker with ID
- `BoxEntity { box_id: u32 }` - Box marker with ID
- `LocalPlayer` - Tag for local player (blue color)

**Resources**:
- `NetworkClient` - WebSocket channels and session ID
- `GameEntities` - HashMap of entity IDs for players/boxes
- `InputState` - Current keyboard state

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
| **Server** | Same (`platformer/server.rs`) | Same (shared code) |
| **Simulation** | Same (`platformer/simulation.rs`) | Same (shared code) |

## Future Enhancements

- **Client-side Prediction**: Reduce input latency
- **Interpolation**: Smooth movement between snapshots
- **Camera Follow**: Track local player
- **Sprites/Textures**: Replace colored squares
- **Sound Effects**: Jump, box push, etc.
- **UI Overlay**: Show FPS, ping, player count

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
- Add interpolation for smoother movement
- Consider client-side prediction

## License

MIT
