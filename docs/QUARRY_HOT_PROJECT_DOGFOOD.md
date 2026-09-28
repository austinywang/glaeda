# Quarry hot-project dogfood experiment

Status: software-side experiment contract. Physical project-disk mutation remains sealed behind the #565 P2/P3/P4 lifecycle, the #628 full-correlation rerun, and the later sealed #589 activation path.

This document owns the first Quarry dogfood workload for #560. It keeps the control path, treatment path, state placement, measurement boundaries, fallback, and promotion bar fixed before the persistent project disk is exercised.

## Question

Measure the agent-visible value of retaining Quarry project state on one separately owned persistent Linux project disk while keeping source, verification, lifecycle, and credential authority outside that disk.

Phase one changes one major storage dimension:

```text
control
  current trusted-Mac Quarry path

versus

treatment
  same Mac / Lima-VZ resource profile
  + dedicated ext4 persistent project disk
  + resident exact source / dependency state
  + task-private Git metadata
  + OverlayFS task view
```

XFS/reflink is a later experiment. It enters only when the ext4 treatment leaves a measured task-materialization or physical-byte reservoir worth attacking.

## Physical prerequisite

The treatment becomes runnable only after current reviewed Glaeda authority can compose the full current chain:

```text
#639 / #565 P2 descriptor-bound host observation
-> #644 / #565 P3 durable create provenance + exact physical/backing binding
-> #645 / #565 P4 accepted filesystem generation + exact current attachment
-> #628 observation-only full-correlation rerun succeeds on that genuine disk
-> #640 sole sealed production project-filesystem correlation path
-> #589 all-FD OverlayFS transaction
-> #580 exact private Git/source proof
-> task ready
```

Missing, drifting, ambiguous, or incompatible evidence routes the task to the ordinary private/cold path.

The experiment tool added with this document performs planning and receipt construction only. It cannot create, format, attach, unlock, resize, repair, or delete disks and cannot execute guest-control requests.

## Persistent project-disk contents

The project disk is useful working state. Surviving bytes grant zero independent source, verification, lifecycle, cleanup, or merge authority.

| State class | Phase-one placement | Lifetime | Reuse / invalidation rule |
| --- | --- | --- | --- |
| immutable Git object pool | persistent project disk | immutable accepted generation | exact pool/source generation; new generation on mismatch |
| resident clean source anchor | persistent project disk | immutable while child leases exist | exact commit + tree + source-index + cleanliness proof |
| Python dependency/tool environment | persistent project disk | project generation | exact Python/tool/dependency generation |
| package/dependency download cache | persistent project disk | evictable project cache | exact cache identity; corruption becomes a miss/reset |
| exact-parented derived indexes | persistent project disk when useful | rebuildable generation | semantic parent + producer/tool generation |
| task-private Git metadata | task area on persistent project disk | task generation | exact task/pool/source/private-Git proof |
| OverlayFS upper/work | task area on persistent project disk | task generation | exact task and mount lifecycle; cleanup after settlement |
| pytest cache | task-private upper | task generation | discarded with the task in phase one |
| Python bytecode cache | task-private upper | task generation | discarded with the task in phase one |
| OverlayFS merged mount | guest kernel state | task generation | transaction continuity only; mount presence is never ownership |

Phase one deliberately keeps pytest and bytecode caches task-private. Shared cross-task promotion can be tested later when measurements show enough value to justify another validity contract.

## Deliberately outside the project disk

Keep these outside the persistent project filesystem:

- project-disk lease, disk generation, attachment generation, and sandbox generation;
- physical disk/filesystem correlation authority;
- accepted source identity and fresh Git/source proof authority;
- verification/result authority;
- Glaeda execution/journal/reconciliation state;
- credentials, GitHub tokens, JIT material, provider capability state, and host secrets;
- performance receipts and the accepted experiment decision;
- canonical cold-reconstruction inputs.

The cold path must work after complete loss of the project disk.

## Frozen Quarry workload

Freeze one exact Quarry commit/tree before opening timings. Use the same commit/tree, Python generation, resource profile, and benchmark patch in both arms.

### 1. Task known -> first useful command

`task_known` is the accepted Glaeda task generation with exact project/source/trust/workload identity.

The first useful command is:

```bash
python -m quarry.agent_brief --format json
```

Record command completion and a SHA-256 digest of the normalized output. The digest must agree across comparable control/treatment samples.

### 2. Edit -> focused pytest

Apply one predeclared comment-only patch to:

```text
src/quarry/agent_brief.py
```

The edit changes source bytes while preserving program behavior. Record `edit_complete` after the write finishes.

Then run:

```bash
python -m pytest -q tests/test_agent_brief.py --disable-warnings
```

