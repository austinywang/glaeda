#!/bin/bash
# Non-root repairs for one cmux build mini, one function per repair. glaeda-mini-fleet fix sends
# this file over SSH as the login user, after a WORKLOAD_PATH assignment and before one call such
# as `cmux_clone manaflow-ai/cmux main '~/cmux' 1`. Each function looks before it acts, so a rerun
# after a failure or an interruption resumes and a finished step says "unchanged". Nothing here
# uses sudo, and nothing removes or overwrites a file this file did not create.
set -euo pipefail
# share_recv reads a tar stream on stdin; everything else must not read from the SSH channel.
[ "${KEEP_STDIN:-}" = 1 ] || exec </dev/null
export PATH="$WORKLOAD_PATH:$HOME/.local/bin"
export GIT_TERMINAL_PROMPT=0 HOMEBREW_NO_ENV_HINTS=1 HOMEBREW_NO_ASK=1
cd "$HOME"

say() { printf 'glaeda-mini-fleet: %s\n' "$*"; }
refuse() { say "refused: $*" >&2; exit 3; }
home() { printf '%s' "${1/#\~/$HOME}"; }

# >>> host gate (sudo-plan scripts inline this block too)
# The host is held while another build holds the fleet host lock (the flock with-host-lock, the build
# worker and the runner hook take) or an active glaeda-reservation/v1 marker names it; an unreadable or
# invalid marker counts as held (glaeda_reservation.py's rule). host_held prints why and succeeds when
# held; only perl's explicit "free" answer (exit 3) reads as free, so a perl failure holds the host.
# host_held reservation checks the reservation alone (a runner drain waits out the job holding the lock).
# perl, because a fresh mini has no Python and macOS has no flock(1).
GLAEDA_FLEET_DIR="${GLAEDA_FLEET_DIR:-/Users/Shared/cmux-build-fleet}"
host_held() {
  local out rc
  out=$(perl -MFcntl=:flock -MJSON::PP -MB -MPOSIX=strftime -e '
    my ($dir, $mode) = @ARGV;
    eval {
      sub flags { my $v = shift; return ref($v) ? 0 : B::svref_2object(\$v)->FLAGS }
      sub is_int { my $f = flags($_[0]); defined $_[0] && ($f & B::SVp_IOK) && !($f & (B::SVp_POK | B::SVp_NOK)) }
      sub is_str { my $f = flags($_[0]); defined $_[0] && ($f & B::SVp_POK) && !($f & (B::SVp_IOK | B::SVp_NOK)) }
      my $res = "$dir/reservation.json";
      if (-e $res) {
        open(my $fh, "<", $res) or do { print "reservation $res is unreadable\n"; exit 0 };
        my $text = do { local $/; <$fh> };
        my $doc = eval { JSON::PP->new->decode($text) };
        # JSON::PP reads 1e3 as an integer; Python (glaeda_reservation.py) does not, so require plain digits.
        my $plain = $text =~ /"since"\s*:\s*-?\d+\s*[,}]/ && $text =~ /"until"\s*:\s*-?\d+\s*[,}]/;
        my $why = ref($doc) ne "HASH" ? "not a JSON object"
          : (($doc->{schema} // "") ne "glaeda-reservation/v1") ? "schema is not glaeda-reservation/v1"
          : !(is_str($doc->{owner}) && is_str($doc->{purpose})) ? "owner or purpose is not a string"
          : !($plain && is_int($doc->{since}) && is_int($doc->{until})) ? "since or until is not integer Unix seconds" : "";
        if ($why ne "") { print "reservation $res is not a valid glaeda-reservation/v1 marker ($why)\n"; exit 0 }
        if (time() < $doc->{until}) {
          my $when = eval { strftime("%Y-%m-%dT%H:%M:%SZ", gmtime($doc->{until})) } // "Unix time $doc->{until}";
          printf "reserved by %s for %s until %s\n", $doc->{owner} || "?", $doc->{purpose} || "?", $when;
          exit 0;
        }
      }
      my $lock = "$dir/host.lock";
      if (($mode // "") ne "reservation" && -e $lock) {
        open(my $fh, "<", $lock) or do { print "cannot open the fleet host lock $lock\n"; exit 0 };
        # Exclusive: PR jobs on a mini with several runners hold it shared (glaeda-cmux-runner-hook).
        flock($fh, LOCK_EX | LOCK_NB) or do { print "held by another build (fleet host lock $lock)\n"; exit 0 };
        flock($fh, LOCK_UN);
      }
      print "free\n";
      exit 3;
    };
    print "cannot read the host lock and reservation: $@";
    exit 0;
  ' "$GLAEDA_FLEET_DIR" "${1:-all}" 2>&1) && rc=0 || rc=$?
  [ "$rc" = 3 ] && [ "$out" = free ] && return 1
  printf '%s\n' "${out:-cannot read the host lock and reservation (perl exit $rc)}"
  return 0
}
# <<< host gate

held_refuse() { say "held: $*, skipped" >&2; exit 20; }

host_check() {  # exit 0 when the host is free, 20 (and why on stdout) when it is held
  local why
  if why=$(host_held); then say "held: $why"; exit 20; fi
  say "free"
}

# One changing step at a time per host: an SSH timeout on the operator side does not stop the remote
# step, so a rerun must not start the same work beside it. A lock whose process is gone is taken over.
# No step starts while the host is held (see host_held). runner_hold honors only reservations: it exists to
# wait out the job that holds the fleet host lock. runner_release and runner_kick only restore runners,
# whose job hook guards the lock itself, so they skip the gate.
lock() {  # [reservation|none]: how much of the host gate this step honors (default: all)
  local dir="$HOME/.local/state/glaeda/mini-fleet/step.lock" pid why
  if [ "${1:-all}" != none ] && why=$(host_held "${1:-all}"); then held_refuse "$why"; fi
  mkdir -p "$(dirname "$dir")"
  if ! mkdir "$dir" 2>/dev/null; then
    pid=$(cat "$dir/pid" 2>/dev/null || true)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then refuse "another glaeda-mini-fleet step (pid $pid) is still running here"; fi
  fi
  echo $$ > "$dir/pid"
  trap 'rm -f "$HOME/.local/state/glaeda/mini-fleet/step.lock/pid"; rmdir "$HOME/.local/state/glaeda/mini-fleet/step.lock" 2>/dev/null || true' EXIT
}

# The cmux checkout: a shallow clone of REF with submodules, as the fleet bootstrap reads them.
cmux_clone() {  # REPO REF ROOT DEPTH
  local root; root=$(home "$3")
  if [ -e "$root/.git" ]; then
    # An interrupted clone leaves a .git with no HEAD; building on it would fail later and obscurely.
    git -C "$root" rev-parse --verify -q HEAD >/dev/null || refuse "$root is an unfinished clone (no HEAD); move it aside"
    say "unchanged: $root is a checkout"; submodules "$3" "$4"; return
  fi
  [ ! -e "$root" ] || refuse "$root exists and is not a git checkout"
  git clone --depth 1 --branch "$2" --progress "https://github.com/$1.git" "$root"
  submodules "$3" "$4"
}

submodules() {  # ROOT DEPTH (0 means full history)
  local root; root=$(home "$1")
  [ -e "$root/.git" ] || refuse "$root is not a git checkout"
  if [ "$2" = 0 ]; then
    git -C "$root" submodule update --init --recursive --progress
  else
    git -C "$root" submodule update --init --recursive --depth "$2" --progress
  fi
  say "submodules: $(git -C "$root" submodule status --recursive | grep -c '^ ' || true) initialized"
}

# A relocatable CPython that the operator copied to DIR becomes ~/.local/bin/python3.
python_prepare() {  # DIR
  mkdir -p "$HOME/.local/bin" "$(home "$1")"
}

python_link() {  # DIR MIN
  local dir link="$HOME/.local/bin/python3"; dir=$(home "$1")
  "$dir/bin/python3" -c 'import sys; want = tuple(int(p) for p in sys.argv[1].split(".")); sys.exit(sys.version_info[:len(want)] < want)' "$2" \
    || refuse "$dir/bin/python3 does not run or is older than $2"
  if [ -e "$link" ] || [ -L "$link" ]; then
    case "$(readlink "$link" || true)" in
      "$HOME"/.local/python-*) ;;
      *) refuse "$link exists and was not made by glaeda-mini-fleet; move it aside first" ;;
    esac
  fi
  ln -sfn "$dir/bin/python3" "$link"
  say "python3: $("$link" --version)"
}

