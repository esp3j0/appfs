#!/bin/sh
set -e

echo -n "TEST mount foreground reuse... "

TEST_AGENT_ID="test-mount-foreground-reuse-agent"
MOUNTPOINT="/tmp/agentfs-test-mount-reuse-$$"
LOG1="/tmp/agentfs-test-mount-reuse-1-$$.log"
LOG2="/tmp/agentfs-test-mount-reuse-2-$$.log"

cleanup() {
    if [ -n "${MOUNT_PID2:-}" ]; then
        kill "$MOUNT_PID2" 2>/dev/null || true
        wait "$MOUNT_PID2" 2>/dev/null || true
    fi
    if [ -n "${MOUNT_PID1:-}" ]; then
        kill "$MOUNT_PID1" 2>/dev/null || true
        wait "$MOUNT_PID1" 2>/dev/null || true
    fi
    fusermount -uz "$MOUNTPOINT" 2>/dev/null || true
    fusermount -u "$MOUNTPOINT" 2>/dev/null || true
    rmdir "$MOUNTPOINT" 2>/dev/null || true
    rm -f "$LOG1" "$LOG2"
    rm -f ".agentfs/${TEST_AGENT_ID}.db" ".agentfs/${TEST_AGENT_ID}.db-shm" ".agentfs/${TEST_AGENT_ID}.db-wal"
}

trap cleanup EXIT

mkdir -p "$MOUNTPOINT"
cargo run -- init "$TEST_AGENT_ID" > /dev/null 2>&1

# First foreground mount
cargo run -- mount ".agentfs/${TEST_AGENT_ID}.db" "$MOUNTPOINT" --foreground >"$LOG1" 2>&1 &
MOUNT_PID1=$!

MAX_WAIT=30
WAITED=0
while [ "$WAITED" -lt "$MAX_WAIT" ]; do
    if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        break
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

if ! mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
    echo "FAILED: first foreground mount did not become ready"
    tail -n 80 "$LOG1" || true
    exit 1
fi

# Hard-kill first process to intentionally create stale endpoint state.
kill -9 "$MOUNT_PID1" 2>/dev/null || true
wait "$MOUNT_PID1" 2>/dev/null || true
unset MOUNT_PID1

# Second foreground mount on the exact same path/id must succeed after auto recovery.
cargo run -- mount ".agentfs/${TEST_AGENT_ID}.db" "$MOUNTPOINT" --foreground >"$LOG2" 2>&1 &
MOUNT_PID2=$!

WAITED=0
while [ "$WAITED" -lt "$MAX_WAIT" ]; do
    # Avoid stale-mount races: require both mountpoint readiness and
    # foreground guidance log from the new process.
    if mountpoint -q "$MOUNTPOINT" 2>/dev/null && grep -q "Press Ctrl+C to unmount and exit." "$LOG2" 2>/dev/null; then
        break
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

if ! mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
    echo "FAILED: second foreground mount did not become ready"
    tail -n 120 "$LOG2" || true
    exit 1
fi

if ! grep -q "Press Ctrl+C to unmount and exit." "$LOG2"; then
    echo "FAILED: foreground mode did not print Ctrl+C guidance"
    tail -n 120 "$LOG2" || true
    exit 1
fi

# Graceful shutdown path.
kill -INT "$MOUNT_PID2" 2>/dev/null || true
wait "$MOUNT_PID2" 2>/dev/null || true
unset MOUNT_PID2

if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
    echo "FAILED: mountpoint still mounted after SIGINT shutdown"
    exit 1
fi

echo "OK"
