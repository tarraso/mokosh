#!/bin/bash

# Test script for multi-client server with auto-stop
# Launches the platformer server and two headless Godot clients

set -e

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GODOT_PROJECT="$PROJECT_DIR/examples/platformer/godot-client"
GODOT_BIN="${GODOT_BIN:-godot}"
TEST_DURATION=10

echo "🧪 Multi-client test starting..."
echo "📁 Project: $GODOT_PROJECT"
echo "⏱️  Duration: ${TEST_DURATION}s"
echo ""

# Cleanup function
cleanup() {
    echo ""
    echo "🧹 Cleaning up processes..."

    # Kill server if running
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "  Stopping server (PID: $SERVER_PID)"
        kill "$SERVER_PID" 2>/dev/null || true
    fi

    # Kill all Godot processes from this test
    pkill -f "Godot.*$GODOT_PROJECT" 2>/dev/null || true

    # Free port 8080
    lsof -ti:8080 | xargs kill -9 2>/dev/null || true

    echo "✅ Cleanup complete"
}

# Set trap for cleanup on exit
trap cleanup EXIT INT TERM

# Start server in background
echo "🚀 Starting Rust server..."
cd "$PROJECT_DIR"
cargo run --example platformer_server --quiet &
SERVER_PID=$!
echo "  Server PID: $SERVER_PID"

# Wait for server to be ready
sleep 2

# Check if server is running
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "❌ Server failed to start!"
    exit 1
fi

echo "✅ Server is running on ws://127.0.0.1:8080"
echo ""

# Start first Godot client
echo "🎮 Starting Client 1..."
"$GODOT_BIN" --headless --path "$GODOT_PROJECT" > /tmp/godot_client1.log 2>&1 &
CLIENT1_PID=$!
echo "  Client 1 PID: $CLIENT1_PID"

sleep 1

# Start second Godot client
echo "🎮 Starting Client 2..."
"$GODOT_BIN" --headless --path "$GODOT_PROJECT" > /tmp/godot_client2.log 2>&1 &
CLIENT2_PID=$!
echo "  Client 2 PID: $CLIENT2_PID"

echo ""
echo "⏳ Running test for ${TEST_DURATION} seconds..."
echo ""

# Wait for test duration
for i in $(seq 1 $TEST_DURATION); do
    echo -n "."
    sleep 1
done

echo ""
echo ""
echo "📊 Test Results:"
echo "==============="

# Show the server process
echo ""
echo "Server process:"
ps aux | grep "platformer_server" | grep -v grep | head -5

# Check if clients are still running
if kill -0 "$CLIENT1_PID" 2>/dev/null; then
    echo "✅ Client 1 still running"
else
    echo "❌ Client 1 crashed"
fi

if kill -0 "$CLIENT2_PID" 2>/dev/null; then
    echo "✅ Client 2 still running"
else
    echo "❌ Client 2 crashed"
fi

# Show client connection logs
echo ""
echo "Client 1 log:"
tail -n 10 /tmp/godot_client1.log

echo "Client 2 log:"
tail -n 10 /tmp/godot_client2.log

echo ""
echo "Test run finished after ${TEST_DURATION}s. Check process status and logs above."
