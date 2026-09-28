#!/usr/bin/env bash
set -euo pipefail

instance="${SMOLRUNNER_VM:-smolrunner}"
guest_repo="${SMOLRUNNER_GUEST_REPO:-/home/lima/smolrunner}"
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "${script_dir}/.." && pwd)"
lima_home="${LIMA_HOME:-${HOME}/.lima}"

case "${instance}" in
  [a-z0-9]*) ;;
  *)
    printf 'error: SMOLRUNNER_VM must begin with a lowercase ASCII letter or digit\n' >&2
    exit 1
    ;;
esac
case "${instance}" in
  *[!a-z0-9._-]*)
    printf 'error: SMOLRUNNER_VM must be one simple lowercase ASCII Lima instance name\n' >&2
    exit 1
    ;;
esac
[ "${#instance}" -le 64 ] || {
  printf 'error: SMOLRUNNER_VM exceeds the reviewed instance-name bound\n' >&2
  exit 1
}
case "${lima_home}" in
  /*) ;;
  *)
    printf 'error: LIMA_HOME must be an absolute path\n' >&2
    exit 1
    ;;
esac
case "${lima_home}" in
  /|*/|*//*|*/./*|*/../*|*/.|*/..)
    printf 'error: LIMA_HOME must be a lexically canonical non-root path\n' >&2
    exit 1
    ;;
esac
[ "${#lima_home}" -le 1024 ] || {
  printf 'error: LIMA_HOME exceeds the reviewed path bound\n' >&2
  exit 1
}

instance_dir="${lima_home}/${instance}"
operation_lock="${instance_dir}/smolrunner-vm-helper.lock"
active_run_marker="${instance_dir}/smolrunner-operator-run.active"
lock_held=0
run_marker_held=0

usage() {
  cat <<'USAGE'
Usage: bash scripts/macbook-runner-vm.sh COMMAND [ARGS...]

Commands:
  up                         Start the configured Lima instance if it is stopped.
  profile interactive|work   Select the exact reviewed VM resource profile.
  run PROFILE -- CMD...      Select PROFILE and run one explicit guest command.
  shell                      Start the VM and open an interactive guest shell.
  tmux                       Start the VM and attach/create the guest tmux session.
  status                     Show Lima instance state and host/guest Git branch status.
  sync                       Fast-forward the clean guest checkout to origin/main.
  doctor                     Run Glaeda doctor inside the guest checkout.
  observe                    Run the read-only Mac/guest observation report.
  exec -- CMD                Run an explicit command inside the guest.
  stop                       Gracefully stop the Lima instance when no helper run is active.

Reviewed profiles:
  interactive  4 vCPU, 3 GiB memory
  work         8 vCPU, 10 GiB memory

Environment:
  SMOLRUNNER_VM          Lima instance name (default: smolrunner)
  SMOLRUNNER_GUEST_REPO  Guest checkout path (default: /home/lima/smolrunner)
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ "${run_marker_held}" -eq 1 ]; then
    rm -f -- "${active_run_marker}"
    run_marker_held=0
  fi
  if [ "${lock_held}" -eq 1 ]; then
    rmdir -- "${operation_lock}" 2>/dev/null || true
    lock_held=0
  fi
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

require_lima() {
  command -v limactl >/dev/null 2>&1 \
    || die 'limactl is unavailable; install Lima first'
}

require_instance() {
  [ -d "${instance_dir}" ] \
    || die "Lima instance '${instance}' does not exist. Create it from examples/lima/glaeda-interactive.yaml before using this helper."
}

acquire_operation_lock() {
  if ! mkdir -- "${operation_lock}" 2>/dev/null; then
    die 'another VM helper operation is active; refusing concurrent profile, run, or stop'
  fi
  lock_held=1
}

profile_values() {
  case "$1" in
    interactive)
      profile_name=interactive
      profile_cpus=4
      profile_memory_gib=3
      profile_memory_label=3GiB
      profile_min_memory_kib=2621440
      ;;
    work)
      profile_name=work
      profile_cpus=8
      profile_memory_gib=10
      profile_memory_label=10GiB
      profile_min_memory_kib=9437184
      ;;
    *)
      die "unknown profile '$1'; expected interactive or work"
      ;;
  esac
}

