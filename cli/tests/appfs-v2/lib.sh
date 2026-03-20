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
