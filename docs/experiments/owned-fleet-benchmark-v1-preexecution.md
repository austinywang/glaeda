# Owned-fleet benchmark v1 — pre-execution decision report

Date: 2026-09-21
Scope: #743 / #546. This report records what can be concluded before v1 has run on any physical candidate.

## What does this machine do well?

The harness begins without assuming a physical machine.

It measures:

- cmux native Apple incremental and broad work where macOS/Xcode is available;
- Glaeda focused and broad Rust verification;
- Quarry focused and parallel verification;
- the reviewed Quarry rootless-Podman workload.

Every result is bound to physical machine comparison identity, execution backend, runtime identity, exact source, toolchain, heat/state, source/state storage placement, resource policy, and semantic validator.

Current v1 answer: **role calibration pending repeated physical useful-work receipts.**

## How many useful concurrent jobs can it sustain?

Unknown until one exact workload/backend/state campaign completes all three fixed-offer treatments:

```text
1 × large
2 × medium
4 × small
```

The absolute CPU/RAM envelope is selected from prior single-task evidence for that workload and machine. The aggregate CPU/RAM must stay identical across all three arms.

Every arm reuses the same offered work IDs and relative arrival offsets. The reducer keeps validated completions, p50/p90 final latency, unfinished work, declared/observed concurrency, pressure/swap, failures, fallbacks, resets, and temperature.

Current v1 answer: **unknown; isolated speed cannot fill this field.**

## What existing bottleneck would buying another one remove?

No purchase is justified by this pre-execution report.

A second node needs an observed reservoir such as:

- repeated unfinished fixed-window work;
- repeated queue delay before useful work begins;
- fallback/reset behavior under fixed offered work;
- sustained swap/pressure while useful work remains available;
- a platform-specific queue, such as native Apple build work;
- base-load Linux work displacing latency-sensitive work.

A raw p90 regression alone does not prove another machine fixes the cause.

Current v1 answer: **pending queue/pressure/fallback/unfinished-work evidence.**

## At what utilization does ownership beat observed hosted alternatives?

Unknown until the same semantic job has a dated hosted receipt.

The economics reducer requires:

```text
actual hosted wall time
hosted queue delay
dated billed rate
billing increment/minimum
exact workload/variant/source/operation/toolchain identity
hosted heat/state class
owned validated wall time
owned queue delay
measured machine idle/load power
purchase price/currency
dated FX when currencies differ
multiple useful-life/utilization assumptions
```

Owned and hosted heat state may differ. That difference is retained explicitly instead of being treated as semantic inequality.

Current v1 answer: **pending semantically equivalent hosted wall-time and measured power receipts.**

## What workloads should still stay hosted?

Keep these hosted until evidence says otherwise:

- burst demand beyond stable local contention capacity;
- jobs whose observed hosted queue + runtime reaches a validated result earlier;
- capability classes absent from the owned fleet;
- local workloads that produce unfinished work, swap/pressure, fallbacks, or resets;
- workload/utilization bands where observed hosted cost remains lower.

Current v1 answer: **hosted remains the default overflow/control arm until equivalent receipts narrow it.**

## Backend identity

The same physical machine can produce distinct execution classes.

Examples:

```text
native-macos
native-linux
reviewed-adapter-owned guest/container backend IDs
```

An Apple host running a Linux guest keeps one physical-machine receipt, while a reviewed VZ adapter produces the guest-side backend evidence and correlates it with that host receipt. The direct runner cannot create VZ evidence by changing a label.

## State classes

Every workload remains separate across:

```text
cold
dependency_warm
compiler_warm
project_resident
exact_reusable_compiled_product_present
```

Reports do not average them into one machine-speed number.

## Storage follow-up

Internal storage is sufficient for phase 1.

Later storage campaigns retain the same semantic workload while changing declared source and/or state placement:

```text
source tier -> exact checkout and source traversal
state tier  -> compiler/package/cache state and Quarry task-private worktrees
```

External NVMe and TB5 remain optional treatments.

## Controller/target checkout rule

Keep the benchmark controller checkout separate from the frozen target checkout. This is mandatory for the Glaeda workloads because their frozen source revision predates the harness itself.

```bash
export FLEET_HARNESS=/srv/src/glaeda-fleet-controller
export GLAEDA_TARGET=/srv/bench/glaeda-target
```

All `owned-fleet-benchmark` invocations below come from `$FLEET_HARNESS`; `--repo-root` points at the exact frozen target.

## Exact next commands on any node

Create and complete physical identity:

```bash
cp "$FLEET_HARNESS/benchmarks/fleet/machine-receipt.v1.template.json" /tmp/node.machine.json
$EDITOR /tmp/node.machine.json

"$FLEET_HARNESS/scripts/owned-fleet-benchmark" validate-machine --machine /tmp/node.machine.json --require-complete
```

Create explicit state evidence:

```bash
"$FLEET_HARNESS/scripts/owned-fleet-benchmark" state-template --state cold --output /tmp/state-cold.json
$EDITOR /tmp/state-cold.json
```

Inspect the frozen job:

```bash
"$FLEET_HARNESS/scripts/owned-fleet-benchmark" plan --workload glaeda-rust-focused-v1 --repo-root "$GLAEDA_TARGET" --state cold --state-dir /var/tmp/glaeda-fleet/glaeda-focused-cold
```

Execute one native-Linux sample:

```bash
"$FLEET_HARNESS/scripts/owned-fleet-benchmark" run --workload glaeda-rust-focused-v1 --repo-root "$GLAEDA_TARGET" --machine /tmp/node.machine.json --state-evidence /tmp/state-cold.json --state-dir /var/tmp/glaeda-fleet/glaeda-focused-cold --backend-id native-linux --resource-policy-id natural --source-storage-tier internal --source-storage-id machine-internal --state-storage-tier internal --state-storage-id machine-internal --output /tmp/receipts/glaeda-focused-cold-01.json
```

For Apple-host Linux guest execution, keep the physical Apple machine receipt and use the reviewed VZ adapter once that adapter is available. The native direct-run command above is intentionally insufficient for guest evidence.

Expected final stdout is compact JSON containing `"validated": true`.

The resulting receipt must have:

```text
document_type = glaeda-owned-fleet-benchmark-receipt
schema_version = 1
exact catalog source/toolchain/operation identity
machine comparison digest
explicit backend/runtime identity
state/storage identity
result.validated = true
```

Any source/toolchain/oracle disagreement makes the sample unusable for throughput/economics.

After A/A single-task blocks:

1. choose one workload-specific aggregate CPU/RAM envelope;
2. freeze one offered-work/arrival schedule;
3. run `large`, `medium`, and `small` under one reviewed resource policy, marking the campaign `enforced` only when every member binds the same reviewed enforcement evidence ID;
4. reduce all three fixed windows;
5. compare the full trio;
6. add one semantically equivalent hosted receipt per workload/backend condition of interest;
7. run the economics reducer;
8. generate the human report.

## Contention comparability fence

The three windows must agree on:

```text
machine comparison identity
backend + runtime
workload/variant
source commit/tree
semantic operation
toolchain
state class
source/state storage identity
offered work + arrival pattern
resource policy + enforcement status/evidence ID
aggregate CPU/RAM
window duration
```

Any drift is a refusal rather than a comparison row.
