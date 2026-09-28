#!/bin/bash
# Run the trusted writer's build, then publish the per-commit marker only if
# every write reached the fleet store (#1134 M3).
#
# usage: fleet-cas-writer-build.sh REPO COMMIT -- BUILD COMMAND...
#
# Runs on the writer host, whose node daemon signs every index entry. The
# marker `REPO/COMMIT/<Xcode build version>` tells workers the fleet store
# holds that commit (fleet-cas-marker.sh). A build that succeeds while some
# writes failed (store down, refused, backing off) leaves the store partly
# filled: it gets no marker, and the next main build fills the rest.
# Settings come from /Users/Shared/cmux-build-fleet/xcode/fleet-cas.env, which
# `glaeda-fleet-cas writer` installs. The build command must add the plugin
# settings from `fleet-cas-settings.sh`. Exit: the build's code; 3 if the node
# is down (nothing is built); 4 if the build passed but no marker was written.
# The writer node keeps its own copy of what it wrote, so a fleet store that is
# emptied needs the writer's node store emptied too, or later markers can
# claim entries the store lost. The build must use the fixed local CAS
# ($ROOT/cas), which this script empties first: an entry whose upload failed
# could otherwise be answered from it and never uploaded again (not observed;
# how Xcode orders local and remote lookups is unverified). The writer's node
# store keeps the writer's own builds fast.
# The marker also names the fill's manifest: every index key the build looked
# up or wrote, from the lines the writer node appended to its key log during
# the build. Readers pass it to `fleet-cas warm` (fleet-cas-warm.sh) to copy
# the whole commit into their node before building.
set -u
repo=${1:?usage: fleet-cas-writer-build.sh REPO COMMIT -- CMD...}
commit=${2:?usage: fleet-cas-writer-build.sh REPO COMMIT -- CMD...}
[ "${3:-}" = -- ] || { echo "usage: fleet-cas-writer-build.sh REPO COMMIT -- CMD..." >&2; exit 2; }
shift 3
case $commit in *[!0-9a-f]* | "") echo "COMMIT must be a full hex SHA" >&2; exit 2 ;; esac
[ ${#commit} -eq 40 ] || { echo "COMMIT must be a full 40-character SHA" >&2; exit 2; }
ROOT=${FLEET_CAS_ROOT:-/Users/Shared/cmux-build-fleet/xcode}
env_get() { sed -n "s/^$1=//p" "$ROOT/fleet-cas.env" | tail -1; }
store=$(env_get FLEET_CAS_STORE) key=$(env_get FLEET_CAS_SIGN_KEY)
[ -n "$store" ] && [ -n "$key" ] || { echo "$ROOT/fleet-cas.env has no writer settings" >&2; exit 2; }
stats=$ROOT/node-store/stats.json
# Sum of the named node counters; empty unless every one is in the stats file.
count() {
  perl -e 'open my $f, "<", shift or exit 1; my $s = <$f>; my $n = 0;
    for my $k (@ARGV) { $s =~ /"$k":(\d+)/ or exit 1; $n += $1 } print $n' "$stats" "$@"
}
# The node process: its counters restart from 0 with it, so a restart during
# the build would hide failures before it.
instance() { count instance; }
# A write that did not reach the fleet store (write_failed counts every write
# Xcode got an error for), or a skipped fleet-store call.
failures() { count write_failed write_refused up_errors up_skipped; }
# Xcode talked to the node (a build without the plugin settings does not).
activity() { count kv_get_hit kv_get_miss kv_put; }
xcode=$(xcodebuild -version | awk '/Build version/ {print $3}')
[ -n "$xcode" ] || { echo "no Xcode build version" >&2; exit 2; }
"$ROOT/bin/fleet-cas-settings.sh" "$ROOT/fleet-cas.sock" >/dev/null || exit 3
rm -rf "$ROOT/cas"
keylog=$ROOT/node-store/fill-keys.log
k0=$( { wc -c <"$keylog"; } 2>/dev/null | tr -d ' ')
i0=$(instance) f0=$(failures) a0=$(activity)
[ -n "$i0" ] && [ -n "$f0" ] && [ -n "$a0" ] || { echo "cannot read $stats" >&2; exit 3; }
"$@"
rc=$?
[ "$rc" -eq 0 ] || exit "$rc"
# A node that died late in the build and is not back yet left its last stats
# behind: calls to the dead socket fail without being counted anywhere.
"$ROOT/bin/fleet-cas-settings.sh" "$ROOT/fleet-cas.sock" >/dev/null || {
  echo "fleet-cas: the node is down after the build; no marker for $repo/$commit" >&2
  exit 4
}
sleep 1  # the node rewrites stats.json every 500 ms
i1=$(instance) f1=$(failures) a1=$(activity)
if [ "$i1" != "$i0" ]; then
  echo "fleet-cas: the node restarted during the build; no marker for $repo/$commit" >&2
  exit 4
fi
if [ -z "$f1" ] || [ "$f1" != "$f0" ]; then
  echo "fleet-cas: failed or skipped fleet-store calls during the build; no marker for $repo/$commit" >&2
  exit 4
fi
if [ "$a1" = "$a0" ]; then
  echo "fleet-cas: the build never used the node (plugin settings missing?); no marker for $repo/$commit" >&2
  exit 4
fi
# The keys this build used. A node without a key log (older than warm) gives
# a marker without a manifest: readers can still check it, not warm from it.
manifest=()
if [ -n "$k0" ]; then
  list=$ROOT/fill-manifest.$$
  tail -c +"$((k0 + 1))" "$keylog" | sort -u >"$list" || exit 4
  trap 'rm -f "$list"' EXIT
  [ -s "$list" ] || { echo "fleet-cas: the key log gained nothing during the build; no marker for $repo/$commit" >&2; exit 4; }
  manifest=(--manifest "$list")
  echo "fleet-cas: manifest of $(wc -l <"$list" | tr -d ' ') keys"
else
  echo "fleet-cas: no key log at $keylog (node predates warm); marker without a manifest" >&2
fi
"$ROOT/bin/fleet-cas" marker put "http://$store" "$repo/$commit/$xcode" --sign-key "$key" \
  --entry "repo=$repo" --entry "commit=$commit" --entry "xcode=$xcode" \
  --entry "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)" ${manifest[@]+"${manifest[@]}"} || exit 4
echo "fleet-cas: marker $repo/$commit/$xcode"
# The newest fill, for readers' prewarm (fleet-cas-prewarm.sh). Only after the commit's own
# marker, so it never names a commit without one. Best effort: the fill itself succeeded.
"$ROOT/bin/fleet-cas" marker put "http://$store" "$repo/latest/$xcode" --sign-key "$key" \
  --entry "repo=$repo" --entry "commit=$commit" --entry "xcode=$xcode" \
  --entry "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)" || echo "fleet-cas: could not update $repo/latest/$xcode" >&2
