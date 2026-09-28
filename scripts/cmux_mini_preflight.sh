#!/bin/bash
# Read-only preflight probe for one cmux build mini, run after scripts/cmux_mini_probe.sh in the same
# SSH session. glaeda-mini-fleet prepends the inputs as shell assignments:
#   CMUX_ROOT       the cmux checkout (a leading ~ means the login user's home)
#   XCODE_PIN       the pinned Xcode app, or empty
#   WORKLOAD_PATH   cmux_fleet_bootstrap.CMUX_WORKLOAD_TOOL_PATH
#   WORKLOAD_TOOLS  cmux_fleet_bootstrap.MACOS_WORKLOAD_TOOLS, space separated
#   PYTHONS         glaeda-mini-enroll's interpreter candidates, space separated, ~ for home
#   CANDIDATE_PIN   the Glaeda candidate source commit the fleet manifest (or glaeda-mini-fleet upgrade) pins, or empty
#   ENROLL_FLAGS    the glaeda-mini-enroll options onboarding passes, space separated
#   PIN_FORMULAS    the Homebrew formulas class receipts pin, space separated
# Emits "key<TAB>value" lines like the probe. Installs, downloads and writes nothing: rustup runs with
# RUSTUP_AUTO_INSTALL=0 and git with GIT_OPTIONAL_LOCKS=0. Never prints tokens or runner URLs; for a readable
# secret it prints the path alone.
# The group is parsed whole before it runs, so its </dev/null cannot eat the rest of this script.
{
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
e() { printf '%s\t%s\n' "$1" "$2"; }
first() { head -1 | cut -c1-160; }
last() { grep -v '^[[:space:]]*$' | tail -1 | cut -c1-200; }
CMUX_ROOT="${CMUX_ROOT/#\~/$HOME}"
export RUSTUP_AUTO_INSTALL=0 GIT_OPTIONAL_LOCKS=0

# Xcode: the pinned app's own tools, as CI selects it through DEVELOPER_DIR.
dev=""
[ -n "$XCODE_PIN" ] && [ -d "$XCODE_PIN/Contents/Developer" ] && dev="$XCODE_PIN/Contents/Developer"
if [ -n "$dev" ]; then
  out=$(DEVELOPER_DIR="$dev" xcodebuild -showsdks 2>&1)
  if printf '%s' "$out" | grep -qE 'DVTPlugIn|runFirstLaunch|DVTDownloads|PlugInLoading'; then
    e pf_sdks "plugin_error|$(printf '%s\n' "$out" | grep -E 'DVTPlugIn|runFirstLaunch|DVTDownloads|PlugInLoading' | first)"
  elif printf '%s' "$out" | grep -q 'macosx'; then
    e pf_sdks ok
  else
    e pf_sdks "fail|$(printf '%s\n' "$out" | last)"
  fi
  if out=$(DEVELOPER_DIR="$dev" xcrun metal --version 2>&1); then
    e pf_metal "ok|$(printf '%s\n' "$out" | first)"
  else
    e pf_metal "fail|$(printf '%s\n' "$out" | last)"
  fi
fi

# Build tools exactly where the workload looks: the fixed PATH, from the checkout (rustup reads
# rust-toolchain.toml from the working directory).
cd "$CMUX_ROOT" 2>/dev/null || cd "$HOME"
# The probe above ran the Xcode shims already; a hashed /usr/bin path would beat WORKLOAD_PATH.
hash -r
# Without a developer directory the /usr/bin git, xcodebuild and xcrun shims open an install dialog.
xcode-select -p >/dev/null 2>&1 && shims=run || shims=skip
for t in $WORKLOAD_TOOLS; do
  p=$(PATH="$WORKLOAD_PATH" command -v "$t" 2>/dev/null)
  v=""
  case "$p" in /usr/bin/*) [ "$shims" = run ] || v="not run: no developer directory selected" ;; esac
  if [ -n "$p" ] && [ -z "$v" ]; then
    case "$t" in
      zig) v=$("$p" version 2>&1 | first) ;;
      xcodebuild) v=$("$p" -version 2>&1 | first) ;;
      *) v=$(PATH="$WORKLOAD_PATH" "$p" --version 2>&1 | grep -v '^info:' | first) ;;
    esac
  fi
  e pf_tool "$t|$p|$v"
done
rustup=$(PATH="$WORKLOAD_PATH" command -v rustup 2>/dev/null)
channel=$(sed -nE 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$CMUX_ROOT/Native/DiffSidecar/rust-toolchain.toml" 2>/dev/null | head -1)
if [ -n "$channel" ] && [ -n "$rustup" ]; then
  out=$(PATH="$WORKLOAD_PATH" "$rustup" run "$channel" rustc --version 2>&1)
  cargo=$(PATH="$WORKLOAD_PATH" "$rustup" run "$channel" cargo --version 2>&1)
  e pf_rust_channel "$channel|$(printf '%s\n' "$out" | grep -v '^help:' | last)|$(printf '%s\n' "$cargo" | grep -v '^help:' | last)"
elif [ -n "$channel" ]; then
  e pf_rust_channel "$channel|"
fi
# The toolchain plain cargo and rustc resolve to outside the checkout (the manifest's rustup_default).
[ -n "$rustup" ] && e pf_rust_default "$(cd "$HOME" && PATH="$WORKLOAD_PATH" "$rustup" default 2>&1 | grep -v '^info:' | first)"

# glaeda-mini-enroll's interpreter search, in its order.
py=""
for c in $PYTHONS; do
  c="${c/#\~/$HOME}"
  [ -x "$c" ] || continue
  v=$("$c" -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null) || continue
  case "$v" in 3.1[3-9]|3.[2-9][0-9]|[4-9].*) py="$c|$v"; break ;; esac
  [ -n "$py" ] || py="|$v"
done
e pf_python "$py"
# python3 as the workload resolves it; the bootstrap's profileRunnerInterpreter needs 3.13+ (os.waitid).
p3=$(PATH="$WORKLOAD_PATH" command -v python3 2>/dev/null)
v3=""
case "$p3" in /usr/bin/*) [ "$shims" = run ] || v3="not run: no developer directory selected" ;; esac
if [ -n "$p3" ] && [ -z "$v3" ]; then
  v3=$("$p3" -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>&1 | first)
fi
e pf_python3 "$p3|$v3"
[ -x /opt/homebrew/bin/python3.13 ] && e pf_brew_python3 /opt/homebrew/bin/python3.13
# gh as the workload resolves it (cmux CI jobs call it with GH_TOKEN); only its path, so nothing runs.
e pf_gh "$(PATH="$WORKLOAD_PATH" command -v gh 2>/dev/null)"

# The cmux checkout: submodules, setup artifacts and local changes. Then the pinned candidate.
if [ -e "$CMUX_ROOT/.git" ]; then
  e pf_cmux present
  e pf_cmux_pin "$(cat "$CMUX_ROOT/.xcode-version" 2>/dev/null)"
  git -C "$CMUX_ROOT" submodule status --recursive 2>&1 | while IFS= read -r line; do e pf_submodule "$line"; done
  e pf_zig_min "$(sed -nE 's/^[[:space:]]*\.minimum_zig_version[[:space:]]*=[[:space:]]*"([0-9.]+)".*/\1/p' "$CMUX_ROOT/ghostty/build.zig.zon" 2>/dev/null | head -1)"
  artifacts=no
  if [ -f "$CMUX_ROOT/ghostty/include/ghostty.h" ]; then
    for k in "$CMUX_ROOT/GhosttyKit.xcframework" "$CMUX_ROOT/ghostty/macos/GhosttyKit.xcframework"; do
      [ -d "$k" ] && artifacts=yes
    done
  fi
  e pf_setup_artifacts "$artifacts"
  e pf_cmux_dirty "$(git -C "$CMUX_ROOT" status --porcelain=v1 --untracked-files=all 2>/dev/null | wc -l | tr -d ' ')"
else
  e pf_cmux missing
fi
source="$CANDIDATE_PIN"
if [ -n "$source" ]; then
  short=${source:0:12}
  staged=no; [ -f "$HOME/Projects/glaeda-generations/$short/stage-receipt.json" ] && staged=yes
  archive=no; [ -f "$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$short/glaeda-$source-aarch64-apple-darwin.tar.gz" ] && archive=yes
  e pf_candidate "$short|$staged|$archive"
  # The digest the enrollment records for a node that runs this candidate (the bootstrap's glaedaGeneration).
  bin="$HOME/Projects/glaeda-generations/$short/bin/glaeda"
  [ "$staged" = yes ] && [ -f "$bin" ] && e pf_candidate_generation "sha256:$(shasum -a 256 "$bin" | cut -d' ' -f1)"
fi
[ -f "$HOME/glaeda/scripts/cmux_fleet.py" ] && e pf_glaeda present || e pf_glaeda missing
[ -e "$HOME/glaeda/.git" ] && e pf_glaeda_head "$(git -C "$HOME/glaeda" rev-parse -q --verify HEAD 2>/dev/null)"
# The options an old ~/glaeda cannot run ("unrecognized arguments"), and whether it could be moved.
if [ -f "$HOME/glaeda/scripts/glaeda-mini-enroll" ]; then
  lacking=""
  for f in $ENROLL_FLAGS; do grep -qF -- "\"$f\"" "$HOME/glaeda/scripts/glaeda-mini-enroll" || lacking="$lacking $f"; done
  e pf_glaeda_lacking "${lacking# }"
  e pf_glaeda_dirty "$(git -C "$HOME/glaeda" status --porcelain=v1 --untracked-files=no 2>/dev/null | wc -l | tr -d ' ')"
fi
[ -d "$HOME/.cache/glaeda/cmux-native-cache" ] && e pf_cache_root present || e pf_cache_root missing

# Enrollment progress (glaeda-mini-enroll); the node id itself comes from the probe.
fleet="${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet"
[ -f "$fleet/enrollment.json" ] && e pf_enroll_state "$(plutil -extract state raw "$fleet/enrollment.json" 2>/dev/null)"
[ -f "$fleet/enrollment.json" ] && e pf_enroll_generation "$(plutil -extract glaedaGeneration raw "$fleet/enrollment.json" 2>/dev/null)"
[ -f "$fleet/enrollment.json" ] && e pf_enroll_reason "$(plutil -extract quarantineReason raw "$fleet/enrollment.json" 2>/dev/null)"
[ -f "$fleet/acceptance/cmux_macos_native_build.json" ] && \
  e pf_acceptance "$(plutil -extract result raw "$fleet/acceptance/cmux_macos_native_build.json" 2>/dev/null)"
# A node that adopted a class receipt cannot export one (class acceptance does not chain).
[ -f "$fleet/acceptance/cmux_macos_native_build.json" ] && \
  e pf_acceptance_class "$(plutil -extract executionClass raw "$fleet/acceptance/cmux_macos_native_build.json" 2>/dev/null)"

# Homebrew's owner decides whether an install needs sudo -u <owner>. Only a working brew counts: a stalled
# installer leaves /opt/homebrew (even Cellar and bin) without one, which pf_brew_dir reports instead.
hb=/opt/homebrew
if [ -x "$hb/bin/brew" ]; then
  for mark in "$hb/Cellar" "$hb/bin"; do
    [ -e "$mark" ] && { e pf_brew_owner "$(stat -f %Su "$mark")"; break; }
  done
  # Class receipts pin toolchain strings, so the reference host pins these formulas.
  for f in $PIN_FORMULAS; do
    [ -d "$hb/Cellar/$f" ] || continue
    [ -L "$hb/var/homebrew/pinned/$f" ] && e pf_brew_formula "$f|pinned" || e pf_brew_formula "$f|unpinned"
  done
elif [ -d "$hb" ]; then
  # partial: an interrupted homebrew_fetch (git init done, no brew yet), which fix resumes.
  files=$(find "$hb" -mindepth 1 -not -type d 2>/dev/null | head -1)
  if [ -d "$hb/.git" ]; then kind=partial; elif [ -n "$files" ]; then kind=files; else kind=empty; fi
  e pf_brew_dir "$(stat -f %Su "$hb")|$kind"
fi

# A running or prepared macOS update means a restart is coming.
# Only an install or download; softwareupdate -l or --history from a monitoring job is not an update.
applying='(^|[ /])softwareupdate( .*)? (-i|-ia|-ir|-d|-a|--install|--download|--all|--restart|-R)( |$)'
pgrep -f "$applying" >/dev/null && \
  e pf_update_running "$(pgrep -fl "$applying" | grep -v '^[0-9]* sudo ' | head -1 | cut -d' ' -f2- | cut -c1-120)"
# An update prepared for this build since the last boot is waiting for a restart. One prepared before
# the last boot survived a restart without applying (suspended), which does not block.
booted=$(sysctl -n kern.boottime | sed -E 's/^[{] sec = ([0-9]+).*/\1/')
for f in /System/Volumes/Update/Preflight.plist /System/Volumes/Update/Update.plist; do
  [ -f "$f" ] || continue
  from=$(plutil -extract update-asset-attributes.PrerequisiteBuild raw "$f" 2>/dev/null || plutil -extract BootedOSVersion raw "$f" 2>/dev/null)
  [ -n "$from" ] && [ "$from" = "$(sw_vers -buildVersion)" ] || continue
  to=$(plutil -extract update-asset-attributes.OSVersion raw "$f" 2>/dev/null)
  if [ "$(stat -f %m "$f")" -ge "${booted:-0}" ]; then state=pending; else state=suspended; fi
  e pf_update_prepared "$state|${to:-unknown}"
  break
done
pmset -g custom 2>/dev/null | while IFS= read -r line; do e pf_pmset "$line"; done
# Automatic install and download of macOS updates, as install|download (empty means unset, which is on).
# A build host should take macOS updates on purpose (glaeda-mini-fleet upgrade), not mid-day or as a
# prepared update that waits for a restart.
sw=/Library/Preferences/com.apple.SoftwareUpdate
e pf_autoupdate "$(defaults read $sw AutomaticallyInstallMacOSUpdates 2>/dev/null)|$(defaults read $sw AutomaticDownload 2>/dev/null)"

# Runners registered for this user: the directory and agent name, whether its launchd agent is
# loaded in the GUI domain, whether its listener runs, and whether glaeda-mini-fleet holds it stopped.
for r in "$HOME"/actions-runner*/.runner; do
  [ -f "$r" ] || continue
  dir=$(dirname "$r"); loaded=unknown; listening=no; held=no
  plist=$(cat "$dir/.service" 2>/dev/null || true)
  [ -z "$plist" ] && [ "$(basename "$dir")" = actions-runner-glaeda ] && \
    plist="$HOME/Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
  if [ -n "$plist" ]; then
    launchctl print "gui/$(id -u)/$(basename "$plist" .plist)" >/dev/null 2>&1 && loaded=yes || loaded=no
  fi
  pgrep -f "$dir/bin/Runner.Listener" >/dev/null 2>&1 && listening=yes
  [ -f "$HOME/.local/state/glaeda/mini-fleet/runner-held/$(basename "$dir")" ] && held=yes
  e pf_runner "$(basename "$dir")|$(grep -o '"agentName": *"[^"]*"' "$r" | sed -E 's/.*"([^"]*)"$/\1/')|$loaded|$listening|$held"
