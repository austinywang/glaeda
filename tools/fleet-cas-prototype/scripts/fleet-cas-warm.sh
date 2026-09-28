#!/bin/bash
# Copy a commit's whole fill into this host's node before a catch-up build, so
# the build's index lookups are local hits instead of one fleet-store round
# trip each (fleet-cas warm, src/warm.rs).
#
# usage: fleet-cas-warm.sh REPO COMMIT
# exit 0: warmed; 1: no marker, or a marker without a manifest; 2: something
# did not verify (a marker, entry, manifest or object); 3: incomplete (store
# unreachable, busy or failing, or not done within --timeout, 150 s). On 3
# nothing was written that would slow a build: entries are written only after
# their objects. Run it after fleet-cas-marker.sh returns 0, as the node's user
# (it writes into the node's store).
set -u
repo=${1:?usage: fleet-cas-warm.sh REPO COMMIT}
commit=${2:?usage: fleet-cas-warm.sh REPO COMMIT}
ROOT=${FLEET_CAS_ROOT:-/Users/Shared/cmux-build-fleet/xcode}
env_get() { sed -n "s/^$1=//p" "$ROOT/fleet-cas.env" 2>/dev/null | tail -1; }
store=$(env_get FLEET_CAS_STORE) keys=$(env_get FLEET_CAS_TRUSTED_KEYS)
[ -n "$store" ] && [ -n "$keys" ] || { echo "$ROOT/fleet-cas.env has no store or trusted keys" >&2; exit 2; }
xcode=$(xcodebuild -version 2>/dev/null | awk '/Build version/ {print $3}')
[ -n "$xcode" ] || { echo "no Xcode build version" >&2; exit 2; }
exec "$ROOT/bin/fleet-cas" warm "http://$store" "$repo/$commit/$xcode" --trusted-keys "$keys" \
  --store "$ROOT/node-store"
