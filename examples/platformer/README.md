# 2D Platformer Example

Multiplayer platformer with a Rust authoritative server and a Godot 4 client. The server simulates gravity, jumping, collision and pushable boxes. The Godot client sends inputs and displays received snapshots.

## Structure

```text
examples/platformer/
├── godot-client/      # Godot project and GDScript client
├── server.rs          # WebSocket server
├── server_udp.rs      # UDP server
├── client_udp.rs      # Headless UDP client
├── run_platformer.sh  # WebSocket server and Godot client launcher
└── README.md
```

Physics and message types are implemented in [crates/examples-shared/src/platformer.rs](../../crates/examples-shared/src/platformer.rs).

## Godot Quick Start

Run these commands from the repository root. The bindings use the Godot 4.6 API.

```bash
cargo build -p mokosh-bindings

# Terminal 1
cargo run --example platformer_server

# Terminal 2, with Godot on PATH
godot --path examples/platformer/godot-client
```

The launcher [run_platformer.sh](run_platformer.sh) starts the server and Godot together:

```bash
./examples/platformer/run_platformer.sh
```

The root launchers [run_demo.sh](../../run_demo.sh) and [test_multi_client.sh](../../test_multi_client.sh) use the same server and Godot project. Set `GODOT_BIN` if the Godot executable is not on PATH.

## UDP Quick Start

```bash
# Terminal 1
cargo run --example platformer_server_udp

# Terminal 2
cargo run --example platformer_client_udp
```

Both UDP examples use reliability and the shared demonstration PSK from [examples-shared](../../crates/examples-shared/src/lib.rs). The graphical UDP client is in the [Bevy example](../platformer_bevy/README.md).

## Messages

- `PlayerInput`, route 100: movement and jump input from client to server.
- `GameState`, route 101: world snapshots from server to client.

The Godot wrapper [platformer_client.gd](godot-client/scripts/platformer_client.gd) sends JSON through `NetClient.send_message()` and handles the `message_received` signal.

## Physics

`PlatformerSimulation` implements the `Simulation` trait and runs on the server.

- Gravity: 980 pixels/s².
- Jump velocity: -400 pixels/s.
- Move speed: 200 pixels/s.
- AABB collision detection and box pushing.

Prediction and reconciliation helpers are demonstrated separately in [prediction_demo.rs](../prediction_demo.rs).

## Controls

- Arrow keys / WASD: move left or right.
- Space: jump.
