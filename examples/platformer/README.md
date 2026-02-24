# 2D Platformer Example

Multiplayer 2D platformer demonstrating:
- **Shared simulation** between client and server (`crates/simulation/src/platformer.rs`)
- **Server-authoritative physics** with gravity, jumping, collision
- **Pushable boxes** that can be stacked
- **Client-side prediction** (coming soon - Godot integration)
- **Rust → Godot integration** via GDExtension

## Structure

```
examples/platformer/
├── godot-client/         # Godot 4 client (GDScript + Mokosh bindings)
├── run_platformer.sh     # Launcher script (server + clients)
└── README.md             # This file
examples/platformer_server.rs  # Server executable (Rust)
crates/simulation/src/platformer.rs  # Shared physics simulation
```

## Quick Start

```bash
# Option 1: Run with script (recommended)
./examples/platformer/run_platformer.sh

# Option 2: Manual
cargo run --example platformer_server  # Terminal 1
/Applications/Godot.app/Contents/MacOS/Godot --path examples/platformer/godot-client  # Terminal 2
```

## Architecture

### Shared Simulation
The physics simulation (`PlatformerSimulation`) implements the `Simulation` trait:
- **Server** runs authoritative simulation
- **Client** will run prediction (future: Godot integration)
- **Deterministic** physics ensures consistency

### Messages
- `PlayerInput` (route_id=100): Client → Server movement/jump
- `GameState` (route_id=101): Server → Client world snapshot

### Physics
- **Gravity**: 980 pixels/s²
- **Jump velocity**: -400 pixels/s
- **Move speed**: 200 pixels/s
- **Collision**: AABB-based with box pushing

## Future: Client-Side Prediction
```rust
// Server (authoritative)
let mut sim = PlatformerSimulation::new();
sim.apply_input(&input);
sim.step(delta_time);
let state = sim.snapshot();  // Broadcast to clients

// Client (prediction) - TODO: Implement in Godot
sim.apply_input(&input);      // Apply immediately (prediction)
sim.step(delta_time);          // Step locally
// On server snapshot: reconcile if diverged
```

## Controls
- **Arrow Keys** / **WASD**: Move left/right
- **Space**: Jump
