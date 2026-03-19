#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-005 Manifest Policy Checks"

manifest="$APPFS_APP_DIR/_meta/manifest.res.json"
assert_file "$manifest"

if ! command -v jq >/dev/null 2>&1; then
    skip "jq not found; skip manifest policy checks"
fi

# Ensure node keys do not contain traversal or platform-unsafe patterns.
while IFS= read -r node_key; do
    printf '%s\n' "$node_key" | grep -q '\.\.' && fail "node key contains '..': $node_key"
    printf '%s\n' "$node_key" | grep -q '\\\\' && fail "node key contains backslash: $node_key"
    printf '%s\n' "$node_key" | grep -Eiq '(^|/)[A-Za-z]:($|/)' && fail "node key contains drive-letter segment: $node_key"
done <<EOF
$(jq -r '.nodes | keys[]' "$manifest")
EOF
pass "node keys pass basic safety policy"

action_count="$(jq -r '.nodes | to_entries[] | select(.value.kind=="action") | .key' "$manifest" | wc -l | tr -d ' ')"
[ "$action_count" -gt 0 ] || fail "manifest has no action nodes"
pass "manifest has action nodes: $action_count"

missing_action_mode="$(jq -r '.nodes | to_entries[] | select(.value.kind=="action") | select(.value.execution_mode==null) | .key' "$manifest" || true)"
[ -z "$missing_action_mode" ] || fail "action node missing execution_mode: $missing_action_mode"
pass "all action nodes declare execution_mode"

invalid_snapshot="$(jq -r '.nodes | to_entries[] | select(.value.kind=="resource") | select((.value.output_mode // "json")=="jsonl") | select((.value.snapshot.max_materialized_bytes // 0) <= 0 or (.value.paging.enabled // false)==true) | .key' "$manifest" || true)"
[ -z "$invalid_snapshot" ] || fail "snapshot jsonl resource policy violation: $invalid_snapshot"
pass "snapshot resources declare max_materialized_bytes and disable paging"

invalid_live_paging="$(jq -r '.nodes | to_entries[] | select(.value.kind=="resource") | select((.value.paging.enabled // false)==true) | select((.value.paging.mode // "snapshot") != "live") | .key' "$manifest" || true)"
[ -z "$invalid_live_paging" ] || fail "pageable resource must use paging.mode=live: $invalid_live_paging"
pass "pageable resources use paging.mode=live"

say "CT-005 done"
