# Owned-fleet useful-work benchmark v1

Status: candidate-agnostic benchmark/receipt contract. Physical execution is an operator action on any suitable node.
Owners: #743 and #546. Candidate-specific hardware issues consume this contract. Contention follows #760's fixed-offer, validated-completion discipline.

## Purpose

The harness turns hardware ideas into comparable useful-work evidence.

It never assigns acquisition authority to synthetic CPU scores or nominal core counts. A candidate is evaluated through exact repository work, exact semantic validators, explicit heat/state, observed contention, power, queue delay, and semantically equivalent hosted comparisons.

Canonical files:

- `benchmarks/fleet/workloads.v1.json` — frozen workload/toolchain/validator catalog.
- `benchmarks/fleet/machine-receipt.v1.template.json` — generic physical-machine receipt.
- `scripts/owned-fleet-benchmark` — plan/run/window/economics/report CLI.
- `scripts/test-owned-fleet-benchmark.py` — pure contract tests.

The repository contains no preferred machine model, generation, architecture, or purchase price.

## Canonical useful work

| ID | Semantic operation | Frozen source | Validator |
| --- | --- | --- | --- |
| `cmux-native-apple-incremental-v1` | mtime-only Swift touch -> focused native macOS compile/test | cmux `2319ef27…`, tree `08bcff46…` | xcodebuild success + exit 0 |
| `cmux-native-apple-broad-v1` | complete `cmux-unit` native suite | same | xcodebuild success + exit 0 |
| `glaeda-rust-focused-v1` | `scripts/verify focused` | Glaeda `0be1d302…`, tree `810238ad…` | nested exact-source Glaeda receipt |
| `glaeda-rust-broad-v1` | `scripts/verify full-tests` | same | nested exact-source Glaeda receipt |
| `quarry-verification-v1` | focused pytest or four-worker `parallel-full` | Quarry `468ccc44…`, tree `9316a33c…` | repository result / Quarry v2 receipt |
| `quarry-reviewed-container-v1` | reviewed rootless-Podman focused profile | same | reviewed profile exit/suite oracle |

cmux also records the exact Xcode/macOS SDK/Swift probes, Ghostty submodule revision, and pinned GhosttyKit archive checksum identity. Glaeda pins Rust 1.97.1. Quarry pins the Python 3.14 verifier closure.

Inspect the frozen catalog:

```bash
scripts/owned-fleet-benchmark catalog |
  jq '.workloads[] | {id,repository,commit,tree,toolchain,resource_envelope}'
```

## Controller checkout versus target checkout

Run the harness from a current Glaeda checkout that contains this benchmark code, and keep each workload target in its own exact-source checkout.

For Glaeda itself this means two checkouts, because the frozen workload commit predates the harness:

```bash
export FLEET_HARNESS=/srv/src/glaeda-fleet-controller
export GLAEDA_TARGET=/srv/bench/glaeda-target

git -C "$FLEET_HARNESS" rev-parse --show-toplevel
cd "$FLEET_HARNESS"
git -C "$GLAEDA_TARGET" fetch origin 0be1d302785df5876c81f5cd729540763aacf2c6
git -C "$GLAEDA_TARGET" checkout --detach 0be1d302785df5876c81f5cd729540763aacf2c6

"$FLEET_HARNESS/scripts/owned-fleet-benchmark" plan \
  --workload glaeda-rust-focused-v1 \
  --repo-root "$GLAEDA_TARGET" \
  --state project_resident \
  --state-dir /var/tmp/glaeda-fleet/glaeda-focused-project-resident
```

cmux and Quarry naturally use their own target checkouts. The controller checkout supplies only the versioned harness/catalog; semantic commands always execute inside the exact target checkout.

## Physical machine receipt

Create a receipt for whichever machine is actually available:

```bash
cp benchmarks/fleet/machine-receipt.v1.template.json /tmp/node.machine.json
$EDITOR /tmp/node.machine.json
scripts/owned-fleet-benchmark validate-machine --machine /tmp/node.machine.json --require-complete
```

Record:

