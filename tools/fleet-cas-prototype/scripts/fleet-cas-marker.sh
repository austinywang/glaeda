#!/bin/bash
# Does the fleet store hold COMMIT for this host's Xcode? Checks the signed
# per-commit marker the trusted writer publishes after a complete fill.
#
# usage: fleet-cas-marker.sh REPO COMMIT
# exit 0: held (marker entries printed), 1: not held, 2: unverified or error.
# Pick catch-up mode only on 0 (docs/CMUX_BUILD_SLOT_MODES.md, "When to switch").
set -u
repo=${1:?usage: fleet-cas-marker.sh REPO COMMIT}
commit=${2:?usage: fleet-cas-marker.sh REPO COMMIT}
ROOT=${FLEET_CAS_ROOT:-/Users/Shared/cmux-build-fleet/xcode}
env_get() { sed -n "s/^$1=//p" "$ROOT/fleet-cas.env" 2>/dev/null | tail -1; }
store=$(env_get FLEET_CAS_STORE) keys=$(env_get FLEET_CAS_TRUSTED_KEYS)
[ -n "$store" ] && [ -n "$keys" ] || { echo "$ROOT/fleet-cas.env has no store or trusted keys" >&2; exit 2; }
xcode=$(xcodebuild -version 2>/dev/null | awk '/Build version/ {print $3}')
[ -n "$xcode" ] || { echo "no Xcode build version" >&2; exit 2; }
exec "$ROOT/bin/fleet-cas" marker get "http://$store" "$repo/$commit/$xcode" --trusted-keys "$keys"
