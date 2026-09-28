# Agent execution safety

Read this before changing ownership/adoption, persistence, mutation, recovery,
subprocess, host, or physical-experiment behavior. `AGENTS.md` owns universal
entry rules; this file owns their cold mechanics.

## Ownership and adoption

- Use kind-specific canonical resource constructors. Desired identities carry minimum
  immutable evidence. Observations may omit evidence only to return `unknown`;
  present evidence must validate canonically.
- Unmarked exact-evidence matches require explicit adoption. Names, paths, PIDs,
  tags, survivors, cache presence, and lease IDs alone prove no ownership.
- Re-observe before adoption, mutation, cleanup, or release. Restores stay
  quarantined until fresh host, GitHub, and backend evidence satisfies ownership.
- Follow `docs/adr/0004-lease-lifecycle-core.md`: accepted transitions advance
  revision; terminal leases stay terminal.

## Durable authority and reusable state

Authority-bearing or decision state requires atomic persistence/locking,
restrictive permissions/symlink defense, crash recovery/migrations, and
installation/generation identity.

Performance hints may skip it only when bounded, privacy-preserving,
non-authoritative, disposable, and irrelevant to safe mutation/reuse. A hint that
changes safety is decision state.

Exact-input mismatch, corrupt reusable state, invalid project lease, or stale
toolchain generation takes the miss/reset/cold path. Separate project-local
mutable state across trust/project boundaries. Shared writable state needs
poisoning, ownership, quota, and publication rules. Public
journals contain only bounded public receipts and failures.

## Mutation and recovery

- Add an apply path only after ownership persistence/root-elevation, runner-user,
  journal, credential, and package-rollback requirements exist.
- Every host mutation needs plan/dry-run behavior and a rollback or compensation
  class. Invalid or unconfirmed irreversible work blocks the batch before the
  first executor call. Planning stays side-effect free.
- Compensation is a bounded forward action after failure, distinct from exact
  restoration. Fresh post-effect observation determines actual state.
- Automatic repair needs action policy, budget, circuit breaker, exact owner,
  durable checkpoint, fresh observation, and rollback/compensation class.
- Privileged control replacement requires exact digest, compatible state, drained
  work/journals, previous verified version, preserved launch config, and health
  evidence.
- Fleet desired state and optimizer recommendations remain inputs. Local
  ownership conflicts, unknown state, active recovery/work, and operator holds
  veto them.

## Process and dependency safety

- Child programs use absolute executable paths and argv, with no implicit shell.
  Environments start empty and receive only explicitly allowlisted values.
- Output redaction is defense in depth; it grants no authority to expose secrets
  to a child.
- Prefer stable system tools for package-manager, filesystem, systemd, Git,
  container, proxy, and TLS behavior. Keep Linux behind a narrow host/backend
  boundary; unsupported platforms fail clearly.
- Add dependencies for concrete need plus maintenance rationale. Pin third-party
  GitHub Actions to reviewed commit SHAs.
- Tests require no root, systemd, Podman, formatting, VM creation, reverse proxy,
  or live credentials unless explicitly marked physical/integration with exact
  opt-in. Manifests own host/execution policy; repository files own builds.

## Performance evidence

A performance claim names baseline/candidate, comparable-work contract and
validator, primary latency, secondary CPU/RAM/disk effects, and fallback/reset.
Builder/benchmark metadata grants no stronger cache, artifact, or residency
authority. Evidence stays local-first, bounded, versioned, and content-minimised.

## Physical experiments

Record exact Glaeda head, hardware/OS class, backend, guest/kernel/filesystem,
project/revision, toolchain/package manager, resource profile, and candidate.
Then:

1. hold one semantic workload constant while changing one optimization;
2. separate cold and warm/reuse paths;
3. separate physical allocated bytes and host backing growth from logical size;
4. retain exact cleanup/rebuild evidence for experiment-created resources;
5. quarantine ambiguous physical state; avoid broad sweeping;
6. publish only bounded results and opaque/canonical identities.

Formatting filesystems, creating VDO state, replacing VMs, deleting disks, or
mutating operator-machine networking belongs in an explicitly authorized
physical path outside ordinary hosted CI.