```text
machine/config
purchase date + all-in price + currency
CPU topology
RAM
internal storage
host OS/version/kernel
applicable host toolchains
power policy
filesystem
network class
idle/load power when measured
sustained thermal/throttling observation when measured
```

Serial numbers, hostname, credentials, usernames, and unrelated host details stay out.

A machine may be `status=complete` while power or thermal measurement is `pending`; useful-work execution can proceed. The economics reducer later requires measured idle/load power.

Performance receipts bind two machine identities:

- the full machine-receipt digest, for provenance;
- a machine-comparison digest covering hardware/config/platform/power policy/filesystem/network class.

Purchase price and later power/thermal observations do not alter the comparison digest. CPU/RAM/storage/OS/kernel/toolchain/power-policy drift does.

## Execution backend identity

The physical machine and the execution backend are separate facts.

Every sample carries an explicit backend token. The direct runner can mint only the native backend that matches its current runtime:

```text
native-macos
native-linux
```

Guest, container, or remote execution classes such as `lima-vz` and `apple-container` require separately reviewed adapters that execute inside the claimed backend and bind that runtime back to the physical-machine receipt. A backend label alone carries zero authority.

The run receipt also captures a path-free runtime identity: OS class, kernel release, architecture, `/etc/os-release` digest where available, and source/state filesystem types.

## Five heat/state classes

Every workload keeps these classes separate:

```text
cold
dependency_warm
compiler_warm
project_resident
exact_reusable_compiled_product_present
```

Create explicit state evidence:

```bash
scripts/owned-fleet-benchmark state-template --state project_resident --output /tmp/state-project-resident.json
$EDITOR /tmp/state-project-resident.json
```

The boolean state contract is exact. A sample cannot silently call an exact reusable product merely “project resident”, or vice versa.

Use a benchmark-owned state directory outside the source checkout. Receipt/log files must also stay outside that state directory so measurement output cannot inflate storage growth.

Example preparations:

```bash
# Glaeda dependency warm
CARGO_HOME="$STATE/cargo-home" CARGO_TARGET_DIR="$STATE/target" cargo fetch --locked
rm -rf "$STATE/target"
mkdir -p "$STATE/target"

# Glaeda compiler/project warm
CARGO_HOME="$STATE/cargo-home" CARGO_TARGET_DIR="$STATE/target" cargo check --locked --all-targets --all-features

# cmux reviewed static prerequisite + dependency warm
./scripts/select-ci-xcode.sh
./scripts/download-prebuilt-ghosttykit.sh
xcodebuild -project cmux.xcodeproj -scheme cmux-unit -configuration Debug -clonedSourcePackagesDirPath "$STATE/SourcePackages" -resolvePackageDependencies
rm -rf "$STATE/DerivedData"
mkdir -p "$STATE/DerivedData"

# cmux compiler/project warm
./scripts/test-unit.sh build -derivedDataPath "$STATE/DerivedData" -clonedSourcePackagesDirPath "$STATE/SourcePackages"

# Quarry compiler/project warm
PYTHONPYCACHEPREFIX="$STATE/pycache" python -m compileall -q src
```

## Single-task receipt

Print the frozen semantic operation first:

```bash
scripts/owned-fleet-benchmark plan --workload glaeda-rust-focused-v1 --repo-root /srv/src/glaeda --state project_resident --state-dir /var/tmp/glaeda-fleet/glaeda-focused-project-resident
```

Run a native-Linux sample:

```bash
scripts/owned-fleet-benchmark run --workload glaeda-rust-focused-v1 --repo-root /srv/src/glaeda --machine /tmp/node.machine.json --state-evidence /tmp/state-project-resident.json --state-dir /var/tmp/glaeda-fleet/glaeda-focused-project-resident --backend-id native-linux --resource-policy-id natural --source-storage-tier internal --source-storage-id machine-internal --state-storage-tier internal --state-storage-id machine-internal --output /tmp/receipts/glaeda-focused-project-resident-01.json
```

The direct runner deliberately refuses a `lima-vz` label. Apple-host Linux guest measurements use the same generic workload/receipt schema through a reviewed VZ adapter that proves guest execution and correlates it with the physical Apple machine receipt.

