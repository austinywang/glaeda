#!/bin/bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  printf 'usage: %s KIND OUTPUT_DIR\n' "$0" >&2
  exit 64
fi

readonly kind="$1"
readonly output_dir="$2"
mkdir -p "$output_dir"
readonly receipt="$output_dir/receipt.txt"
readonly timing="$output_dir/time.txt"
readonly pressure_before="$output_dir/pressure-before.txt"
readonly pressure_after="$output_dir/pressure-after.txt"

case "$kind" in
  hosted|big-red) ;;
  *) exit 64 ;;
esac

revision="$(git rev-parse HEAD)"
[[ "$revision" =~ ^[0-9a-f]{40}$ ]]
start_ns="$(date +%s%N)"
{
  printf 'kind=%s\n' "$kind"
  printf 'source_revision=%s\n' "$revision"
  printf 'job_shell_start_unix_ns=%s\n' "$start_ns"
  printf 'runner_name=%s\n' "${RUNNER_NAME:-unknown}"
  printf 'runner_os=%s\n' "${RUNNER_OS:-unknown}"
  printf 'runner_arch=%s\n' "${RUNNER_ARCH:-unknown}"
  printf 'github_run_id=%s\n' "${GITHUB_RUN_ID:-unknown}"
  printf 'github_job=%s\n' "${GITHUB_JOB:-unknown}"
} >"$receipt"

for resource in cpu memory io; do
  if [[ -r "/proc/pressure/$resource" ]]; then
    printf '== %s ==\n' "$resource" >>"$pressure_before"
    cat "/proc/pressure/$resource" >>"$pressure_before"
  fi
done

printf 'workload=owned-linux-jit-contract-tests\n' >>"$receipt"
set +e
/usr/bin/time -v -o "$timing" python3 scripts/test-owned-linux-jit-task.py
status=$?
set -e

for resource in cpu memory io; do
  if [[ -r "/proc/pressure/$resource" ]]; then
    printf '== %s ==\n' "$resource" >>"$pressure_after"
    cat "/proc/pressure/$resource" >>"$pressure_after"
  fi
done

end_ns="$(date +%s%N)"
{
  printf 'workload_exit_code=%s\n' "$status"
  printf 'workload_end_unix_ns=%s\n' "$end_ns"
  printf 'workload_elapsed_ns=%s\n' "$((end_ns - start_ns))"
} >>"$receipt"

exit "$status"
