#!/bin/bash

# Mokosh Multiplayer Demo Launcher
# Launches server and multiple Godot clients for testing

set -e

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GODOT_PROJECT="$PROJECT_DIR/examples/platformer/godot-client"
GODOT_BIN="${GODOT_BIN:-godot}"

# Default values
NUM_CLIENTS=2
RUN_DURATION=30
HEADLESS=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -c|--clients)
            NUM_CLIENTS="$2"
            shift 2
            ;;
        -d|--duration)
            RUN_DURATION="$2"
            shift 2
            ;;
        --headless)
            HEADLESS=true
            shift
            ;;
        -h|--help)
            echo "Mokosh Multiplayer Demo Launcher"
            echo ""
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -c, --clients NUM    Number of Godot clients (default: 2)"
            echo "  -d, --duration SEC   Run duration in seconds (default: 30)"
            echo "  --headless           Run Godot in headless mode"
            echo "  -h, --help           Show this help"
            echo ""
            echo "Examples:"
            echo "  $0                   # Start server + 2 clients for 30s"
            echo "  $0 -c 4              # Start server + 4 clients"
            echo "  $0 -c 3 -d 60        # Start server + 3 clients for 60s"
            echo "  $0 --headless        # Run in headless mode (no GUI)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Use -h or --help for usage information"
            exit 1
            ;;
    esac
done

# Cleanup function
cleanup() {
    echo ""
    echo "🧹 Cleaning up..."

    # Kill server if running
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "  Stopping server (PID: $SERVER_PID)"
        kill "$SERVER_PID" 2>/dev/null || true
    fi

    # Kill all Godot clients
    pkill -f "Godot.*$GODOT_PROJECT" 2>/dev/null || true

    # Free port 8080
    lsof -ti:8080 | xargs kill -9 2>/dev/null || true

    echo "✅ Cleanup complete"
}

# Set trap for cleanup on exit
trap cleanup EXIT INT TERM

echo "🎮 Mokosh Multiplayer Demo"
echo "=========================="
echo "📁 Project: $GODOT_PROJECT"
echo "👥 Clients: $NUM_CLIENTS"
echo "⏱️  Duration: ${RUN_DURATION}s"
echo "🖥️  Mode: $([ "$HEADLESS" = true ] && echo "Headless" || echo "GUI")"
echo ""

# Start server
echo "🚀 Starting Rust server..."
cd "$PROJECT_DIR"
cargo run --example platformer_server --quiet 2>&1 &
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

# Launch Godot clients
echo "🎮 Starting $NUM_CLIENTS Godot client(s)..."
CLIENT_PIDS=()

for i in $(seq 1 $NUM_CLIENTS); do
    DELAY=$((2 * (i - 1)))

    if [ "$HEADLESS" = true ]; then
        (sleep $DELAY && "$GODOT_BIN" --headless --path "$GODOT_PROJECT" 2>&1 > /dev/null) &
    else
        (sleep $DELAY && "$GODOT_BIN" --path "$GODOT_PROJECT" 2>&1 > /dev/null) &
    fi

    CLIENT_PID=$!
    CLIENT_PIDS+=($CLIENT_PID)
    echo "  Client $i: PID $CLIENT_PID (starting in ${DELAY}s)"
done

echo ""
echo "⏳ Running for ${RUN_DURATION} seconds..."
echo "   Press Ctrl+C to stop early"
echo ""

# Wait for test duration with progress
for i in $(seq 1 $RUN_DURATION); do
    if [ $((i % 10)) -eq 0 ]; then
        echo -n "[$i/${RUN_DURATION}s] "
    else
        echo -n "."
    fi
    sleep 1
done

echo ""
echo ""
echo "📊 Test Summary"
echo "==============="

# Check server status
if kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "✅ Server still running"
else
    echo "❌ Server crashed"
fi

# Check client status
ALIVE_COUNT=0
for i in "${!CLIENT_PIDS[@]}"; do
    PID=${CLIENT_PIDS[$i]}
    CLIENT_NUM=$((i + 1))
    if kill -0 "$PID" 2>/dev/null; then
        echo "✅ Client $CLIENT_NUM still running"
        ALIVE_COUNT=$((ALIVE_COUNT + 1))
    else
        echo "❌ Client $CLIENT_NUM crashed"
    fi
done

echo ""
echo "📈 Results: $ALIVE_COUNT/$NUM_CLIENTS clients alive after ${RUN_DURATION}s"

if [ "$ALIVE_COUNT" -eq "$NUM_CLIENTS" ]; then
    echo "🎉 Test PASSED - All clients stable!"
    exit 0
else
    echo "⚠️  Test FAILED - Some clients crashed"
    exit 1
fi
