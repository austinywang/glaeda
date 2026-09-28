# Quarry parallel-full: Big Red triangulated batch result

Status: GREEN. One Glaeda-managed batch runs Quarry's exact
repository-owned `parallel-full` verifier to completion (15/15 shards
passed, including `verifier`) with a truthful canonical receipt, at
Quarry `468ccc44` / tree `9316a33c`. Landed as
`scripts/quarry_parallel_impl.py` with unit gate
`scripts/test-quarry-parallel.py` (21 tests), plus a bounded channel
extension to the shared task primitive
(`execute_capturing`/`execute_capturing_split` in
`scripts/owned_linux_task.py`).

## Landed slice

One Glaeda-managed batch runs Quarry's exact repository-owned
`parallel-full` verifier — complete corpus, internal sharding, canonical
v2 `--receipt-stdout` channel — inside the shared task-private
systemd/bubblewrap boundary (network `none`, closed environment,
absolute argv only) and returns one bounded canonical receipt
(`glaeda-quarry-parallel-receipt`, stdout only).

```text
exact resident Quarry commit/tree + clean checkout + observed pytest closure
-> experiment admission gate (pressure, memory/CPU reserve, VPN lane, RDP observed)
-> pristine task-private materialization from resident Git
-> writable task clone (full object copy, revalidated commit/tree/clean)
   + origin/main ref fetch (the verifier observes it; a detached clone lacks it)
   + task-local .git/info/exclude for the build mountpoint (never committed)
-> fixed venv skeleton (/venv: pyvenv.cfg + symlinks + ro closure bind)
-> systemd-run grant: CPUQuota=400%, MemoryHigh=6G, MemoryMax=8G,
   TasksMax=512, RuntimeMaxSec=900, network none
-> fixed argv: task-python scripts/run_local_tests.py parallel-full
   --workers 4 --receipt-stdout
-> split-stream bounded capture (stdout receipt <= 65,536; stderr private digest)
-> unit settlement, task cleanup
-> one canonical receipt: source, toolchain, workload, grant, admission
   before/after, per-shard states + output digests, outer terminal/cleanup
```

No queue, daemon, scheduler, remote CLI, or caller-selected command,
mount, or resource property. Quarry owns the verifier and its receipt
semantics; Glaeda owns admission, isolation, bounded capture, cleanup
observation, and the outer receipt.

## Admission: VPN lanes and interactive RDP are never starved

Same gate as the sweep slice: refuses before any materialization when
the Tailscale lane is not `IFF_UP`, PSI `avg10` exceeds cpu 30 / mem 10 /
io 30, available memory is below the 8 GiB job+owner reserve, or fewer
than 6 CPUs are visible. The job carries no network traffic and is
capped at 4 of 16 CPUs / 8 GiB of 30 GiB. RDP (`:3389` LISTEN) is
observed and recorded before/after every attempt rather than required.

## Complete comparable loop

Exact workload: Quarry `468ccc44` / tree `9316a33c`, workers 4,
`--receipt-stdout`, pytest 9.1.1 on CPython 3.14.4.

| Arm | Result | Wall | Worker-visible bytes |
| --- | --- | ---: | --- |
| Cold direct | completed, 15/15 passed, cleanup passed | 84.1 s | 8,049 receipt |
| Glaeda managed | completed, 15/15 passed, cleanup passed | 87.8 s | 8,049 receipt |

No speedup is claimed and none is expected: the managed arm adds
one full-clone materialization plus the sandbox boundary for ~4 s
(4.4%) over cold. The product win is admission, isolation, and
bounded identity-bearing evidence for an external workload that
previously had none.

On digests: the v2 canonical receipt embeds per-shard `wall_millis`,
so byte-identical output digests across runs are not expected and
not the bar. Semantic equality is proven instead: same termination
(`completed`), same per-shard states (15/15 `passed`, same names),
same cleanup (`passed`), same receipt size (8,049 bytes), same
bound source (`468ccc44`/`9316a33c`).

## What the failures taught (the actual findings)

Three sandbox-shaped failures, each diagnosed to a root cause:

