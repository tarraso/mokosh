#!/bin/bash

# Run the 2D platformer demo
# 1. Start server
# 2. Start Godot client

echo "🎮 Starting 2D Platformer Demo..."

# Kill any existing processes
pkill -f "platformer_server|Godot" 2>/dev/null

# Determine paths based on where script is run from
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Start server in background (from repo root)
echo "🚀 Starting server..."
cd "$REPO_ROOT"
cargo run --example platformer_server > /tmp/platformer_server.log 2>&1 &
SERVER_PID=$!

# Wait for server to start
sleep 2

# Start Godot client
echo "🎮 Starting Godot client..."
/Applications/Godot.app/Contents/MacOS/Godot --path "$SCRIPT_DIR/godot-client" > /tmp/platformer_client.log 2>&1 &
GODOT_PID=$!

echo "✅ Demo started!"
echo "   Server PID: $SERVER_PID"
echo "   Godot PID: $GODOT_PID"
echo "   Server log: /tmp/platformer_server.log"
echo "   Client log: /tmp/platformer_client.log"
echo ""
echo "   Use Ctrl+C to stop"

# Wait for either process to finish
wait $SERVER_PID $GODOT_PID