instance_facts() {
  local facts
  facts="$(limactl list --format '{{.Status}}|{{.CPUs}}|{{.Memory}}' "${instance}")" \
    || die 'unable to observe the Lima instance'
  IFS='|' read -r observed_status observed_cpus observed_memory <<EOF_FACTS
${facts}
EOF_FACTS
  [ -n "${observed_status}" ] && [ -n "${observed_cpus}" ] && [ -n "${observed_memory}" ] \
    || die 'Lima returned incomplete instance evidence'
}

vm_is_running() {
  instance_facts
  [ "${observed_status}" = Running ]
}

configured_profile_matches() {
  [ "${observed_cpus}" = "${profile_cpus}" ] \
    && [ "${observed_memory}" = "${profile_memory_label}" ]
}

refuse_active_work() {
  [ ! -e "${active_run_marker}" ] \
    || die 'an operator run is active; refusing profile change or stop'

  if [ "${observed_status}" = Running ]; then
    limactl shell "${instance}" -- /usr/bin/test -x /usr/bin/pgrep >/dev/null 2>&1 \
      || die 'guest process observation is unavailable; refusing to claim the VM is idle'
    set +e
    limactl shell "${instance}" -- /usr/bin/pgrep -f Runner.Worker >/dev/null 2>&1
    local worker_status=$?
    set -e
    case "${worker_status}" in
      0) die 'an Actions worker process is active; refusing profile change or stop' ;;
      1) ;;
      *) die 'guest process observation failed; refusing to claim the VM is idle' ;;
    esac
  fi
}

verify_guest_profile() {
  local guest_cpus guest_memory_kib
  guest_cpus="$(limactl shell "${instance}" -- /usr/bin/nproc)" \
    || die 'unable to verify guest CPU count'
  guest_memory_kib="$(limactl shell "${instance}" -- /usr/bin/awk '/^MemTotal:/ { print $2; exit }' /proc/meminfo)" \
    || die 'unable to verify guest memory'

  case "${guest_cpus}" in
    ''|*[!0-9]*) die 'guest CPU observation is invalid' ;;
  esac
  case "${guest_memory_kib}" in
    ''|*[!0-9]*) die 'guest memory observation is invalid' ;;
  esac

  [ "${guest_cpus}" -eq "${profile_cpus}" ] \
    || die 'guest CPU count does not match the selected profile'
  [ "${guest_memory_kib}" -ge "${profile_min_memory_kib}" ] \
    || die 'guest memory is below the selected profile envelope'
  [ "${guest_memory_kib}" -le "$((profile_memory_gib * 1048576))" ] \
    || die 'guest memory exceeds the selected profile envelope'

  printf 'profile=%s state=running cpus=%s memory=%s verified=true\n' \
    "${profile_name}" "${profile_cpus}" "${profile_memory_label}"
}

start_vm() {
  require_lima
  require_instance
  instance_facts
  case "${observed_status}" in
    Running) ;;
    Stopped)
      limactl start "${instance}"
      ;;
    *)
      die 'Lima instance is transitioning or unavailable; refusing to start'
      ;;
  esac
}

select_profile() {
  profile_values "$1"
  require_lima
  require_instance
  instance_facts

  case "${observed_status}" in
    Running)
      if configured_profile_matches; then
        refuse_active_work
        verify_guest_profile
        return 0
      fi
      refuse_active_work
      limactl stop "${instance}"
      instance_facts
      [ "${observed_status}" = Stopped ] \
        || die 'Lima instance did not reach stopped state after graceful stop'
      ;;
    Stopped)
      refuse_active_work
      ;;
    *)
      die 'Lima instance is transitioning or unavailable; refusing profile change'
      ;;
  esac

  if ! configured_profile_matches; then
    limactl edit --tty=false --cpus "${profile_cpus}" --memory "${profile_memory_gib}" "${instance}"
  fi
  limactl start "${instance}"
  instance_facts
  [ "${observed_status}" = Running ] \
    || die 'Lima instance did not reach running state after profile selection'
  configured_profile_matches \
    || die 'configured Lima resources do not match the selected profile after start'
  verify_guest_profile
}

begin_operator_run() {
  [ ! -e "${active_run_marker}" ] \
    || die 'an operator run is already active'
  umask 077
  (set -C; printf 'profile=%s\n' "${profile_name}" > "${active_run_marker}") 2>/dev/null \
    || die 'unable to record the operator run start'
  run_marker_held=1
  printf 'run profile=%s state=starting\n' "${profile_name}"
}