1. **Read-only source vs worktree creation.** `parallel-full` creates
   git worktrees inside its checkout. Fix: writable task clone (full
   object copy, `--no-hardlinks`, revalidated) bound writable; canonical
   resident state is never mounted and shares no inodes. This is the
   physical meaning of #1011's "task-private checkout" requirement.

2. **Toolchain probe requires a venv interpreter.** Quarry's
   `capture_verifier_toolchain` probes `sys.executable -I`, which only
   sees pytest when the launcher itself runs as a venv interpreter.
   Fix: fixed task venv skeleton (`pyvenv.cfg` + symlinks + read-only
   resident closure bind), with `quarry.__file__` precedence proven to
   resolve to the task source, not the venv's editable install.

3. **Detached clones lack the observed `origin/main` ref.** The
   verifier's repository observation reads `refs/remotes/origin/main`
   (falling back to `refs/heads/main`); the continuation test failed
   deterministically 2/2 managed vs 2/2 cold passing plus 50/50
   standalone passes. Fix: fetch exactly that ref from the admitted
   resident checkout into the task clone and verify equality. Bisected
   through namespace isolation (not network, not uid, not tmpfs) down
   to the exact `git rev-parse` error text.

4. **Tmpfs sizing is workload-shaped.** Fifteen materialized shard
   worktrees do not fit the shared 512 MiB `/tmp` tmpfs (honest
   `ENOSPC` with truthful started/cancelled/not-started states — the
   receipt's cancellation semantics working as designed). Fix:
   workload-scoped 2 GiB `/quarry-tmp` tmpfs; the shared constant is
   untouched.

## Open gap: the accepted toolchain closure

RESOLVED during this session, from both sides:

- **Executor side (this repo):** the venv skeleton now mirrors the
  resident venv's console binaries — in-venv entries are copied
  (symlinks would dangle; resident paths are unmounted), venv-shebang
  scripts are copied with the shebang rewritten to `/venv/bin/python`,
  system-path entries are symlinked. The previously failing profile
  file passes 6/6 under the rebased interpreter.
- **Workload side (quarry #1136, merged as `468ccc44`):**
  `require_profile_binaries()` probes the exact ruff invocation shape
  at `_run_profile` entry and `parallel-full` startup, so a
  non-conforming interpreter is refused by name in ~1 s instead of
  failing one shard after a full batch. No receipt-schema change.
- **Ruled out with evidence:** toolchain identity was never the gap —
  `capture_verifier_toolchain()` yields the identical id under
  `TZ=UTC` and host CST.

The probe paid for itself immediately: it caught the first version
of the skeleton fix (dangling symlinks) in 1 second with a named
refusal, before a full batch was burned. Fail-fast preconditions
compose across the executor/workload boundary: the workload names
its invariant executably, the executor satisfies it structurally.

With the closure question closed, the remaining step toward feeding
the Rust adapter's success path is the personal-worker attempt
machinery (reservations, bindings, generations) that this slice
deliberately does not invent.

## What this does not do

- No Rust-adapter integration: receipts stay in the Python experiment
  boundary. Feeding `quarry_parallel_verification_adapter` requires the
  accepted closure above plus the personal-worker attempt machinery
  (reservations, bindings, generations) that this slice deliberately
  does not invent.
- No durable replay store: each attempt executes fresh under a
  workload-identity unit name that refuses on collision.
- No production admission-root installation or machine mutation.

## Proposed next workload: DONE — live composition through the adapter

`tests/quarry_parallel_live_producer_composition.rs` (+ fixture
`tests/fixtures/quarry_parallel_live_receipt_468ccc44.json`, the exact
8,049 producer bytes) feeds the live Big Red capture plus the real
managed-run outer facts (exit 0, start/end millis, complete cleanup)
through the unchanged `quarry_parallel_verification_adapter` into a
`PersonalWorkerRepositoryCompletionInput` with terminal `Passed` and
the receipt digest bound to the live bytes. A second test proves
toolchain drift fails closed as `BindingMismatch`. #1011 items 2-4
are now physical; items 6-10 were already covered by the adapter
contract suite.

Attempt, reservation, and store identity in that test stay
experiment-scoped: it proves live-bytes composition, not queue
authority. The remaining program is the worker loop (reservation
issuance through the reconciler) and B07 durable publication — the
personal-worker attempt machinery that must not be inferred from
test ids.