A successful receipt ends with compact stdout containing `"validated": true`.

The receipt records:

- request-known -> command start;
- command start -> first useful result;
- request-known -> final semantic result;
- child CPU time/utilization;
- sampled member-process RSS;
- host swap start/max/end;
- Linux memory PSI when available;
- state growth;
- sampled temperature;
- direct-run queue delay fixed to zero at the runner-owned request-known boundary;
- fallback/reset counts fixed to zero because the direct runner does not own an upstream scheduler/controller lifecycle;
- source and state storage tiers/filesystems;
- execution Wh when load power is available.

Repository semantic receipts begin absent on every sample. A stale semantic receipt from an earlier success cannot validate a later run.

Source commit/tree and cleanliness probes use absolute `/usr/bin/git` with a
closed C-locale environment, global/system Git config disabled, and
`--ignore-submodules=none` for cleanliness. Reviewed catalog commands execute
through absolute `/bin/bash` with the benchmark's explicit environment rather
than the caller's ambient process environment.

## Contention: 1 large / 2 medium / 4 small

The catalog defines only the concurrency shape:

```text
large = 1 admitted job
medium  = 2 admitted jobs
small = 4 admitted jobs
```

Absolute CPU/RAM is chosen per workload experiment after single-task measurement. The aggregate envelope must remain identical across all three arms.

Example experiment variables:

```bash
export AGG_CPU_MILLIS=8000
export AGG_MEMORY_BYTES=$((16 * 1024 * 1024 * 1024))
export RESOURCE_POLICY_ID=cgroup-v2-fleet-v1
export RESOURCE_POLICY_EVIDENCE_ID=reviewed-cgroup-v2-fleet-v1
export ARRIVAL_PATTERN_ID=simultaneous-burst-v1
```

Those numbers are illustrative. A different workload/machine may earn a different envelope; changing it creates a different experiment.

For each profile, every member receipt must declare the per-job division:

```text
large:  cpu = aggregate/1, memory = aggregate/1
medium: cpu = aggregate/2, memory = aggregate/2
small:  cpu = aggregate/4, memory = aggregate/4
```

The direct benchmark runner records resource limits as declarations only. It deliberately refuses `--resource-policy-status enforced`, because it does not itself install or verify cgroup, scheduler, or other resource controls. Direct-run contention windows are therefore diagnostic.

Example direct member run for the medium arm:

```bash
scripts/owned-fleet-benchmark run --workload glaeda-rust-focused-v1 --repo-root /srv/src/glaeda --machine /tmp/node.machine.json --state-evidence /tmp/state-project-resident.json --state-dir /var/tmp/glaeda-fleet/glaeda-focused-project-resident --backend-id native-linux --resource-policy-id "$RESOURCE_POLICY_ID" --cpu-millis $((AGG_CPU_MILLIS / 2)) --memory-limit-bytes $((AGG_MEMORY_BYTES / 2)) --source-storage-tier internal --source-storage-id machine-internal --state-storage-tier internal --state-storage-id machine-internal --output /tmp/windows/medium/job-01.json
```

Capacity-grade `enforced` receipts must come from a separately reviewed enforcement adapter that actually applies and observes the declared CPU/RAM policy, then feeds those receipts into the same window reducer. An opaque evidence ID alone cannot upgrade a direct-run receipt.

A reduced-window manifest binds the fixed window, arrival schedule, and aggregate resource envelope:

```json
{
  "schema_version": 1,
  "document_type": "glaeda-owned-fleet-window-manifest",
  "experiment_id": "node-a-glaeda-focused-r1",
  "machine_id": "node-a",
  "workload_id": "glaeda-rust-focused-v1",
  "variant": null,
  "state_class": "project_resident",
  "profile_id": "medium",
  "window_start_monotonic_ns": 123456789000000,
  "window_elapsed_seconds": 300,
  "arrival_pattern_id": "simultaneous-burst-v1",
  "arrival_tolerance_ms": 25,
  "resource_policy_id": "cgroup-v2-fleet-v1",
  "resource_policy_status": "enforced",
  "resource_policy_evidence_id": "reviewed-cgroup-v2-fleet-v1",
  "aggregate_cpu_millis": 8000,
  "aggregate_memory_limit_bytes": 17179869184,
  "offered_work": [
 {"work_id":"job-01","arrival_offset_ms":0,"receipt":"job-01.json"},
 {"work_id":"job-02","arrival_offset_ms":0,"receipt":"job-02.json"},
 {"work_id":"job-03","arrival_offset_ms":5000,"receipt":null},
 {"work_id":"job-04","arrival_offset_ms":5000,"receipt":null}
  ]
}
```