finish_operator_run() {
  local status="$1"
  rm -f -- "${active_run_marker}"
  run_marker_held=0
  printf 'run profile=%s state=completed status=%s vm_shutdown=explicit\n' \
    "${profile_name}" "${status}"
}

guest_bash() {
  limactl shell "${instance}" -- \
    /usr/bin/env SMOLRUNNER_GUEST_REPO="${guest_repo}" \
    /usr/bin/bash -lc "$1"
}

show_git_status() {
  printf '\n== host checkout ==\n'
  git -C "${repo_root}" status --short --branch || true

  printf '\n== guest checkout ==\n'
  if vm_is_running; then
    guest_bash 'if [ -d "$SMOLRUNNER_GUEST_REPO/.git" ]; then git -C "$SMOLRUNNER_GUEST_REPO" status --short --branch; else printf "missing checkout: %s\n" "$SMOLRUNNER_GUEST_REPO"; fi' || true
  else
    printf 'instance %s is stopped\n' "${instance}"
  fi
}

command_name="${1:-}"
case "${command_name}" in
  up)
    start_vm
    ;;
  profile)
    [ "$#" -eq 2 ] || die 'profile requires exactly interactive or work'
    acquire_operation_lock
    select_profile "$2"
    ;;
  run)
    [ "$#" -ge 4 ] || die 'run requires PROFILE -- CMD...'
    profile_arg="$2"
    [ "$3" = -- ] || die 'run requires -- before the guest command'
    shift 3
    [ "$#" -gt 0 ] || die 'run requires a command after --'
    acquire_operation_lock
    select_profile "${profile_arg}"
    instance_facts
    refuse_active_work
    begin_operator_run
    set +e
    limactl shell "${instance}" -- "$@"
    run_status=$?
    set -e
    finish_operator_run "${run_status}"
    exit "${run_status}"
    ;;
  shell)
    start_vm
    exec limactl shell "${instance}"
    ;;
  tmux)
    start_vm
    exec limactl shell "${instance}" -- /usr/bin/bash -lc '
      if ! command -v tmux >/dev/null 2>&1; then
        printf "tmux is not installed in the guest. Run: sudo apt-get update && sudo apt-get install -y tmux\n" >&2
        exit 1
      fi
      exec tmux new-session -A -s glaeda
    '
    ;;
  status)
    require_lima
    require_instance
    limactl list "${instance}"
    show_git_status
    ;;
  sync)
    start_vm
    guest_bash '
      set -euo pipefail
      cd "$SMOLRUNNER_GUEST_REPO"
      if [ -n "$(git status --porcelain)" ]; then
        printf "guest checkout has local changes; refusing to switch or update\n" >&2
        git status --short --branch >&2
        exit 1
      fi
      git switch main
      git fetch --prune origin main
      git merge --ff-only origin/main
      git status --short --branch
    '
    ;;
  doctor)
    start_vm
    guest_bash '
      set -euo pipefail
      cd "$SMOLRUNNER_GUEST_REPO"
      cargo run --locked --quiet -- --output json doctor
    '
    ;;
  observe)
    require_lima
    bash "${repo_root}/scripts/macbook-runner-observe.sh" "${instance}"
    ;;
  exec)
    shift
    [ "${1:-}" = -- ] && shift
    [ "$#" -gt 0 ] || die 'exec requires a command after --'
    start_vm
    exec limactl shell "${instance}" -- "$@"
    ;;
  stop)
    require_lima
    require_instance
    acquire_operation_lock
    instance_facts
    case "${observed_status}" in
      Stopped)
        printf 'state=stopped memory_released=true persistent_disk_retained=true\n'
        ;;
      Running)
        refuse_active_work
        limactl stop "${instance}"
        instance_facts
        [ "${observed_status}" = Stopped ] \
          || die 'Lima instance did not reach stopped state after graceful stop'
        printf 'state=stopped memory_released=true persistent_disk_retained=true\n'
        ;;
      *)
        die 'Lima instance is transitioning or unavailable; refusing stop'
        ;;
    esac
    ;;
  help|-h|--help|'')
    usage
    ;;
  *)
    usage >&2
    die "unknown command '${command_name}'"
    ;;
esac
