#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-003 Stream Replay and Cursor"

cursor="$APPFS_APP_DIR/_stream/cursor.res.json"
from_seq_dir="$APPFS_APP_DIR/_stream/from-seq"

assert_file "$cursor"
assert_exists "$from_seq_dir"

if ! command -v jq >/dev/null 2>&1; then
    skip "jq not found; skip json-level replay assertions"
fi

assert_json_key "$cursor" ".min_seq"
assert_json_key "$cursor" ".max_seq"
assert_json_key "$cursor" ".retention_hint_sec"

min_seq="$(jq -r '.min_seq' "$cursor")"
max_seq="$(jq -r '.max_seq' "$cursor")"

[ "$min_seq" != "null" ] || fail "cursor.min_seq is null"
[ "$max_seq" != "null" ] || fail "cursor.max_seq is null"

valid_seq="$max_seq"
valid_path="$from_seq_dir/$valid_seq.evt.jsonl"

assert_file "$valid_path"
[ -s "$valid_path" ] || fail "replay file is empty: $valid_path"
pass "valid replay file has content: $valid_path"

if [ "$min_seq" -gt 0 ] 2>/dev/null; then
    old_seq=$((min_seq - 1))
    old_path="$from_seq_dir/$old_seq.evt.jsonl"
    run_expect_fail cat "$old_path"
fi

say "CT-003 done"