rust_default() {  # TOOLCHAIN
  rustup default "$1"
}

rust_channel() {  # ROOT CHANNEL (the toolchain rust-toolchain.toml in the checkout names)
  cd "$(home "$1")" && rustup toolchain install "$2" --profile minimal
}

# Glaeda itself: the checkout glaeda-mini-enroll runs from, then its user-level setup.
glaeda() {  # PYTHON
  local dir="$HOME/glaeda"
  if [ ! -f "$dir/scripts/cmux_fleet.py" ]; then
    [ ! -e "$dir" ] || refuse "$dir exists and is not a Glaeda checkout"
    # the fleet's read-only mirror of teamleaderleo/glaeda (docs/GLAEDA_OTA.md, Fleet mirror)
    git clone --depth 1 --progress https://github.com/manaflow-ai/glaeda.git "$dir"
  fi
  # an older checkout's setup predates --ota-source; glaeda-mirror.sh in hq sets it on such hosts later
  local ota=()
  grep -q -- '--ota-source' "$dir/scripts/glaeda-mini-setup" && ota=(--ota-source manaflow-ai/glaeda)
  "$(home "$1")" "$dir/scripts/glaeda-mini-setup" --apply ${ota[@]+"${ota[@]}"}
}

# ~/glaeda at one commit for the whole run (glaeda-mini-fleet passes --glaeda-ref, else the tip of main), so
# every host runs the same glaeda-mini-enroll and staging verifier; that commit must know every FLAG onboarding
# passes (default --renew). A clean fetch of exactly that commit and a detached checkout; local changes refuse.
glaeda_sync() {  # COMMIT [FLAG...]
  local dir="$HOME/glaeda" head enroll flag commit="$1"; shift
  [ $# -gt 0 ] || set -- --renew
  set -- "$commit" "$@"
  [ -f "$dir/scripts/cmux_fleet.py" ] || refuse "$dir is not a Glaeda checkout; run glaeda-mini-fleet fix first"
  head=$(git -C "$dir" rev-parse -q --verify HEAD || true)
  if [ "$head" != "$1" ]; then
    [ -z "$(git -C "$dir" status --porcelain=v1 --untracked-files=no)" ] || refuse "$dir has local changes; commit or move them aside"
  fi
  if ! git -C "$dir" cat-file -e "$1^{commit}" 2>/dev/null; then
    if [ "$(git -C "$dir" rev-parse --is-shallow-repository)" = true ]; then
      git -C "$dir" fetch --quiet --depth 1 origin "$1"
    else
      git -C "$dir" fetch --quiet origin "$1"
    fi
  fi
  # Captured, not piped into grep -q: under pipefail an early grep exit would fail git show.
  enroll=$(git -C "$dir" show "$1:scripts/glaeda-mini-enroll")
  for flag in "${@:2}"; do
    case "$enroll" in
      *"\"$flag\""*) ;;
      *) refuse "glaeda ${1:0:12} predates glaeda-mini-enroll $flag" ;;
    esac
  done
  # Checked after the options, so a head that lacks one is refused rather than reported unchanged.
  if [ "$head" = "$1" ]; then say "unchanged: ~/glaeda is at ${1:0:12}"; return; fi
  git -C "$dir" checkout --quiet --detach "$1"
  say "~/glaeda moved from ${head:0:12} to ${1:0:12}"
}

