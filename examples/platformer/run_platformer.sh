#!/bin/bash

# Run the 2D platformer demo
# 1. Start server
# 2. Start Godot client

echo "🎮 Starting 2D Platformer Demo..."

# Kill any existing processes
pkill -f "platformer_server|Godot" 2>/dev/null

# Start server in background
echo "🚀 Starting server..."
cargo run --example platformer_server 2>&1 &
SERVER_PID=$!

# Wait for server to start
sleep 2

# Start Godot client
echo "🎮 Starting Godot client..."
/Applications/Godot.app/Contents/MacOS/Godot --path examples/platformer/godot-client 2>&1 &

echo "✅ Demo started!"
echo "   Server PID: $SERVER_PID"
echo "   Use Ctrl+C to stop"

# Wait for server to finish
wait $SERVER_PID
