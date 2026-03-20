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

banner() {
    say "================================================"
    say "  $1"
    say "================================================"
}
