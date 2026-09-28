# MacBook runner development bootstrap

This note records the manual Ubuntu 24.04 ARM64 bootstrap used to validate the Lima profiles before `glaeda host prepare` exists.

It is a development procedure, not the final runner-account design. The Lima administrator account is `lima`; the eventual GitHub Actions listener and rootless Podman runtime must use a separate dedicated account reconciled and owned by Glaeda.

## Proven environment

The interactive profile has been exercised with:

- Apple Virtualization Framework (`vz`);
- Ubuntu 24.04 ARM64;
- 4 vCPUs, 3 GiB memory and an 80 GiB sparse disk;
- Lima plain mode with no host mounts or SSH-agent forwarding;
- systemd and cgroup v2;
- rootless Podman using the overlay driver.

The first ARM64 build exposed a target-dependent Rust `st_nlink` width and led to the ARM64 CI check added in PR #102.

The first Alpine pull exposed a separate host-preparation requirement: installing Podman is insufficient when the target account has no subordinate UID/GID ranges. That implementation is tracked in issue #103.

## Install development prerequisites

Inside the guest:

```bash
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  git \
  build-essential \
  curl \
  pkg-config \
  libssl-dev \
  podman \
  uidmap \
  slirp4netns \
  fuse-overlayfs
```

## Inspect subordinate-ID authority

Before changing anything, inspect the exact account and all existing allocations:

```bash
id
grep '^lima:' /etc/subuid /etc/subgid || true
sudo cat /etc/subuid
sudo cat /etc/subgid
```

Do not copy an example range onto a machine where it overlaps another owner.

For the fresh dedicated development VM used during validation, `300000-365535` was proven free and assigned to the `lima` administrator account:

```bash
sudo usermod --add-subuids 300000-365535 lima
sudo usermod --add-subgids 300000-365535 lima

grep '^lima:' /etc/subuid /etc/subgid
```

Expected records for that specific fresh VM:

```text
/etc/subuid:lima:300000:65536
/etc/subgid:lima:300000:65536
```

Refresh the rootless namespace after mapping changes:

```bash
podman system migrate
```

Verify the maps:

```bash
podman unshare cat /proc/self/uid_map
podman unshare cat /proc/self/gid_map
```

Then run a mutation-explicit smoke test:

```bash
podman run --rm docker.io/library/alpine:latest \
  sh -c 'id && uname -m'
```

The final architecture line should be:

```text
aarch64
```

Do not use `ignore_chown_errors`; it changes image ownership semantics rather than fixing the user namespace.

## Validate Glaeda

From the checked-out repository inside the guest:

```bash
cargo check --locked
cargo run --locked --quiet -- --output json doctor
cargo run --locked --quiet -- plan --file examples/quarry.yml
cargo run --locked --quiet -- --output json host plan --file examples/quarry.yml
```

The `examples/quarry.yml` manifest is a fixture. Its proposed `quarry-runner` account, repository and image are not the live MacBook configuration and must not be applied as-is.

## Final product boundary

The final `host prepare` path must:

1. install reviewed prerequisite packages;
2. create the dedicated runner account and home;
3. parse complete bounded `/etc/subuid` and `/etc/subgid` authority state;
4. preserve existing valid allocations and reject overlaps or ambiguity;
5. choose or verify deterministic non-overlapping ranges;
6. apply mappings through reviewed root-lane commands;
7. re-observe authority files after mutation;
8. run `podman system migrate` through the sealed runner-user lane when mappings changed;
9. prove the ADR 0019 readiness contract before runner registration or image builds.

Until that path lands, the manual `lima` account bootstrap is only for local development and smoke testing.