# Homebrew in an /opt/homebrew the login user owns (sudo-plan creates it), without the brew.sh installer, which
# stalls on a Command Line Tools install the selected Xcode makes moot. A shallow fetch of Homebrew/brew from a
# LAN peer's /opt/homebrew when given (ssh://user@peer/opt/homebrew), else from UPSTREAM; origin is always
# UPSTREAM afterwards, so brew update reads GitHub. Only an empty prefix (directories alone, as a stalled
# installer leaves it) is filled; an unfinished fetch (a .git, no brew) resumes.
homebrew_fetch() {  # UPSTREAM [PEER_URL [IDENTITY]]
  local dir="${GLAEDA_HOMEBREW_DIR:-/opt/homebrew}" upstream="$1" peer="${2:-}" identity="${3:-}" tag ref
  if [ -x "$dir/bin/brew" ]; then say "unchanged: $dir has brew"; return; fi
  [ -d "$dir" ] || refuse "$dir does not exist; glaeda-mini-fleet sudo-plan creates it"
  [ -O "$dir" ] || refuse "$dir is not $(id -un)'s; glaeda-mini-fleet sudo-plan hands it over"
  if [ ! -d "$dir/.git" ]; then
    [ -z "$(find "$dir" -mindepth 1 -not -type d 2>/dev/null | head -1)" ] || \
      refuse "$dir has files but no brew; move them aside"
    git -C "$dir" init -q
  fi
  export GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new${identity:+ -i $identity}"
  if [ -n "$peer" ] && git -C "$dir" fetch --quiet --depth 1 "$peer" HEAD; then
    say "fetched Homebrew/brew from $peer"
  else
    [ -z "$peer" ] || say "could not fetch from $peer; fetching $upstream"
    # The newest release tag, as the brew.sh installer checks out; HEAD only when there is none.
    tag=$(git ls-remote --tags --refs "$upstream" | sed -n 's#.*refs/tags/\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$#\1#p' \
      | sort -t. -k1,1n -k2,2n -k3,3n | tail -1 || true)
    ref=HEAD; [ -z "$tag" ] || ref="+refs/tags/$tag:refs/tags/$tag"
    git -C "$dir" fetch --quiet --depth 1 "$upstream" "$ref"
  fi
  git -C "$dir" checkout --quiet --detach FETCH_HEAD
  if git -C "$dir" remote get-url origin >/dev/null 2>&1; then
    git -C "$dir" remote set-url origin "$upstream"
  else
    git -C "$dir" remote add origin "$upstream"
  fi
  [ -x "$dir/bin/brew" ] || refuse "$dir/bin/brew is missing after the checkout"
  say "Homebrew $(git -C "$dir" describe --tags --always 2>/dev/null) in $dir"
}