`window_start_monotonic_ns` and every member receipt must come from the same host boot/monotonic clock domain. A receipt finishing after the fixed deadline is represented as unfinished for that window.

Reduce all three:

```bash
scripts/owned-fleet-benchmark reduce-window --manifest /tmp/windows/large.manifest.json --output /tmp/windows/large.window.json

scripts/owned-fleet-benchmark reduce-window --manifest /tmp/windows/medium.manifest.json --output /tmp/windows/medium.window.json

scripts/owned-fleet-benchmark reduce-window --manifest /tmp/windows/small.manifest.json --output /tmp/windows/small.window.json

scripts/owned-fleet-benchmark compare-windows --window /tmp/windows/large.window.json --window /tmp/windows/medium.window.json --window /tmp/windows/small.window.json --output /tmp/windows/contention-comparison.json
```

`compare-windows` requires exactly one large, medium, and small arm. It also requires identical machine comparison identity, backend/runtime, workload/source/toolchain/state/storage identity, offered arrival pattern, resource policy, aggregate CPU/RAM, and window duration.

A fixed-offer manifest with zero settled member receipts is retained as
`glaeda-owned-fleet-window-partial-receipt`: it records the declared
offered/unfinished work, frozen manifest/arrival/resource-policy request, zero
observed concurrency, and absent final-result percentiles. Its authority is
`declared_offer_only` / `manifest_only`. It stays visible for experiment
diagnosis, while `compare-windows`, fleet-capacity classification, bottleneck
claims, and #546 routing evidence refuse it because no member receipt established
that the offer reached the declared machine/backend/toolchain.

The reducer derives:

- validated completions/window;
- nearest-rank p50/p90 final-result latency over validated members;
- unfinished work;
- semantic-mismatch, source/currentness-unvalidated, process/timeout-failure,
  fallback, and reset counts as separate terminal evidence classes;
- declared vs observed concurrency;
- max member peak RSS;
- host swap/pressure/temperature maxima, plus swap growth relative to each member's start observation so preexisting swapped pages are not attributed to the benchmark.

Observed concurrency above the declared 1/2/4 profile is a refusal. Under-filled declared concurrency remains visible as a collapse flag.

## Owned versus hosted economics

Create a hosted comparator:

```bash
scripts/owned-fleet-benchmark hosted-template --output /tmp/hosted.json
$EDITOR /tmp/hosted.json
```

Retain:

```text
backend
measurement date
measurement-evidence SHA-256 binding the hosted wall/queue observation
actual hosted wall time
queue delay
rate/currency/billing increment/minimum
rate source
workload/variant/source/operation/toolchain identity
hosted state class
dated FX when currencies differ
semantic validation
```

The hosted measurement evidence digest and rate source are required. A manually
entered queue delay or rate with no provenance is rejected by the economics
reducer. The reduced economics receipt preserves both
`hosted.measurement_evidence_sha256` and `hosted.rate_source` so downstream
routing/accounting consumers can inspect the evidence behind the hosted queue
and price inputs.

The hosted state class may differ from owned state. A persistent owned `project_resident` sample can legitimately be compared with a hosted `cold` sample when the semantic job/toolchain/source are exact. The economics receipt records both conditions instead of pretending they are the same heat state.

Economics requires measured machine idle/load power:

