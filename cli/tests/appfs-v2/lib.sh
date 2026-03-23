#!/bin/sh
set -eu

say() {
    printf '%s\n' "$*"
}

pass() {
    say "  OK   $*"
}

fail() {
    say "  FAIL $*"
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "missing command: $1"
}

endpoint_host_port() {
    endpoint="$1"
    trimmed="${endpoint#*://}"
    authority="${trimmed%%/*}"
    host="${authority%%:*}"
    port="${authority##*:}"
    if [ -z "$host" ] || [ -z "$port" ] || [ "$port" = "$authority" ]; then
        fail "invalid bridge endpoint (expected scheme://host:port): $endpoint"
    fi
    printf '%s %s\n' "$host" "$port"
}

wait_tcp_ready() {
    host="$1"
    port="$2"
    timeout="${3:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if command -v python3 >/dev/null 2>&1; then
            if python3 - "$host" "$port" <<'PY' >/dev/null 2>&1
import socket
import sys

host = sys.argv[1]
port = int(sys.argv[2])
sock = socket.socket()
sock.settimeout(1.0)
try:
    sock.connect((host, port))
    sys.exit(0)
except OSError:
    sys.exit(1)
finally:
    sock.close()
PY
            then
                return 0
            fi
        elif command -v nc >/dev/null 2>&1; then
            if nc -z "$host" "$port" >/dev/null 2>&1; then
                return 0
            fi
        else
            fail "missing python3 or nc for bridge readiness check"
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

wait_bridge_endpoint_ready() {
    timeout="${APPFS_ADAPTER_BRIDGE_WAIT_SEC:-20}"

    if [ -n "${APPFS_ADAPTER_HTTP_ENDPOINT:-}" ] && [ -n "${APPFS_ADAPTER_GRPC_ENDPOINT:-}" ]; then
        fail "APPFS_ADAPTER_HTTP_ENDPOINT and APPFS_ADAPTER_GRPC_ENDPOINT are mutually exclusive"
    fi

    if [ -n "${APPFS_ADAPTER_HTTP_ENDPOINT:-}" ]; then
        set -- $(endpoint_host_port "$APPFS_ADAPTER_HTTP_ENDPOINT")
        wait_tcp_ready "$1" "$2" "$timeout" || fail "http bridge endpoint not ready: $APPFS_ADAPTER_HTTP_ENDPOINT"
    fi

    if [ -n "${APPFS_ADAPTER_GRPC_ENDPOINT:-}" ]; then
        set -- $(endpoint_host_port "$APPFS_ADAPTER_GRPC_ENDPOINT")
        wait_tcp_ready "$1" "$2" "$timeout" || fail "grpc bridge endpoint not ready: $APPFS_ADAPTER_GRPC_ENDPOINT"
    fi
}

stop_windows_agentfs_for_root() {
    bin_path="${1:-}"
    root_dir="${2:-}"
    case "$bin_path" in
        *.exe) ;;
        *)
            return 0
            ;;
    esac
    [ -n "$root_dir" ] || return 0
    command -v powershell.exe >/dev/null 2>&1 || return 0

    win_root="$root_dir"
    if command -v wslpath >/dev/null 2>&1; then
        win_root="$(wslpath -w "$root_dir")"
    fi

    WIN_ROOT="$win_root" powershell.exe -NoProfile -Command "\$root = \$env:WIN_ROOT; Get-CimInstance Win32_Process -Filter \"Name = 'agentfs.exe'\" | Where-Object { \$_.CommandLine -like ('*--root ' + \$root + '*') } | ForEach-Object { Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue }" >/dev/null 2>&1 || true
}

stop_adapter_process() {
    pid="${1:-}"
    bin_path="${2:-}"
    root_dir="${3:-}"

    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi

    stop_windows_agentfs_for_root "$bin_path" "$root_dir"
}

assert_file() {
    path="$1"
    [ -f "$path" ] || fail "missing file: $path"
    pass "file: $path"
}

resolve_cargo_cmd() {
    if command -v cargo >/dev/null 2>&1; then
        printf '%s\n' "cargo"
        return 0
    fi
    if command -v cargo.exe >/dev/null 2>&1; then
        printf '%s\n' "cargo.exe"
        return 0
    fi
    if [ -x "/mnt/c/Users/esp3j/.cargo/bin/cargo.exe" ]; then
        printf '%s\n' "/mnt/c/Users/esp3j/.cargo/bin/cargo.exe"
        return 0
    fi
    fail "cargo not found (checked cargo, cargo.exe, /mnt/c/Users/esp3j/.cargo/bin/cargo.exe)"
}

ensure_agentfs_bin() {
    cli_dir="${1:-${CLI_DIR:-}}"
    [ -n "$cli_dir" ] || fail "CLI_DIR is required for ensure_agentfs_bin"

    if [ -n "${AGENTFS_BIN:-}" ] && [ -f "$AGENTFS_BIN" ]; then
        return 0
    fi

    build_before_run="${APPFS_V2_BUILD_BEFORE_RUN:-1}"
    cargo_cmd="$(resolve_cargo_cmd)"

    if [ "$build_before_run" = "1" ]; then
        case "$cargo_cmd" in
            cargo)
                say "Building Linux agentfs binary for AppFS v2 contract tests..."
                ;;
            *)
                say "Building Windows agentfs binary for AppFS v2 contract tests..."
                ;;
        esac
        (cd "$cli_dir" && "$cargo_cmd" build --quiet) || fail "failed to build agentfs with $cargo_cmd"
    fi

    linux_bin="$cli_dir/target/debug/agentfs"
    windows_bin="$cli_dir/target/debug/agentfs.exe"

    case "$cargo_cmd" in
        cargo)
            if [ -f "$linux_bin" ]; then
                AGENTFS_BIN="$linux_bin"
                return 0
            fi
            ;;
        *)
            if [ -f "$windows_bin" ]; then
                AGENTFS_BIN="$windows_bin"
                return 0
            fi
            ;;
    esac

    if [ -f "$linux_bin" ]; then
        AGENTFS_BIN="$linux_bin"
        return 0
    fi
    if [ -f "$windows_bin" ]; then
        AGENTFS_BIN="$windows_bin"
        return 0
    fi

    fail "missing agentfs binary; expected $linux_bin or $windows_bin"
}

