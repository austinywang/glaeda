# Linux acceptance testing

Glaeda keeps ordinary unit tests rootless and host-independent. This acceptance layer adds two explicit test classes without exposing a mutation CLI or treating a container as a systemd virtual machine.

## Test classes

### External recovery-contract tests

`tests/durable_journal_recovery.rs` uses the public journal traits from outside the crate. It injects interruption at every checkpoint defined by ADR 0014:

1. initial journal publication;
2. before action execution;
3. after successful action execution;
4. before the next action;
5. after failed action execution;
6. before rollback or compensation;
7. after rollback or compensation.

Each case verifies the checkpoint phase, action identity, attempted snapshot, last durable snapshot, and the number of executor calls. A rerun case starts with retained `executing` evidence and verifies that a conflicting initial publication stops before execution instead of replacing that evidence.

Run it with:

```bash
cargo test --locked --test durable_journal_recovery
```

### Disposable Debian and Ubuntu containers

`tests/linux_acceptance.rs` is compiled as a static musl test binary. `scripts/run-linux-acceptance.sh` mounts only that binary into fresh containers and runs it as root inside the disposable guest. The host source tree, home directory, SSH agent, Git configuration, and credentials are not mounted.

The default images are:

- `debian:12-slim`;
- `ubuntu:24.04`.

The script pulls each image and prints the resolved repository digest before running it. The container is not privileged and does not run systemd as PID 1.

Host requirements:

- Linux on x86-64 or ARM64;
- Docker;
- Rustup and Cargo;
- `musl-gcc` from the distribution's `musl-tools` package.

Run both defaults with:

```bash
bash scripts/run-linux-acceptance.sh
```

Run one explicit image with:

```bash
bash scripts/run-linux-acceptance.sh debian:12-slim
```

The dedicated `.github/workflows/linux-acceptance.yml` workflow installs the native musl linker, runs the recovery-contract tests, and then runs both container images. It has read-only repository permissions and receives no secrets.

## Coverage

| Check | Ordinary test | Disposable container | Real systemd VM |
| --- | --- | --- | --- |
| Fixed absolute executable paths | Constructor tests | Root-owned file metadata on Debian and Ubuntu | Recheck on operator image |
| Empty child environment | Process unit tests | Real `/usr/bin/env` execution | Recheck through services |
| Explicit environment allowlist | Process unit tests | Real `/usr/bin/env` execution | Recheck through services |
| Effective UID gate | Injected probe tests | Real effective UID 0 | Real elevation path |
| Root-lane command delivery | Injected executor tests | Group, user, home, subordinate UID, and subordinate GID commands | Recheck with operator elevation policy |
| Runner-user transition | Exact argv tests | Real `runuser` transition and `git --version` | Recheck with PAM and logind active |
| Account and group observation | Fake receipts | Real NSS lookups | Recheck configured NSS sources |
| Home metadata | Fake filesystem | Real owner, group, type, and mode | Recheck persistent disk |
| Subordinate UID/GID observation | Parser and fake filesystem | Real `/etc/subuid` and `/etc/subgid` | Recheck operator allocations |
| Linger observation | Fake filesystem | Protected relocated marker | Real `loginctl` and systemd marker |
| Package observation | Fake dpkg receipts | Real dpkg inventory on both distributions | Recheck operator package state |
| Journal interruption boundaries | Public integration tests | Same binary contract | Abrupt process and VM interruption |

## What the container test changes

The test creates one account named `smolaccept` inside the disposable container, assigns dedicated subordinate ranges, creates its home, and creates a test runtime directory. It then observes the resulting state and verifies that a second plan has no account, home, or subordinate-ID commands left to execute.

The test also creates protected relocated subordinate-ID files and a protected empty linger marker to exercise the conservative filesystem observer. This validates marker classification only. It does not prove that a systemd user manager is active.

All changes disappear when the container exits.

## Checks reserved for a real VM

The following checks require a booted Debian or Ubuntu VM with systemd, root privileges, and an operator-controlled test window:

1. Execute the reviewed `loginctl enable-linger` command and verify the resulting marker through the default system path.
2. Verify `/run/user/UID` is created and owned by logind instead of a test fixture.
3. Execute rootless `podman info` through the verified runner-user lane and confirm user-namespace mappings.
4. Kill the process or VM after each journal publication boundary, then inspect the persisted journal and re-observe host state before any retry.
5. Interrupt atomic journal replacement around temporary-file write, file synchronization, rename, and parent-directory synchronization.
6. Reboot and repeat account, package, subordinate-ID, home, linger, and executable observations.
7. Run the harness inside the Apple-silicon MacBook VM to cover the ARM64 guest path.
8. Suspend and resume the Mac while the VM is idle, then repeat read-only observations before starting another job.

A privileged container must not be used as evidence for these checks. Record the exact Glaeda commit, guest image, architecture, commands, and journal files for every VM run.

## Failure interpretation

A failed command after process creation leaves host state uncertain. Preserve the journal, re-observe the affected resource, and classify the fresh evidence before retrying. An `executing` or `rollback_in_progress` record describes uncertainty; it does not authorize replay.
