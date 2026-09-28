# Roadmap

Glaeda's product goal is **blazingly hot, trust-tiered execution for compute workloads**.

The core rule is:

> **Disposable is a capability, not a mandate. Trust decides residency.**

The runtime should do every reusable, trustworthy piece of work before the workload needs it. Hostile work receives fresh isolated execution state and exact teardown. Trusted repeatable work can consume prepared and immutable reusable generations. Ultra-trusted work can keep valuable mutable state resident when exact validity, ownership, and reset rules permit it.

Coding agents and GitHub Actions are major current proving workloads. They are not the outer product boundary. See [`COMPUTE_RUNTIME.md`](COMPUTE_RUNTIME.md) and [#770](https://github.com/teamleaderleo/smolrunner/issues/770) for the general workload model.

The shared correctness requirement is durable execution truth: destroying physical state must preserve enough exact evidence to recover safely, and retaining physical state must never make correctness depend on that state surviving.

## Product goals

The operator experience should converge on four properties:

- **blazingly hot** — useful computation begins with as little repeated setup as possible;
- **trust-tiered** — worker/context lifetime, writable-state reuse, credentials, networking, and publication follow explicit policy;
- **recoverable** — VMs, sandboxes, project disks, caches, indexes, compiler trees, datasets, model state, and services can disappear without losing the ability to decide the next safe action;
- **quiet** — queueing, capacity, crashes, stale external state, retries, cleanup, revalidation, migration, and eviction normally converge without operator archaeology.

Universal performance targets include:

```text
request -> first useful compute
request -> first useful output
request -> final accepted result
workload completion wall time
throughput under concurrency
resource occupancy / cost
recovery time after interruption
```

Workload families add stronger metrics. Current agent/dev workloads care about edit-to-test, edit-to-build, verification completion, task fan-out, and fleet throughput.

## Product boundary

Glaeda owns the compute-side layer shared across workload families:

- workload and attempt identity;
- trust/capability admission;
- capacity ownership and contention;
- backend/host eligibility and placement;
- hot-state validity, reuse, reset, and retention;
- lifecycle, quiescence, supersession, and settlement;
- durable no-replay recovery;
- bounded performance evidence;
- physical-state migration, quarantine, destruction, and reacquisition policy.

Workload adapters own domain semantics. Repository verification can understand Git, commands, tests, and verification evidence. Data, rendering, model, research, and service adapters can own their own typed inputs and result contracts.

GitHub Actions remains the ordinary workflow scheduler, check/status surface, and hosted job-log surface for GitHub jobs. It is an integration, not the runtime's semantic root.

The first production host/backend remains operator-owned Apple silicon running Linux through Lima/VZ. macOS is the current trusted control plane; Linux carries the filesystem-heavy execution path. Other local/native/fleet backends compete through capability and complete-loop evidence rather than product-specific rewrites.

## Trust classes

### Hostile / unknown

```text
fresh isolated execution state
-> one bounded workload
-> terminal output/evidence
-> exact teardown
-> prove absence
```

No inherited mutable working state. Reusable inputs are separately reviewed immutable/read-only generations where policy permits them.

### Trusted repeatable compute

```text
work arrives
-> prepared environment / warm pool
-> exact reusable inputs and eligible state
-> execute
-> destroy, reset, or retain according to policy
```

The individual execution can remain disposable while expensive preparation stays hot around it.

### Ultra-trusted resident compute

```text
resident compute context
-> revalidate exact owner / inputs / runtime / capabilities
-> perform useful work
-> retain valuable mutable state and selected services
-> reset, migrate, stop, or evict when policy changes
```

Current project/agent residency is one important realization, not the only future workload family.

## Durable execution kernel

Persist only the facts a restarted controller genuinely needs and cannot safely infer again, including where applicable:

- workload / request / attempt / execution identity;
- capacity and ownership generations;
- exact external mutation intent and no-replay checkpoints;
- immutable runtime/toolchain/input generations;
- workload-specific delivery/acquisition identities where integrations require them;
- VM/sandbox/project-disk/resident-context bindings required for reconciliation;
- terminal outcomes and teardown/settlement receipts;
- causation/idempotency identities;
- explicit recovery debt.

The durable kernel stays bounded and non-executable. Rich physical state remains family-owned working state.

## Useful foundation already built

Current main already contains substantial reusable foundations:

- [x] Rust CLI with shared typed human/JSON reports;
- [x] canonical configuration, host observations, ownership classifications, and bounded public errors;
- [x] shell-free process execution with explicit environments;
- [x] crash-safe durable stores, revisions, journals, and recovery classifications;
- [x] host capacity observation and durable admission/reservation state;
- [x] prepared Lima/VZ worker inputs and physical prepared-template acceptance;
- [x] official GitHub Runner Scale Set bridge, Keychain credential acquisition, durable delivery recovery, and JIT composition;
- [x] LaunchAgent plan/apply/status and supervision path;
- [x] durable worker/attempt recovery and controller-death evidence;
- [x] trusted resident sandbox and project-disk identity/lifecycle foundations;
- [x] bounded hot-execution performance receipt vocabulary;
- [x] hot-state path classification/admission foundations;
- [x] source-anchor/task-view lifecycle core;
- [x] trusted OverlayFS mount planning and descriptor leases;
- [x] safe-Rust all-FD OverlayFS mount transaction, sealed behind physical correlation authority;
- [x] immutable Git object-pool generation/lease/marker/observation foundations;
- [x] Git index-v2 stat-cache handling and inherited-index research;
- [x] one-shot guest-control protocol and increasingly exact Mac-to-Linux transport/authority boundary;
- [x] generic compute workload identity seam on current main (#773).

Detailed current M6 state belongs in [`M6_HOT_RUNTIME_HANDOFF.md`](M6_HOT_RUNTIME_HANDOFF.md).

## Current sequencing

The strict disposable lifecycle remains an important production prerequisite for hostile work. Hot trusted execution progresses in parallel where it does not weaken that lane.

Current priorities are:

1. complete the installed-service one-job disposable path and converge to exact runner/VM absence plus released capacity;
2. close remaining restart-safe ownership/recovery gaps around external Lima and guest mutations;
3. prove and implement the hostile-worker credential/network boundary;
4. finish the genuine persistent project-disk format/attach/mount/correlation chain on the operator Mac;
5. compose the landed source-anchor/Git-pool/OverlayFS primitives into one end-to-end resident task path;
6. measure complete cold/prepared/resident useful-result loops under concurrency;
7. continue widening generic workload admission/capacity/hot-state seams without weakening current repository semantics;
8. feed repeated measurements into explainable heat-aware placement, verification reuse, and diagnostics.

## Milestone 1 — durable disposable-attempt reconciliation

The durable reconciler establishes the correctness kernel for fresh one-job workers.

**Outcome:** restart at any durable lifecycle boundary preserves exact ownership and prevents duplicate worker authority or premature capacity release.

Current status: core attempt/reservation/recovery machinery is substantially landed.

## Milestone 2 — prepared disposable Linux worker

Build a worker factory that makes fresh hostile-writable state cheap and exact.

Key properties:

- pinned runtime/guest/runner/provisioning inputs;
- exact resource envelope;
- no host filesystem or credential inheritance;
- bounded lifecycle mutations and post-command observation;
- restart-safe clone/start/delete ownership;
- full teardown/absence proof.

Current status: prepared-template lifecycle is physically proven; remaining work centers on broader crash/recovery coverage and production composition.

## Milestone 3 — GitHub-native one-job execution

Make ordinary GitHub demand the normal strict-disposable control path.

Key path:

```text
GitHub demand
-> durable admission/reservation
-> one fresh Linux worker
-> one exact JIT runner
-> one job
-> terminal result
-> runner removal
-> VM teardown
-> capacity release
```

Current status: most controller/JIT/bridge pieces are landed; the supported installed-service physical journey remains a key acceptance target.

## Milestone 4 — hostile-work credential and network boundary

Make arbitrary repository code an intended hostile workload.

Required outcome:

- durable controller credentials stay outside the worker;
- worker cannot reach the Mac control plane, unrelated LAN/private destinations, or peer workers except where policy explicitly grants it;
- nested/container behavior remains least-privilege;
- exact teardown survives a compromised guest;
- hostile writable state never becomes reusable merely by surviving.

## Milestone 5 — supervised unattended recovery

Make execution disappear from routine operator attention.

Required outcome:

- controller/service restart;
- host sleep/wake and reboot;
- backend/GitHub outage;
- failed provisioning;
- lost runner/stuck work;
- ambiguous mutation response;
- failed teardown;
- stale registration;

all converge through durable identity + fresh observation + bounded recovery debt.

## Milestone 6 — blazingly hot execution

M6 is a primary product programme. The universal target is **time to useful accepted result**, with current agent/dev workloads supplying especially demanding latency and fan-out benchmarks.

### M6A — resident execution environments

Compare the same semantic workload across:

```text
cold disposable
prepared disposable
warm-pool disposable
resident after idle
resident immediate reuse
resident repeated-work loop
```

Measure startup/resume, first useful output, final result, resource residency, reset/rebuild cost, and concurrency.

Lima/VZ is the current primary backend. Apple `container` / Containerization and native/fleet backends enter through controlled complete-loop comparisons.

### M6B — persistent Linux project filesystem

For the current repository-development workload, keep filesystem-heavy work on a native Linux filesystem inside the guest rather than putting a macOS shared checkout on the hot path.

Current physical candidates remain bounded:

```text
resident root/ext4
dedicated persistent ext4 project disk
dedicated persistent XFS/reflink project disk
```

Measure internal/external backing storage, host backing-file growth, recovery, and complete workload latency rather than filesystem microbenchmarks alone.

#560/#565 own the storage/project-disk programme.

### M6C — immutable source fan-out through OverlayFS

OverlayFS has graduated from a research comparator to the **leading current clean source-view path**.

Current composition:

```text
exact immutable source anchor
+ immutable Git object-pool generation
+ task-private Git metadata
+ inherited reviewed clean source index
+ task-private upper/work state
-> exact OverlayFS mount
-> hardened non-mutating Git/source proof
-> ready task view
```

Representative hosted-ARM measurements in #570/#580 reached roughly:

```text
tasks    ordinary materialization    OverlayFS + inherited index
1        ~76.2 ms                    ~15.9 ms
8        ~224.3 ms                   ~52.4 ms
32       ~695.6 ms                   ~175.2 ms
```

with ready-state physical growth collapsing from MiB-scale copies to KiB-scale task state in that comparison. The same source-view concept also works on ext4.

The landed mount executor uses Linux's new mount API through held descriptors and safe Rust. Production mutation stays sealed until the project-disk/filesystem/resident-sandbox correlation path proves the exact current filesystem authority.

#570/#580/#589/#640/#707/#708 own this chain.

### M6D — write-heavy private state

OverlayFS is not the default for every path class.

Write-heavy compiler/build output can suffer expensive copy-up. Current evidence favors task-private CoW/reflink lineage where supported for those state families.

One recorded synthetic eight-task Rust comparison was roughly:

```text
warmed build state behind OverlayFS lower    ~5.70 s
private reflink/CoW build-state lineage      ~2.77 s
```

Keep mechanism selection path-class-specific:

- immutable/mostly-read -> overlay/share;
- write-heavy -> private CoW/reflink where useful;
- no reusable parent -> private empty;
- shared mutable -> explicit producer/consumer contract only.

#573 owns the selection model.

### M6E — dependency, index, service, and non-repository heat

Treat expensive state families explicitly rather than calling all of them cache:

- package-manager state;
- compiler caches/build trees;
- language/code-search indexes;
- databases/fixtures;
- datasets and derived representations;
- model assets/preprocessed shards;
- renderer caches/compiled assets;
- long-lived trusted services.

Each family needs exact validity parents, resource accounting, reset/revalidation, and a cold reacquisition path when rebuildable.

### M6F — complete-loop proof

For every serious optimization record:

```text
workload identity
baseline / candidate identity
exact validity inputs
request -> first useful output
request -> final accepted result
p50 / p90 under relevant concurrency
CPU/RAM/storage effects
reset/invalidation behavior
fallback path
```

Current proving workloads include Quarry and Glaeda itself. The generic workload seam should also carry at least one non-repository compute example through the same admission/capacity/lifecycle kernel.

**M6 acceptance:** repeated trusted work begins near the cost of the useful computation itself, concurrency remains efficient, hot physical state is replaceable, and the runtime can explain what was reused, reset, rebuilt, or routed elsewhere.

## Milestone 7 — adaptive execution runtime

Turn repeated measurements into better execution decisions without allowing observations to become authority.

Near-term directions:

- use bounded comparable observations to estimate completion cost;
- choose host/backend/resource profile from current capacity and valid heat;
- prefer a hot older/slower host over a cold nominally quicker host when predicted completion wins;
- compile/reuse domain work such as verification plans/results only under exact family-owned contracts;
- classify recurring failures and promote proven causes into preflight/diagnostic policy;
- explain every placement/reuse/reset decision through stable human/JSON reports;
- extend the same placement/admission kernel beyond repository workloads.

**M7 acceptance:** Glaeda gets quicker and easier to operate as it observes repeated workload families while every optimization remains inspectable, reversible, and bounded by trust.

## Hotness hierarchy

Optimize in this order unless measurements say otherwise:

1. keep valuable trusted state resident;
2. remove avoidable semantic work;
3. reuse exact completed work;
4. overlap independent preparation;
5. share immutable inputs;
6. parallelize genuinely independent work;
7. optimize hot kernels/storage/data movement;
8. buy or rent additional compute.

A huge machine rebuilding the useful world every iteration is still cold.

## Suspected-compromise recovery target

For hostile or compromised execution:

```text
mark affected attempt/generation suspect
-> stop admitting derived reusable writes
-> destroy or quarantine the execution environment
-> remove external registration where exact authority exists
-> revoke/expire narrow affected capability
-> discard affected reusable generation when policy requires it
-> continue from canonical durable truth + reviewed inputs
```

For trusted resident workloads, ambiguity in ownership/input/runtime/capability validity triggers revalidation, reset, quarantine, migration, or destruction. Residency never gains authority merely by surviving.

## Deferred / non-goals

- public multi-tenancy before there is a concrete product reason;
- a replacement workflow language or generic CI scheduler;
- cross-workload writable-state sharing without explicit trust and validity contracts;
- autonomous semantic authority inferred from model/diagnostic output;
- custom hypervisors, container runtimes, package managers, filesystems, data engines, model frameworks, or cache servers before mature components fail a measured requirement;
- deployment/publication authority inferred from verification or execution success;
- forcing every future workload through repository/GitHub/agent vocabulary.