start_appfs_v2_adapter() {
    adapter_log="$1"
    bin_path="$2"
    root_dir="$3"
    app_id="$4"
    poll_ms="${5:-50}"
    strict_actionline="${6:-0}"
    snapshot_expand_delay_ms="${7:-}"
    snapshot_publish_delay_ms="${8:-}"
    snapshot_refresh_force_expand="${9:-}"

    wait_bridge_endpoint_ready

    bridge_args=""
    if [ -n "${APPFS_ADAPTER_HTTP_ENDPOINT:-}" ]; then
        bridge_args="$bridge_args --adapter-http-endpoint ${APPFS_ADAPTER_HTTP_ENDPOINT} --adapter-http-timeout-ms ${APPFS_ADAPTER_HTTP_TIMEOUT_MS:-5000}"
    fi
    if [ -n "${APPFS_ADAPTER_GRPC_ENDPOINT:-}" ]; then
        bridge_args="$bridge_args --adapter-grpc-endpoint ${APPFS_ADAPTER_GRPC_ENDPOINT} --adapter-grpc-timeout-ms ${APPFS_ADAPTER_GRPC_TIMEOUT_MS:-5000}"
    fi
    if [ -n "${APPFS_ADAPTER_BRIDGE_MAX_RETRIES:-}" ]; then
        bridge_args="$bridge_args --adapter-bridge-max-retries ${APPFS_ADAPTER_BRIDGE_MAX_RETRIES}"
    fi
    if [ -n "${APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS:-}" ]; then
        bridge_args="$bridge_args --adapter-bridge-initial-backoff-ms ${APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS}"
    fi
    if [ -n "${APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS:-}" ]; then
        bridge_args="$bridge_args --adapter-bridge-max-backoff-ms ${APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS}"
    fi
    if [ -n "${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES:-}" ]; then
        bridge_args="$bridge_args --adapter-bridge-circuit-breaker-failures ${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES}"
    fi
    if [ -n "${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS:-}" ]; then
        bridge_args="$bridge_args --adapter-bridge-circuit-breaker-cooldown-ms ${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS}"
    fi

    runtime_root="$root_dir"
    case "$bin_path" in
        *.exe)
            win_bin="$bin_path"
            if command -v wslpath >/dev/null 2>&1; then
                runtime_root="$(wslpath -w "$root_dir")"
                win_bin="$(wslpath -w "$bin_path")"
            elif command -v cygpath >/dev/null 2>&1; then
                runtime_root="$(cygpath -w "$root_dir")"
                win_bin="$(cygpath -w "$bin_path")"
            fi
            cmd_prefix=""
            if [ "$strict_actionline" = "1" ]; then
                cmd_prefix="${cmd_prefix}set APPFS_V2_ACTIONLINE_STRICT=1&& "
            fi
            if [ -n "$snapshot_expand_delay_ms" ]; then
                cmd_prefix="${cmd_prefix}set APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS=$snapshot_expand_delay_ms&& "
            fi
            if [ -n "$snapshot_publish_delay_ms" ]; then
                cmd_prefix="${cmd_prefix}set APPFS_V2_SNAPSHOT_PUBLISH_DELAY_MS=$snapshot_publish_delay_ms&& "
            fi
            if [ -n "$snapshot_refresh_force_expand" ]; then
                cmd_prefix="${cmd_prefix}set APPFS_V2_SNAPSHOT_REFRESH_FORCE_EXPAND=$snapshot_refresh_force_expand&& "
            fi
            cmd.exe /C "${cmd_prefix}$win_bin serve appfs --root $runtime_root --app-id $app_id --poll-ms $poll_ms$bridge_args" >"$adapter_log" 2>&1 &
            ;;
        *)
            env_args=""
            if [ "$strict_actionline" = "1" ]; then
                env_args="$env_args APPFS_V2_ACTIONLINE_STRICT=1"
            fi
            if [ -n "$snapshot_expand_delay_ms" ]; then
                env_args="$env_args APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS=$snapshot_expand_delay_ms"
            fi
            if [ -n "$snapshot_publish_delay_ms" ]; then
                env_args="$env_args APPFS_V2_SNAPSHOT_PUBLISH_DELAY_MS=$snapshot_publish_delay_ms"
            fi
            if [ -n "$snapshot_refresh_force_expand" ]; then
                env_args="$env_args APPFS_V2_SNAPSHOT_REFRESH_FORCE_EXPAND=$snapshot_refresh_force_expand"
            fi
            # shellcheck disable=SC2086
            env $env_args "$bin_path" serve appfs --root "$runtime_root" --app-id "$app_id" --poll-ms "$poll_ms" $bridge_args >"$adapter_log" 2>&1 &
            ;;
    esac

    adapter_pid=$!
    sleep 1
    if ! kill -0 "$adapter_pid" 2>/dev/null; then
        tail -n 120 "$adapter_log" 2>/dev/null || true
        fail "appfs adapter failed to start"
    fi

    printf '%s\n' "$adapter_pid"
}

banner() {
    say "================================================"
    say "  $1"
    say "================================================"
}
