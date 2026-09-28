#!/bin/bash
# One measured Xcode build with compilation caching on.
#
# usage: PKG=<package or project dir> SCHEME=<scheme> WORK=<scratch dir> \
#          [PROJECT=<x.xcodeproj>] [SPM=<cloned packages dir>] [DD=<path>] \
#          xcode-cache-build.sh <label> [extra build settings...]
#
# SPM builds against packages resolved once into that directory, so every
# run (and machine) sees the same package checkouts at the same path.
#
# Each label gets its own DerivedData and local CAS under $WORK, so a second
# label is a "fresh machine" whose only warm source is whatever the extra
# settings point at (a shared local CAS path or a remote service socket).
# Prefix mapping keeps cache keys independent of the DerivedData path.
#
# With DD=<path>, every label builds in that one DerivedData path and one
# local CAS path ($WORK/localcas), both emptied first; they must be the same
# absolute paths on every machine. Keys match without mapping DerivedData,
# which avoids a Swift 6.2 (Xcode 26.3) compiler crash the mapping triggers.
# The fixed CAS path matters for targets with a bridging header: the
# bridging-header PCH key includes the local CAS path, and every Swift
# compile in the target carries that key.
set -u
: "${PKG:?}" "${SCHEME:?}" "${WORK:?}"
label=$1; shift
# Paths are resolved after the cd below, and DD mode empties them: absolute only.
for v in WORK ${DD:+DD}; do
  case ${!v} in /*) ;; *) echo "$v must be an absolute path" >&2; exit 2 ;; esac
done
if [ -n "${DD:-}" ]; then
  case ${DD%/} in
    "" | "$HOME" | "$HOME/Library/Developer/Xcode/DerivedData" | "${WORK%/}" | "${PKG%/}")
      echo "refusing DD=$DD" >&2; exit 2 ;;
  esac
  dd=$DD
  cas="$WORK/localcas"
  rm -rf "$dd" "$cas"
  map_derived=()
else
  dd="$WORK/dd-$label"
  cas="$WORK/localcas-$label"
  map_derived=(SWIFT_OTHER_PREFIX_MAPPINGS="$dd=/^derived" CLANG_OTHER_PREFIX_MAPPINGS="$dd=/^derived")
fi
# A plugin socket nobody answers on makes Xcode crawl instead of failing:
# drop the plugin settings and build with the local cache alone.
args=() sock= node_down=
for a in "$@"; do
  case $a in COMPILATION_CACHE_REMOTE_SERVICE_PATH=*) sock=${a#*=} ;; esac
done
if [ -n "$sock" ] && ! "$(dirname "$0")/fleet-cas-settings.sh" "$sock" >/dev/null; then
  [ -n "${REQUIRE_NODE:-}" ] && { echo "$label: node down on $sock" >&2; exit 3; }
  node_down=1
  for a in "$@"; do
    case $a in COMPILATION_CACHE_ENABLE_PLUGIN=* | COMPILATION_CACHE_REMOTE_SERVICE_PATH=*) ;; *) args+=("$a") ;; esac
  done
  set -- ${args[@]+"${args[@]}"}
fi
log="$WORK/log-$label.txt"
cd "$PKG" || exit 2
where=()
[ -n "${PROJECT:-}" ] && where+=(-project "$PROJECT")
[ -n "${SPM:-}" ] && where+=(-clonedSourcePackagesDirPath "$SPM" -disableAutomaticPackageResolution)
start=$(date +%s.%N)
xcodebuild build ${where[@]+"${where[@]}"} -scheme "$SCHEME" -configuration Debug \
  -destination 'platform=macOS' \
  -derivedDataPath "$dd" \
  COMPILATION_CACHE_ENABLE_CACHING=YES \
  COMPILATION_CACHE_CAS_PATH="$cas" \
  COMPILATION_CACHE_ENABLE_DIAGNOSTIC_REMARKS=YES \
  SWIFT_ENABLE_PREFIX_MAPPING=YES CLANG_ENABLE_PREFIX_MAPPING=YES \
  SWIFT_ENABLE_PROJECT_PREFIX_MAPPING=YES CLANG_ENABLE_PROJECT_PREFIX_MAPPING=YES \
  ${map_derived[@]+"${map_derived[@]}"} "$@" >"$log" 2>&1
rc=$?
end=$(date +%s.%N)
printf '%s rc=%s %.1fs hits=%s misses=%s\n' "$label" "$rc" "$(echo "$end - $start" | bc)" \
  "$(grep -c 'cache hit' "$log")" "$(grep -c 'cache miss' "$log")"
if [ -n "$node_down" ]; then echo "$label: node was down; built with the local cache only"; fi
if grep -q bad_optional_access "$log"; then echo "$label: compiler crashed (bad_optional_access)"; fi
exit "$rc"