done

# Secrets this user can read. PR jobs run as the login user on a runner host, so whatever it can read,
# a PR can read (cmuxterm-hq#595). Paths only, never contents. The secrets directories are walked whole
# (subdirectories and dotfiles count); nullglob drops a pattern with no match, dotglob keeps .files, and a
# literal path still comes through, so every entry must be a readable, non-empty file. A path with a
# newline or tab is skipped: it could forge probe lines.
# The home the paths below expand, for the manifest's ~-prefixed accepted_secrets.
e pf_home "$HOME"
shopt -s nullglob dotglob
while IFS= read -r -d '' f; do
  case "$f" in *$'\n'*|*$'\t'*|*$'\r'*) continue ;; esac
  [ -f "$f" ] && [ -r "$f" ] && [ -s "$f" ] || continue
  case "$f" in
    *.pub) continue ;;
    "$HOME/.config/gh/hosts.yml") grep -q oauth_token "$f" 2>/dev/null || continue ;;
    "$HOME/.docker/config.json") grep -Eq '"auth"[[:space:]]*:' "$f" 2>/dev/null || continue ;;
  esac
  e pf_secret "$f"
done < <(
  find -H /Users/Shared/cmux-build-fleet/secrets "$HOME/.secrets" \
    "$HOME/Library/Application Support/cmux-build-fleet/secrets" \
    "/Library/Application Support/cmux-build-controller/secrets" \( -type f -o -type l \) -print0 2>/dev/null
  printf '%s\0' "$HOME"/.config/glaeda/*.key "$HOME"/.ssh/id_* "$HOME/.config/gh/hosts.yml" "$HOME/.netrc" \
    "$HOME/.git-credentials" "$HOME/.docker/config.json" "$HOME/.aws/credentials" "$HOME/.config/rclone/rclone.conf"
)
shopt -u nullglob dotglob
} </dev/null
