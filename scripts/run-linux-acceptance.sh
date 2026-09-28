#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

for command in cargo docker rustup musl-gcc; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'required command is missing: %s\n' "$command" >&2
    exit 1
  fi
done

case "$(uname -m)" in
  x86_64)
    rust_target=x86_64-unknown-linux-musl
    linker_variable=CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER
    ;;
  aarch64 | arm64)
    rust_target=aarch64-unknown-linux-musl
    linker_variable=CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER
    ;;
  *)
    printf 'unsupported Linux architecture for the acceptance binary: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac

if (($# > 0)); then
  images=("$@")
else
  images=(debian:12-slim ubuntu:24.04)
fi

target_dir=$(mktemp -d)
cleanup() {
  rm -rf "$target_dir"
}
trap cleanup EXIT

rustup target add "$rust_target"
# Build both acceptance crates against the same exact static target before entering the guests.
env \
  "$linker_variable=musl-gcc" \
  CARGO_TARGET_DIR="$target_dir" \
  cargo test --locked \
    --test linux_acceptance \
    --test host_prepare_acceptance \
    --target "$rust_target" \
    --no-run

find_acceptance_binary() {
  local test_name=$1
  find "$target_dir/$rust_target/debug/deps" \
    -maxdepth 1 \
    -type f \
    -name "${test_name}-*" \
    -perm -111 \
    -print \
    -quit
}

linux_acceptance_binary=$(find_acceptance_binary linux_acceptance)
host_prepare_acceptance_binary=$(find_acceptance_binary host_prepare_acceptance)

if [[ -z "$linux_acceptance_binary" ]]; then
  printf 'could not locate the compiled Linux acceptance test binary\n' >&2
  exit 1
fi
if [[ -z "$host_prepare_acceptance_binary" ]]; then
  printf 'could not locate the compiled host-prepare acceptance test binary\n' >&2
  exit 1
fi

for image in "${images[@]}"; do
  case "$image" in
    debian:*) expected_os=debian ;;
    ubuntu:*) expected_os=ubuntu ;;
    *)
      printf 'image must be an explicit Debian or Ubuntu tag: %s\n' "$image" >&2
      exit 1
      ;;
  esac

  docker pull "$image"
  image_receipt=$(docker image inspect --format '{{index .RepoDigests 0}}' "$image")
  printf '\nRunning %s acceptance against %s\n' "$expected_os" "$image_receipt"

  docker run --rm \
    --tmpfs /run:rw,nosuid,nodev,mode=0755 \
    --tmpfs /tmp:rw,nosuid,nodev,mode=1777 \
    --mount "type=bind,src=$linux_acceptance_binary,dst=/usr/local/bin/glaeda-linux-acceptance,readonly" \
    --mount "type=bind,src=$host_prepare_acceptance_binary,dst=/usr/local/bin/glaeda-host-prepare-acceptance,readonly" \
    --env SMOLRUNNER_LINUX_ACCEPTANCE=1 \
    --env "SMOLRUNNER_EXPECTED_OS=$expected_os" \
    "$image" \
    /bin/sh -euxc '
      printf "#!/bin/sh\nexit 101\n" > /usr/sbin/policy-rc.d
      chmod 0755 /usr/sbin/policy-rc.d
      export DEBIAN_FRONTEND=noninteractive
      apt-get update
      apt-get install --yes --no-install-recommends \
        ca-certificates \
        git \
        passwd \
        systemd \
        uidmap \
        util-linux
      rm -rf /var/lib/apt/lists/*
      /usr/local/bin/glaeda-linux-acceptance --test-threads=1 --nocapture
      /usr/local/bin/glaeda-host-prepare-acceptance --test-threads=1 --nocapture
    '
done
