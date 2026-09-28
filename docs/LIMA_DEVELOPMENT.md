# Lima development environment

> **Historical development environment:** this persistent guest remains useful for development, but it is not the current one-job production worker design. See [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md).

The Lima integration is an optional macOS development and live-integration convenience. It is not Glaeda's production installation contract. Glaeda itself continues to target an ordinary Debian or Ubuntu host with systemd, cgroup v2, rootless Podman, and a native Glaeda binary.

The existing VM, helper, environment-variable, path, and example names in this historical development lane intentionally retain their `smolrunner`/`SMOLRUNNER_*` identities until their owning migration lanes move those live surfaces.

## Commands

From the repository root:

```bash
make vm-create
make vm-bootstrap
make vm-check
make vm
make vm-stop
```

`vm-create` installs Lima with Homebrew when necessary and creates the `smolrunner` instance from `examples/lima/smolrunner-interactive.yaml`. It preserves an existing instance and never deletes or replaces one. Existence is checked through `limactl`; a stray or stale directory is not accepted as an instance.

`vm-bootstrap` starts the instance and idempotently prepares a development guest. Before package installation or repository execution, it verifies that the running guest is ARM64 and has no Lima host-filesystem mounts. It then installs Ubuntu build and rootless-Podman prerequisites, disables unused rootful Podman socket and automatic-update units, verifies that the system Podman service and socket are disabled and non-active, verifies that no privileged process is listening on `/run/podman/podman.sock`, and removes that path only when it is a proven-stale, non-symlink, root-owned socket inode. It then installs the stable Rust toolchain with rustup when absent, creates or safely fast-forwards the guest checkout, builds with the committed lockfile, and runs `glaeda doctor`.

`vm-check` is read-only. It repeats the guest-isolation check, reports host and guest resource use, reports the system Podman unit states, rejects an enabled or active rootful Podman control path, rejects a live listener, and fails closed on a symlink or ambiguous filesystem entry. A non-listening root-owned stale socket inode is reported distinctly rather than treated as an exposed API. The check also prints the rootless Podman execution mode, checks the guest checkout, and runs `glaeda doctor`.

The existing `vm`, `vm-up`, `vm-tmux`, `vm-status`, `vm-sync`, `vm-doctor`, `vm-observe`, and `vm-stop` commands remain available for normal operation.

## Safety boundary

The checked-in template deliberately uses native Apple Virtualization (`vz`) with an ARM64 Ubuntu guest, no host directory mounts, no port forwarding, no Rosetta, no containerd, no SSH-agent forwarding, and no inherited proxy environment.

An existing instance is not assumed to match that template merely because its name is `smolrunner`. `vm-bootstrap` fails before `apt`, Git, Cargo, or Podman execution when the guest exposes a Lima host mount or is not ARM64. This matters because the Lima login user has passwordless sudo and repository builds may execute build scripts.

The privileged Podman check relies on three independent facts: systemd enablement, systemd active state, and the kernel's Unix-listener table. Filesystem presence alone is not treated as proof of exposure because systemd may leave a stale socket inode behind after deactivation. Bootstrap mutates that stale entry only after proving its exact type, ownership, non-symlink status, and absence from the listener table.

The bootstrap does not:

- register a GitHub Actions runner;
- request or store GitHub credentials;
- install Caddy or publish preview ports;
- expose either the rootful or rootless Podman API socket;
- delete, reset, or resize a Lima instance;
- prepare a production host.

The Lima login user has passwordless sudo for VM administration. Repository jobs must not run directly as that administrative user once a real GitHub Actions runner is introduced. Runner registration belongs behind Glaeda's dedicated non-sudo account and rootless-container execution boundary.

## Overrides

The helpers accept:

```text
SMOLRUNNER_VM
SMOLRUNNER_GUEST_REPO
SMOLRUNNER_REPO_URL
SMOLRUNNER_REPO_REF
SMOLRUNNER_LIMA_CONFIG
```

The defaults are the `smolrunner` instance, `/home/lima/smolrunner`, this repository's HTTPS URL, the `main` branch, and the checked-in Lima template.

Overrides are deliberately narrow:

- the instance name must be a simple non-option Lima name;
- the checkout must be a lexical child of `/home/lima`;
- the repository URL must be public HTTPS without embedded credentials;
- the branch must pass a safe host-side subset and Git's exact `check-ref-format --branch` validation before checkout.

These checks prevent option injection, accidental credential persistence in `.git/config`, and cloning or building into arbitrary guest paths. An override still identifies trusted development input; the helper does not authenticate arbitrary source code.

## Future convergence

The guest bootstrap is development scaffolding, not a second host-management implementation. As durable host reconciliation becomes executable, the intended flow is:

```text
Lima helper
  -> create a plain supported Linux guest
  -> install a checksum-verified Glaeda release
  -> glaeda host plan
  -> glaeda host prepare
  -> live integration checks
```

At that point, package, account, subordinate-ID, state-directory, Podman, and systemd mutations should move out of the shell helper and into Glaeda's typed, journaled, plan-before-mutation implementation. The helper should remain responsible only for the macOS-to-Linux VM boundary and development checkout.

Destructive reset/delete commands should be added only with an explicit confirmation value and a bounded pre-destruction observation report. Automatic GitHub runner registration should remain a separate command with short-lived token handling.