# ---- The GitHub Actions runner's launchd agent, which glaeda-cmux-runner --apply installs in the
# login user's GUI domain. runner_hold stops it once its current job ends and marks it held; runner_release
# starts what runner_hold stopped (if-eligible: only while the enrollment is eligible, as after a renewal
# that failed before it quarantined); runner_kick restarts a loaded agent whose listener is gone. The held
# marks are what repair reads.
held_dir() { printf '%s' "$HOME/.local/state/glaeda/mini-fleet/runner-held"; }

runner_plist() {  # DIR: the runner's LaunchAgent (svc.sh records it in .service; glaeda-cmux-runner's is fixed), or fail
  local plist; plist=$(cat "$1/.service" 2>/dev/null || true)
  local base; base=$(basename "$1")
  if [ -z "$plist" ] && [ "$base" = actions-runner-glaeda ]; then
    plist="$HOME/Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
  elif [ -z "$plist" ] && [[ "$base" =~ ^actions-runner-glaeda-([0-9]+)$ ]]; then  # --instance K
    plist="$HOME/Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.${BASH_REMATCH[1]}.plist"
  fi
  [ -n "$plist" ] && [ -f "$plist" ] || return 1
  printf '%s' "$plist"
}

runner_hold() {  # WAIT_SECONDS
  local r dir plist label domain deadline
  domain="gui/$(id -u)"
  mkdir -p "$(held_dir)"
  for r in "$HOME"/actions-runner*/.runner; do
    [ -f "$r" ] || continue
    dir=$(dirname "$r")
    plist=$(runner_plist "$dir") || { say "$dir has no launchd agent; nothing to stop"; continue; }
    label=$(basename "$plist" .plist)
    if ! launchctl print "$domain/$label" >/dev/null 2>&1; then say "unchanged: $label is not loaded"; continue; fi
    deadline=$(( $(date +%s) + $1 ))
    while pgrep -f "$dir/bin/Runner.Worker" >/dev/null 2>&1; do
      [ "$(date +%s)" -lt "$deadline" ] || refuse "$label is still running a job after $1 seconds; rerun once it ends"
      say "$label is running a job; waiting for it to end"
      sleep 30
    done
    : > "$(held_dir)/$(basename "$dir")"
    launchctl disable "$domain/$label"
    launchctl bootout "$domain/$label" 2>/dev/null || true
    ! launchctl print "$domain/$label" >/dev/null 2>&1 || refuse "could not stop $label"
    say "stopped $label; it starts again once the node is eligible"
  done
}

