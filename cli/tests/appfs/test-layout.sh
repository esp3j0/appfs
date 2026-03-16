#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-001 Layout and Required Nodes"

assert_exists "$APPFS_APP_DIR"
assert_file "$APPFS_APP_DIR/_meta/manifest.res.json"
assert_file "$APPFS_APP_DIR/_meta/context.res.json"
assert_file "$APPFS_APP_DIR/_meta/permissions.res.json"
assert_exists "$APPFS_APP_DIR/_meta/schemas"

assert_file "$APPFS_APP_DIR/_stream/events.evt.jsonl"
assert_file "$APPFS_APP_DIR/_stream/cursor.res.json"
assert_exists "$APPFS_APP_DIR/_stream/from-seq"

assert_file "$APPFS_APP_DIR/_paging/fetch_next.act"
assert_file "$APPFS_APP_DIR/_paging/close.act"

if command -v jq >/dev/null 2>&1; then
    assert_json_key "$APPFS_APP_DIR/_meta/manifest.res.json" ".app_id"
    assert_json_key "$APPFS_APP_DIR/_meta/manifest.res.json" ".nodes"
fi

say "CT-001 done"
