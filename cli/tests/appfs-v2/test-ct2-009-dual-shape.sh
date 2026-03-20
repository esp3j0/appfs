#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

banner "AppFS v2 CT2-009 Snapshot/Live Dual Shape (Skeleton)"
say "  Pending issue #22: implement minimal dual-semantics read path baseline."
exit 2