runner_release() {  # [if-eligible]
  local r dir plist label domain state
  domain="gui/$(id -u)"
  if [ "${1:-}" = if-eligible ]; then
    state=$(plutil -extract state raw "${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet/enrollment.json" 2>/dev/null || true)
    if [ "$state" != eligible ]; then say "runners stay held: the node is ${state:-not enrolled}"; return; fi
  fi
  for r in "$HOME"/actions-runner*/.runner; do
    [ -f "$r" ] || continue
    dir=$(dirname "$r")
    [ -f "$(held_dir)/$(basename "$dir")" ] || continue
    plist=$(runner_plist "$dir") || { say "$dir has no launchd agent; glaeda-cmux-runner --apply installs it"; continue; }
    label=$(basename "$plist" .plist)
    launchctl enable "$domain/$label"
    launchctl print "$domain/$label" >/dev/null 2>&1 || launchctl bootstrap "$domain" "$plist" \
      || refuse "could not start $label; the runner runs in the GUI session, so log in on the mini (or enable auto-login)"
    rm -f "$(held_dir)/$(basename "$dir")"
    say "started $label"
  done
}

runner_kick() {
  local r dir plist label domain
  domain="gui/$(id -u)"
  for r in "$HOME"/actions-runner*/.runner; do
    [ -f "$r" ] || continue
    dir=$(dirname "$r")
    plist=$(runner_plist "$dir") || continue
    label=$(basename "$plist" .plist)
    launchctl print "$domain/$label" >/dev/null 2>&1 || continue
    if ! pgrep -f "$dir/bin/Runner.Listener" >/dev/null 2>&1; then
      launchctl kickstart -k "$domain/$label"
      say "restarted $label"
    fi
  done
}

# Xcode 26 ships Metal separately; the fleet bootstrap runs xcrun metal.
metal() {  # DEVELOPER_DIR
  if DEVELOPER_DIR="$1" xcrun metal --version >/dev/null 2>&1; then say "unchanged: Metal toolchain present"; return; fi
  DEVELOPER_DIR="$1" xcodebuild -downloadComponent MetalToolchain
  DEVELOPER_DIR="$1" xcrun metal --version | head -1
}

# cmux scripts/setup.sh builds GhosttyKit, the artifacts the bootstrap checks for.
cmux_setup() {  # ROOT DEVELOPER_DIR
  cd "$(home "$1")" && DEVELOPER_DIR="$2" ./scripts/setup.sh
}

# The reviewed Glaeda candidate, where glaeda-mini-enroll --candidate is pointed at it.
candidate_dir() {  # SOURCE12
  mkdir -p "$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$1"
}

candidate_check() {  # SOURCE12 NAME SHA256: exit 0 when the archive is in place, 10 when it is absent
  local file="$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$1/$2"
  [ -f "$file" ] || exit 10
  local got; got=$(shasum -a 256 "$file" | cut -d' ' -f1)
  [ "$got" = "$3" ] || refuse "$file has sha256 $got, want $3; move it aside"
  say "unchanged: $file matches"
}

candidate_place() {  # SOURCE12 NAME SHA256: verify the copy, then move it into place
  local dir="$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$1"
  local got; got=$(shasum -a 256 "$dir/$2.partial" | cut -d' ' -f1)
  [ "$got" = "$3" ] || refuse "$dir/$2.partial has sha256 $got, want $3"
  mv -n "$dir/$2.partial" "$dir/$2"
  [ ! -e "$dir/$2.partial" ] || refuse "$dir/$2 appeared during the copy; $2.partial was left for inspection"
  say "staged: $dir/$2"
}

# ---- LAN seeding (glaeda-mini-fleet share). On the source, share_prepare readies THING once (only
# metal needs it: an exported bundle), share_source checks it and share_send writes it as a tar
# stream to stdout without the step lock, so one source can feed several peers at once. On the peer,
# share_have says whether it is already there, share_recv unpacks the stream into a staging
# directory, and share_finish moves it into place and verifies it. THING is xcode, metal, brew,
# rustup, candidate or python; the arguments follow it.

