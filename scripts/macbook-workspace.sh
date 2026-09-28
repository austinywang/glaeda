#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "${script_dir}/.." && pwd)"
glaeda_session="${GLAEDA_WORK_SESSION:-}"
legacy_session="${SMOLRUNNER_WORK_SESSION:-}"
session="${glaeda_session:-${legacy_session:-glaeda}}"
vm_helper="${repo_root}/scripts/macbook-runner-vm.sh"
command="${1:-tmux}"

usage() {
  cat <<'USAGE'
Usage: bash scripts/macbook-workspace.sh COMMAND

Commands:
  cmux        Open or select the opt-in cmux Mac/Lima workspace.
  setup-cmux  Install cmux through its reviewed Homebrew cask, then open the workspace.
  tmux        Open or select the default compatibility tmux workspace.
  notify-doctor STATUS
              Emit a fixed cmux notification for a completed doctor run.

Environment:
  GLAEDA_WORK_SESSION      Workspace/session name (default: glaeda)
  SMOLRUNNER_WORK_SESSION  Legacy workspace/session alias; conflicting values are refused
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

validate_session() {
  if [ -n "${glaeda_session}" ] && [ -n "${legacy_session}" ] && \
     [ "${glaeda_session}" != "${legacy_session}" ]; then
    die 'GLAEDA_WORK_SESSION conflicts with legacy SMOLRUNNER_WORK_SESSION'
  fi
  case "${session}" in
    ''|*[!A-Za-z0-9_.-]*)
      die 'GLAEDA_WORK_SESSION must contain only letters, digits, dot, underscore, or dash'
      ;;
  esac
}

require_macos() {
  [ "$(uname -s)" = "Darwin" ] || die 'the MacBook workspace helper supports macOS only'
}

ensure_brew() {
  if command -v brew >/dev/null 2>&1; then
    return
  fi
  for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [ -x "${candidate}" ]; then
      PATH="$(dirname "${candidate}"):${PATH}"
      export PATH
      return
    fi
  done
  die 'Homebrew is required before running the MacBook workspace helper'
}

ensure_lima() {
  if command -v limactl >/dev/null 2>&1; then
    return
  fi
  ensure_brew
  printf 'Installing Lima with Homebrew.\n'
  brew install lima
}

