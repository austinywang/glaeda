#!/bin/bash
# Keep this reader's node warm between builds: copy the writer's newest fill into the node
# store, so a catch-up build's own warm (fleet-cas-warm.sh) finds nearly everything local.
# Runs from a LaunchAgent every 5 minutes (glaeda-fleet-cas node --prewarm REPO).
#
# usage: fleet-cas-prewarm.sh REPO
# The writer names its newest fill in the signed marker REPO/latest/<Xcode build>. Nearby
# commits share almost all keys, so a build a few commits away fetches only the difference.
# Skips while any build or CI job holds the host lock (never waits for it). Warm is re-run
# every tick: with everything local it only reads and checks entries (under a second), and it
# refetches whatever glaeda-fleet-cas-prune evicted. Pruning the node store stays with
# glaeda-fleet-cas-prune (hourly, deferred while anything builds).
# Exit: warm's code (0 warmed, 1 no marker, 2 unverified, 3 incomplete); 0 when skipped.
set -u
repo=${1:?usage: fleet-cas-prewarm.sh REPO}
ROOT=${FLEET_CAS_ROOT:-/Users/Shared/cmux-build-fleet/xcode}
LOCK=${FLEET_HOST_LOCK:-/Users/Shared/cmux-build-fleet/host.lock}
env_get() { sed -n "s/^$1=//p" "$ROOT/fleet-cas.env" 2>/dev/null | tail -1; }
store=$(env_get FLEET_CAS_STORE) keys=$(env_get FLEET_CAS_TRUSTED_KEYS)
[ -n "$store" ] && [ -n "$keys" ] || { echo "$ROOT/fleet-cas.env has no store or trusted keys" >&2; exit 2; }
say() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*"; }
# Fleet builds hold the lock exclusively and CI jobs on multi-runner hosts hold it shared, so
# only an exclusive, non-blocking probe (released at once, as cmux_mini_probe.sh does) sees
# both. Absent lock file: nothing on this host takes it.
if [ -e "$LOCK" ] && ! perl -MFcntl=:flock -e 'open(my $f, "<", shift) or exit 0; exit(flock($f, LOCK_EX | LOCK_NB) ? 0 : 1)' "$LOCK"; then
  exit 0
fi
xcode=$(xcodebuild -version 2>/dev/null | awk '/Build version/ {print $3}')
[ -n "$xcode" ] || { say "no Xcode build version"; exit 2; }
latest=$("$ROOT/bin/fleet-cas" marker get "http://$store" "$repo/latest/$xcode" --trusted-keys "$keys" 2>/dev/null)
rc=$?
[ $rc -eq 1 ] && exit 0  # no fill for this Xcode yet
[ $rc -eq 0 ] || { say "latest marker for $repo/$xcode did not verify or the store is unreachable ($rc)"; exit 2; }
commit=$(printf '%s\n' "$latest" | sed -n 's/^commit=//p')
case $commit in *[!0-9a-f]* | "") say "latest marker names no commit"; exit 2 ;; esac
out=$(nice -n 10 "$ROOT/bin/fleet-cas" warm "http://$store" "$repo/$commit/$xcode" \
  --trusted-keys "$keys" --store "$ROOT/node-store" --timeout 900 2>&1)
rc=$?
# Log only what changed something or failed: an all-local tick is the normal case.
case $out in *" 0 fetched, 0 missing), 0 objects"*) [ $rc -eq 0 ] || say "$out" ;; *) say "$out" ;; esac
exit $rc
