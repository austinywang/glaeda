#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
helper="${repo_root}/scripts/macbook-runner-bootstrap.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

mkdir -p "${tmp}/bin" "${tmp}/home"
touch "${tmp}/lima.yaml"
log="${tmp}/limactl.log"

cat >"${tmp}/bin/uname" <<'FAKE'
#!/usr/bin/env bash
printf 'Darwin\n'
FAKE

cat >"${tmp}/bin/limactl" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${SMOLRUNNER_TEST_LOG}"
case "${1:-}" in
  list)
    printf '%s\n' "${SMOLRUNNER_VM}"
    ;;
  shell)
    exit "${SMOLRUNNER_TEST_SHELL_STATUS:-0}"
    ;;
  create|start)
    ;;
  *)
    printf 'unexpected fake limactl command: %s\n' "$*" >&2
    exit 1
    ;;
esac
FAKE

chmod +x "${tmp}/bin/uname" "${tmp}/bin/limactl"

cat >"${tmp}/ambient-bash-env" <<'AMBIENT'
printf 'ambient BASH_ENV reached the helper\n' >&2
return 99
AMBIENT
export BASH_ENV="${tmp}/ambient-bash-env"

helper_env=(
  "PATH=${tmp}/bin:/usr/bin:/bin"
  "HOME=${tmp}/home"
  "LANG=C"
  "LC_ALL=C"
  "SMOLRUNNER_TEST_LOG=${log}"
  "SMOLRUNNER_VM=smolrunner"
  "SMOLRUNNER_GUEST_REPO=/home/lima/smolrunner"
  "SMOLRUNNER_REPO_URL=https://github.com/teamleaderleo/smolrunner.git"
  "SMOLRUNNER_REPO_REF=main"
  "SMOLRUNNER_LIMA_CONFIG=${tmp}/lima.yaml"
)

run_helper() {
  env -i "${helper_env[@]}" bash "${helper}" "$@"
}

: >"${log}"
run_helper create >/dev/null
if grep -q '^create' "${log}"; then
  printf 'existing instance must not be recreated\n' >&2
  exit 1
fi

: >"${log}"
if env -i \
  "${helper_env[@]}" \
  SMOLRUNNER_TEST_SHELL_STATUS=1 \
  bash "${helper}" bootstrap >/dev/null 2>&1
then
  printf 'unsafe guest preflight unexpectedly succeeded\n' >&2
  exit 1
fi
if [ "$(grep -c '^shell' "${log}")" -ne 1 ]; then
  printf 'bootstrap must stop after the first failed guest-boundary shell\n' >&2
  cat "${log}" >&2
  exit 1
fi

for assignment in \
  'SMOLRUNNER_VM=--help' \
  'SMOLRUNNER_GUEST_REPO=/tmp/smolrunner' \
  'SMOLRUNNER_REPO_URL=https://token@example.com/repo.git' \
  'SMOLRUNNER_REPO_REF=-unsafe'
do
  : >"${log}"
  name="${assignment%%=*}"
  value="${assignment#*=}"
  if env -i \
    "${helper_env[@]}" \
    "${name}=${value}" \
    bash "${helper}" create >/dev/null 2>&1
  then
    printf 'unsafe override unexpectedly succeeded: %s\n' "${assignment}" >&2
    exit 1
  fi
  if [ -s "${log}" ]; then
    printf 'unsafe override reached limactl: %s\n' "${assignment}" >&2
    cat "${log}" >&2
    exit 1
  fi
done

printf 'macbook runner bootstrap safety tests passed\n'
