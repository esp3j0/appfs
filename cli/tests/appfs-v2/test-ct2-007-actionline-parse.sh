#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

banner "AppFS v2 CT2-007 ActionLineV2 Parse (Skeleton)"
say "  Pending issue #19: implement JSONL-only ActionLineV2 parser baseline."
exit 2
