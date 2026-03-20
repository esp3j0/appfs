#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

banner "AppFS v2 CT2-002 Snapshot Read Hit (Skeleton)"
say "  Pending issue #21: implement single-resource snapshot hit/miss read-through baseline."
exit 2