share_stage() {  # THING ARGS...: the staging directory on the peer, on the same volume as the target
  case "$1" in
    xcode) printf '%s/.glaeda-share.partial' "$(dirname "$2")" ;;
    candidate) printf '%s/Library/Caches/cmux-fleet/glaeda-candidate-%s/.glaeda-share.partial' "$HOME" "$2" ;;
    rustup) printf '%s/.rustup/.glaeda-share.partial' "$HOME" ;;
    python) printf '%s/.local/.glaeda-share.partial' "$HOME" ;;
    metal) printf '%s/Library/Caches/glaeda/share/metal' "$HOME" ;;
    brew) printf '%s/Library/Caches/glaeda/share/homebrew/downloads' "$HOME" ;;
    *) refuse "unknown thing $1" ;;
  esac
}

xcode_matches() {  # APP VERSION BUILD
  [ -d "$1" ] && [ ! -L "$1" ] || return 1
  local v b
  v=$(plutil -extract CFBundleShortVersionString raw "$1/Contents/Info.plist" 2>/dev/null) || return 1
  b=$(plutil -extract ProductBuildVersion raw "$1/Contents/version.plist" 2>/dev/null) || return 1
  [ "$v|$b" = "$2|$3" ]
}

python_ours() {  # print the ~/.local/python-* directory ~/.local/bin/python3 points into, or fail
  local target; target=$(readlink "$HOME/.local/bin/python3" 2>/dev/null) || return 1
  case "$target" in "$HOME"/.local/python-*/bin/python3) dirname "$(dirname "$target")" ;; *) return 1 ;; esac
}

python_at_least() {  # PYTHON MIN
  "$1" -c 'import sys; want = tuple(int(p) for p in sys.argv[1].split(".")); sys.exit(sys.version_info[:len(want)] < want)' "$2"
}

brew_downloads() {  # the Homebrew owner's download cache, or fail
  [ -d /opt/homebrew/Cellar ] || return 1
  local owner; owner=$(stat -f %Su /opt/homebrew/Cellar)
  if [ "$owner" = "$(id -un)" ]; then printf '%s/downloads' "$(brew --cache)"; else printf '/Users/%s/Library/Caches/Homebrew/downloads' "$owner"; fi
}

share_source() {  # THING ARGS...: exit 0 when this host can seed THING
  case "$1" in
    xcode) xcode_matches "$2" "$3" "$4" || refuse "$2 is not Xcode $3 ($4) here" ;;
    candidate) candidate_check "$2" "$3" "$4" >/dev/null ;;
    python) local dir; dir=$(python_ours) || refuse "~/.local/bin/python3 here is not a relocatable CPython under ~/.local"
            python_at_least "$dir/bin/python3" "$2" || refuse "$dir is older than $2" ;;
    rustup) [ -n "$(ls "$HOME/.rustup/toolchains" 2>/dev/null)" ] || refuse "no rustup toolchains here" ;;
    brew) local d; d=$(brew_downloads) || refuse "no Homebrew here"
          [ -r "$d" ] && [ -x "$d" ] || refuse "$d is not readable by $(id -un)" ;;
    metal) local b; b=$(ls -d "$(share_stage metal)"/*.exportedBundle 2>/dev/null | tail -1 || true)
           [ -n "$b" ] || refuse "no exported Metal toolchain bundle here yet (share prepares one: about 700 MB)"
           say "metal: $b" ;;
    *) refuse "unknown thing $1" ;;
  esac
}

share_prepare() {  # THING ARGS...: ready THING on the source; for metal, export a bundle once
  if [ "$1" != metal ]; then share_source "$@"; return; fi
  local stage tmp; stage=$(share_stage metal)
  mkdir -p "$stage"
  if ! ls -d "$stage"/*.exportedBundle >/dev/null 2>&1; then
    # Export beside the cache and move it in whole, so an interrupted export is never shipped.
    tmp=$(mktemp -d "$stage.export.XXXXXX")
    DEVELOPER_DIR="$2" xcodebuild -downloadComponent MetalToolchain -exportPath "$tmp"
    mv "$tmp"/*.exportedBundle "$stage"/
    rmdir "$tmp"
  fi
  share_source "$@"
}

share_have() {  # THING ARGS...: exit 0 when this host already has THING, 1 (10 for candidate) when it needs it
  case "$1" in
    xcode) if [ -e "$2" ] || [ -L "$2" ]; then xcode_matches "$2" "$3" "$4" || refuse "$2 exists and is not Xcode $3 ($4)"; say "unchanged: $2"; exit 0; fi; exit 1 ;;
    candidate) candidate_check "$2" "$3" "$4" ;;  # exits 10 when absent
    python) local p="$HOME/.local/bin/python3"; [ -x "$p" ] && python_at_least "$p" "$2" && { say "unchanged: $p"; exit 0; }; exit 1 ;;
    metal) DEVELOPER_DIR="$2" xcrun metal --version >/dev/null 2>&1 && { say "unchanged: Metal toolchain present"; exit 0; }; exit 1 ;;
    rustup|brew) exit 1 ;;
    *) refuse "unknown thing $1" ;;
  esac
}

share_send() {  # THING ARGS...: a tar stream of THING on stdout; progress on stderr
  share_source "$@" >&2
  case "$1" in
    xcode) tar -C "$(dirname "$2")" -cf - "$(basename "$2")" ;;
    candidate) tar -C "$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$2" -cf - "$3" ;;
    python) local dir; dir=$(python_ours); tar -C "$HOME/.local" -cf - "$(basename "$dir")" ;;
    rustup) tar -C "$HOME/.rustup" -cf - toolchains $(cd "$HOME/.rustup" && ls -d update-hashes 2>/dev/null) ;;
    brew) local d; d=$(brew_downloads); (cd "$d" && { ls | grep -E 'bottle' | grep -v '\.incomplete$' || true; } | tar -cf - -T -) ;;
    metal) local stage; stage=$(share_stage metal)
           tar -C "$stage" -cf - "$(basename "$(ls -d "$stage"/*.exportedBundle | tail -1)")" ;;
  esac
}

share_recv() {  # THING ARGS...: unpack the stream on stdin into the staging directory
  local stage; stage=$(share_stage "$@")
  # A staging directory named *.glaeda-share.partial is only ever this file's; start it fresh.
  case "$stage" in */.glaeda-share.partial) rm -rf "$stage" ;; esac
  mkdir -p "$stage"
  tar -C "$stage" -xf -
}

