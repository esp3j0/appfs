#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

banner "AppFS v2 CT2-008 Submit Reject Rules (Skeleton)"
say "  Pending issue #20: implement submit-time rejection and validation baseline."
exit 2