find_cmux_cli() {
  local candidate
  if candidate="$(command -v cmux 2>/dev/null)" && [ -x "${candidate}" ]; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  for candidate in \
    /Applications/cmux.app/Contents/Resources/bin/cmux \
    "${HOME}/Applications/cmux.app/Contents/Resources/bin/cmux" \
    /opt/homebrew/bin/cmux \
    /usr/local/bin/cmux; do
    if [ -x "${candidate}" ]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

install_cmux() {
  if find_cmux_cli >/dev/null; then
    return
  fi

  ensure_brew
  printf 'Installing cmux through the manaflow-ai/cmux Homebrew cask.\n'
  brew tap manaflow-ai/cmux
  brew install --cask cmux
  find_cmux_cli >/dev/null || die 'cmux was installed but its CLI could not be found'
}

launch_cmux() {
  local cmux_cli="$1"

  ensure_lima
  bash "${vm_helper}" up

  /usr/bin/osascript - "${repo_root}" "${session}" "${vm_helper}" "${cmux_cli}" <<'APPLESCRIPT'
on run argv
  set repoPath to item 1 of argv
  set workspaceName to item 2 of argv
  set vmHelper to item 3 of argv
  set cmuxPath to item 4 of argv
  set targetWindow to missing value
  set targetTab to missing value

  tell application "cmux"
    activate

    repeat with candidateWindow in windows
      repeat with candidateTab in tabs of candidateWindow
        if name of candidateTab is workspaceName then
          set targetWindow to candidateWindow
          set targetTab to candidateTab
          exit repeat
        end if
      end repeat
      if targetTab is not missing value then exit repeat
    end repeat

    if targetTab is missing value then
      if (count of windows) is 0 then
        set targetWindow to new window
        set targetTab to selected tab of targetWindow
      else
        set targetWindow to front window
        set targetTab to new tab in targetWindow
      end if
      set hostTerminal to focused terminal of targetTab
      set pullsURL to "https://github.com/teamleaderleo/glaeda/pulls"
      set hostCommand to "cd -- " & quoted form of repoPath & " && " & quoted form of cmuxPath & " rename-workspace --workspace \"$CMUX_WORKSPACE_ID\" -- " & quoted form of workspaceName & " && " & quoted form of cmuxPath & " rename-tab --surface \"$CMUX_SURFACE_ID\" -- \"Mac host\" && " & quoted form of cmuxPath & " set-status lima \"running\" --workspace \"$CMUX_WORKSPACE_ID\" --icon \"server.rack\" --color \"#30d158\" && " & quoted form of cmuxPath & " log --workspace \"$CMUX_WORKSPACE_ID\" --level info -- " & quoted form of ("Mac checkout: " & repoPath) & " && " & quoted form of cmuxPath & " log --workspace \"$CMUX_WORKSPACE_ID\" --level info -- " & quoted form of ("Pull requests: " & pullsURL)
      input text (hostCommand & linefeed) to hostTerminal

      set vmTerminal to split hostTerminal direction right
      set vmCommand to "cd -- " & quoted form of repoPath & " && " & quoted form of cmuxPath & " rename-tab --surface \"$CMUX_SURFACE_ID\" -- \"Lima VM\" && exec /bin/bash " & quoted form of vmHelper & " shell"
      input text (vmCommand & linefeed) to vmTerminal
      focus hostTerminal
    end if

    select tab targetTab
    activate window targetWindow
  end tell
end run
APPLESCRIPT
}

notify_doctor() {
  local status="${1:-}"
  local cmux_cli
  local body

  case "${status}" in
    0) body='Doctor completed successfully.' ;;
    ''|*[!0-9]*) die 'notify-doctor requires a numeric exit status' ;;
    *) body="Doctor failed with exit status ${status}." ;;
  esac

  [ -n "${CMUX_WORKSPACE_ID:-}" ] || return 0
  cmux_cli="$(find_cmux_cli)" || return 0
  "${cmux_cli}" notify --title 'Glaeda doctor' --body "${body}" >/dev/null
}

launch_tmux() {
  local packages=()

  ensure_brew
  command -v tmux >/dev/null 2>&1 || packages+=(tmux)
  command -v limactl >/dev/null 2>&1 || packages+=(lima)
  if [ "${#packages[@]}" -gt 0 ]; then
    printf 'Installing missing Mac tools with Homebrew: %s\n' "${packages[*]}"
    brew install "${packages[@]}"
  fi

  bash "${vm_helper}" up

  if ! tmux has-session -t "${session}" 2>/dev/null; then
    tmux new-session -d -s "${session}" -n host -c "${repo_root}"
  fi

  if ! tmux list-windows -t "${session}" -F '#{window_name}' | grep -Fqx host; then
    tmux new-window -d -t "${session}:" -n host -c "${repo_root}"
  fi

  if ! tmux list-windows -t "${session}" -F '#{window_name}' | grep -Fqx vm; then
    tmux new-window -d -t "${session}:" -n vm -c "${repo_root}" \
      'exec bash scripts/macbook-runner-vm.sh shell'
  fi

  tmux select-window -t "${session}:host"

  if [ -n "${TMUX:-}" ]; then
    exec tmux switch-client -t "${session}"
  else
    exec tmux attach-session -t "${session}"
  fi
}

validate_session

case "${command}" in
  cmux)
    require_macos
    cmux_cli="$(find_cmux_cli)" || die 'cmux is unavailable; run make work-cmux-setup once or use make work'
    launch_cmux "${cmux_cli}"
    ;;
  setup-cmux)
    require_macos
    install_cmux
    cmux_cli="$(find_cmux_cli)"
    launch_cmux "${cmux_cli}"
    ;;
  tmux)
    require_macos
    launch_tmux
    ;;
  notify-doctor)
    notify_doctor "${2:-}"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    die "unknown workspace command: ${command}"
    ;;
esac