share_finish() {  # THING ARGS...: move what share_recv staged into place, never over something already there
  local stage entry; stage=$(share_stage "$@")
  case "$1" in
    xcode)
      # mv into an existing directory would nest the copy inside it; refuse instead.
      if [ -e "$2" ] || [ -L "$2" ]; then refuse "$2 appeared during the copy; the staged copy was left in $stage"; fi
      mv -n "$stage/$(basename "$2")" "$(dirname "$2")/"
      [ ! -e "$stage/$(basename "$2")" ] || refuse "$2 appeared during the copy; the staged copy was left in $stage"
      rmdir "$stage" 2>/dev/null || true
      xcode_matches "$2" "$3" "$4" || refuse "$2 is not Xcode $3 ($4) after the copy"
      say "copied: $2 ($3 $4); licence and first launch are sudo-plan steps" ;;
    candidate)
      mv "$stage/$3" "$stage/../$3.partial"
      rmdir "$stage" 2>/dev/null || true
      candidate_place "$2" "$3" "$4" ;;
    python)
      for entry in "$stage"/python-*; do mv -n "$entry" "$HOME/.local/"; done
      rmdir "$stage" 2>/dev/null || true
      python_link "~/.local/$(basename "$entry")" "$2" ;;
    rustup)
      mkdir -p "$HOME/.rustup/toolchains" "$HOME/.rustup/update-hashes"
      for entry in "$stage"/toolchains/* "$stage"/update-hashes/*; do
        [ -e "$entry" ] || continue
        local into; into="$HOME/.rustup/$(basename "$(dirname "$entry")")"
        if [ -e "$into/$(basename "$entry")" ]; then say "unchanged: $into/$(basename "$entry")"; else mv "$entry" "$into/"; say "copied: $(basename "$entry")"; fi
      done
      rm -rf "$stage"  # what is left are copies of toolchains this host already had
      ;;
    metal)
      local bundle; bundle=$(ls -d "$stage"/*.exportedBundle | tail -1)
      DEVELOPER_DIR="$2" xcodebuild -importComponent MetalToolchain -importPath "$bundle"
      DEVELOPER_DIR="$2" xcrun metal --version | head -1 ;;
    brew) say "staged $(ls "$stage" | wc -l | tr -d ' ') Homebrew downloads in $stage; the next brew install (fix or sudo-plan) uses them" ;;
  esac
}