```bash
scripts/owned-fleet-benchmark economics --owned /tmp/receipts/glaeda-focused-project-resident-01.json --owned-reference /tmp/receipts/glaeda-focused-cold-01.json --hosted /tmp/hosted.json --machine /tmp/node.machine.json --electricity-price-per-kwh 0.20 --life-months 24,36,48 --utilizations 0.10,0.25,0.50,0.75 --observed-utilization 0.35 --output /tmp/economics/glaeda-focused.json
```

It reports:

- validated completions / purchase currency;
- validated completions / Wh;
- amortized + power cost per validated completion, with observed utilization tagged separately when supplied;
- utilization crossover for each useful-life assumption;
- queue-to-result savings;
- owned and hosted heat/state context;
- optional measured hot-state delta against an exact-comparable owned reference (typically cold).

Idle power is charged across powered-but-inactive hours. `--observed-utilization` should come from measured fleet history; omit it until such history exists. Nominal local-core/hosted-vCPU arithmetic is absent.

## Storage comparison

TB5 or external storage is never required for phase 1.

The receipt tracks source storage and state storage separately. Tier says what class of placement is under test; the opaque storage ID keeps two distinct devices/enclosures/configurations from collapsing into one comparison bucket:

```text
--source-storage-tier internal|external_nvme|network|other
--source-storage-id   <opaque storage configuration ID>
--state-storage-tier  internal|external_nvme|network|other
--state-storage-id    <opaque storage configuration ID>
```

To compare internal vs external NVMe later:

1. place the exact pinned checkout on the declared source tier when measuring source traversal;
2. place `state_dir` on the declared state tier for compiler/package/cache state;
3. keep Quarry `TMPDIR` under `state_dir`, so its task-private worktrees/materialization follow the state tier;
4. keep machine/backend/source/toolchain/state/resource policy/validator exact;
5. rerun the real workloads and contention windows.

This covers real source traversal, Git worktree/materialization, Xcode/Swift or Cargo state, reviewed immutable artifacts, concurrent I/O, and sustained broader verification. Storage microbenchmarks may diagnose a result; they carry zero acquisition authority by themselves.

## Human report

```bash
scripts/owned-fleet-benchmark report --machine /tmp/node.machine.json --receipts /tmp/receipts --windows /tmp/windows --economics /tmp/economics --output /tmp/node-report.md
```

The report refuses evidence from another machine comparison identity.

It answers:

- What does this machine do well?
- How many useful concurrent jobs can it sustain?
- What existing bottleneck would buying another one remove?
- At what utilization does ownership beat observed hosted alternatives?
- What workloads should still stay hosted?

Fleet-planning roles require repeated validated samples plus a complete stable large/medium/small contention set. A single successful command does not classify a node.

A stability gate is satisfied only by facts that were actually observed. Swap growth is reported as `null` when the host swap probe failed or the platform is unsupported, and that unmeasured value does not count as an observed zero; a window that reports no `failure_count` at all does not count as a window without failures. Such a set is not classified stable, and the report names each unproven window and the fact it never measured. A corrupt or non-object evidence document refuses the report naming the exact path, instead of being skipped into a report that silently omits it.

These are acquisition/redeployment labels only. CMUX execution-role eligibility and routing authority remain with the CMUX fleet enrollment/acceptance contract on current main.

Recognized evidence-driven planning roles include:

```text
latency-sensitive Apple build node
native-Linux base-load node
background/replay node
cache/artifact node candidate
burst-only hosted pool retained where observed hosted completion/cost wins
```

Silicon generation itself has no priority field. Hot state, backend, queue delay, and predicted finish time remain first-class.

## Harness tests

```bash
python3 -m py_compile scripts/owned_fleet_benchmark/*.py scripts/owned-fleet-benchmark scripts/test-owned-fleet-benchmark.py

python3 scripts/test-owned-fleet-benchmark.py
```

The suite covers generic catalog closure, partial-machine execution refusal, machine comparison identity, exact state contracts, validated-only window reduction, over-concurrency refusal, mixed-machine refusal, complete 1/2/4 comparison requirements, fixed-window comparability, cold-hosted/hot-owned economics, machine/economics identity fences, mixed-report evidence, conservative role classification, refusal of unmeasured swap and missing failure counts as stability evidence, and refusal of corrupt evidence documents.