Record the terminal result and direct `edit_complete -> focused_pytest_result` duration. This delta is emitted beside the common #563 performance receipt instead of being inferred from logs.

### 3. Sequential reuse

Run ten fresh task generations against the same accepted project/source generation. Each task gets fresh private Git metadata and fresh OverlayFS upper/work state.

Report p50/p90 for:

- task known -> first useful command;
- edit -> focused pytest result;
- task settlement/cleanup where observed;
- task-private physical-byte growth.

### 4. Fan-out

Run fan-out widths:

```text
1
8
32
```

Measure whole-batch task-ready and first-command latency plus per-task observations. The 32-way setup experiment measures admission/materialization first; avoid turning it into a CPU oversubscription benchmark by running 32 focused pytest processes as the primary fan-out metric.

### 5. VM stop/start reuse

Run three clean stop/start cycles with the project disk retained.

For each cycle:

```text
exact lease/disk/sandbox expectations
-> stop/start or successor attachment under accepted authority
-> fresh physical correlation
-> fresh source/environment/task proof
-> first useful command
-> edit
-> focused pytest
```

A surviving disk or checkout never skips these proofs.

### 6. Forced fallback

Deliberately supply one incompatible expected hot-state parent/generation. Admission must decline resident reuse and complete the ordinary reconstruction path without adopting the incompatible surviving bytes.

### 7. Cold oracle

After the hot samples, independently reconstruct Quarry from canonical inputs and run the repository-owned exact-head verification path:

```bash
python scripts/run_local_tests.py exact-head --base origin/main
```

This is the final semantic oracle for the experiment. Hot-path success never replaces it.

## Measurements

Use the #563 receipt for shared observations and the Quarry dogfood sample envelope for experiment coordinates plus the edit boundary.

Primary agent-facing outcomes:

- task known -> first useful command;
- edit complete -> focused pytest result;
- sequential task p50/p90;
- whole-batch 1/8/32 setup and first-command latency;
- stopped VM -> first useful command after exact revalidation.

Storage observations stay distinct:

- logical project/task bytes;
- guest filesystem used/allocated bytes;
- task materialization delta;
- host backing logical bytes;
- host backing allocated bytes.

Take allocation samples after a quiescent/sync boundary. Establish an idle accounting-noise band before the first task batch. Run a second identical 32 create/delete cycle and report any persistent allocated-byte slope by named state class.

Preparation cost is reported separately from steady-state task latency. Compute the break-even task count from one-time hot preparation cost divided by observed per-task savings.

## Control integrity

Run the current trusted-Mac path as it genuinely exists on the experiment day. Record any ordinary cache/environment hits it receives. Artificially evicting control caches would inflate the treatment win and answer the wrong product question.

Both arms use the same:

- exact Quarry commit/tree;
- semantic task;
- benchmark patch;
- Mac host capability class;
- Lima backend;
- CPU/RAM resource profile;
- Python/toolchain generation;
- network policy during the measured task where the current path permits equivalence.

## Promotion bar

Phase one selects the ext4 hot-project treatment only when correctness/recovery checks all pass and the measured result clears these predeclared bars:

| Criterion | Bar |
| --- | --- |
| source/output equivalence | zero source-proof, agent-brief digest, or focused-test mismatches |
| singleton first command p50 | treatment <= 50% of control |
| singleton first command p90 | treatment <= 65% of control |
| edit -> focused pytest p50 | treatment <= 110% of control |
| 8/32 setup | zero admission/cleanup failures and whole-batch first-command wall >=2x quicker than control materialization |
| stop/start reuse | 3/3 exact revalidations and each resumed sample beats the cold-control median |
| disk growth | every residual class named; second 32-cycle batch has no unexplained positive slope outside the predeclared noise band |
| fallback | incompatible hot state cleanly reaches ordinary reconstruction |
| cold oracle | independent exact-head verification passes |
| amortization | report exact break-even task count; <=10 tasks is the strong phase-one target |

A result that shows most of the win comes from source/environment residency is enough to retain ext4 for Quarry. A remaining task-fork or physical-byte reservoir earns the bounded XFS/reflink follow-up under #560.

## Tool

The branch supplies:

```text
cargo run --bin quarry_hot_project_dogfood -- plan ...
cargo run --bin quarry_hot_project_dogfood -- sample ...
```

`plan` emits this frozen experiment as human or JSON output from exact Quarry/Glaeda identities.

`sample` accepts already-owned monotonic timings and bounded storage/resource observations, emits the common `HotExecutionPerformanceReceipt`, records the dogfood arm/sample coordinates, records the agent-brief digest when available, and computes the direct edit -> focused-pytest duration.

Neither command performs benchmark setup or privileged execution. Later runtime adapters own the physical phase boundaries and pass their observations into this tool.
