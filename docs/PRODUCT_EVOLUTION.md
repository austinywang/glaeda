# Product evolution and scope discipline

Glaeda began with a small practical irritation: use the operator's own Mac for expensive build and test work instead of waiting on hosted GitHub Actions runners.

The project has grown far beyond a shell wrapper around Lima. That growth is deliberate. Each major expansion answers a problem exposed by making the previous capability dependable enough to use.

The product center is now:

> **Glaeda makes useful compute blazingly hot. Disposable is a capability. Trust decides residency.**

Coding agents and GitHub Actions are major current proving workloads. They are not the outer product boundary. The broader job is to move declared work to useful accepted compute results with as little repeated setup as possible while preserving exact ownership, recovery, and reusable state.

See [`COMPUTE_RUNTIME.md`](COMPUTE_RUNTIME.md) for the generic workload boundary and [`BLAZINGLY_HOT.md`](BLAZINGLY_HOT.md) for the current hot-execution path.

## The dependency chain

The product direction can be read as one sequence.

1. **Use available compute.** An operator-owned machine already has useful CPU, memory, storage, and often accelerators. Routine work should be able to use it directly.
2. **Make execution reproducible.** Hand-maintained shell setup and pet VMs drift, break, and require operator memory. Execution environments need reviewed inputs, exact identities, bounded resources, typed status, and repeatable lifecycle actions.
3. **Build a strict disposable capability.** Hostile and unknown work needs fresh isolated state, bounded authority, exact cleanup, and proven absence.
4. **Make mutation crash-safe.** Once execution state is created, attached, started, stopped, or destroyed automatically, controller death can occur between any two side effects. Durable checkpoints, exact ownership, and no-replay recovery become requirements.
5. **Integrate the workload's normal control surface.** GitHub Actions is the first major integration, so Scale Set demand, one-time JIT registration, exact job binding, and runner removal join the durable lifecycle rather than replacing it.
6. **Treat work according to trust.** Hostile work gets the strict disposable contract. Trusted repeatable work may consume prepared or immutable reusable generations. Ultra-trusted work may retain mutable resident state when exact validity and reset rules permit it.
7. **Make the runtime unattended.** Queueing, sleep/wake, reboot, stale external state, failed provisioning, retries, teardown, recovery, and scale-to-zero should normally converge without operator archaeology.
8. **Make reusable state explicit.** Prepared environments, project state, datasets, dependency state, compiler output, indexes, model assets, renderer artifacts, services, and other expensive intermediates can stay ready according to trust, validity, and measured value.
9. **Choose the cheapest safe state mechanism per family.** Immutable source can be shared differently from write-heavy compiler output; a model asset differs from a mutable database; one primitive should not be forced onto all hot state.
10. **Optimize complete useful-result paths.** The universal metrics become request-to-first-useful-compute, request-to-first-useful-output, request-to-final-accepted-result, throughput, resource cost, and recovery time. Workload families add their own stronger metrics.
11. **Route toward useful heat.** A theoretically quicker cold machine can lose to an older or slower host that already contains the exact valid state needed by the work.
12. **Let more workload families consume the same execution kernel.** Repository verification remains first-class, while data transforms, research jobs, simulations, rendering, model workloads, services, and future compute families should not need to pretend they are CI jobs or coding agents.

The early milestones establish durable execution truth and the strict hostile-work lane. The hot-execution programme then uses that durable kernel to keep persistence aggressive where trust permits it and cheap to discard when validity changes.

## Trust-tiered residency

The trust model applies to compute workloads generally.

### Hostile / unknown

```text
fresh isolated execution state
-> one bounded workload
-> terminal output/evidence
-> teardown
-> prove absence
```

Reusable inputs are separately reviewed and immutable or read-only. Workload-specific writable state disappears with the execution environment.

The current primary proving example is arbitrary repository/CI code.

### Trusted repeatable compute

```text
work arrives
-> select prepared environment / warm pool
-> attach exact reusable inputs and eligible hot state
-> execute
-> retain, reset, or destroy according to policy
```

The individual execution can remain disposable while expensive preparation stays hot around it.

Current examples include trusted CI, repository seeds, dependency/compiler caches, prepared workers, and derived verification artifacts. The same class can serve immutable datasets, render inputs, model assets, prepared runtimes, or other repeatable workloads with their own exact validity contracts.

### Ultra-trusted resident compute

```text
resident compute context
-> revalidate exact owner / inputs / runtime / capabilities
-> perform useful work
-> retain valuable mutable state and selected services
-> reset, migrate, stop, or evict when policy changes
```

Current agent/project work is one strong example: a resident Linux project sandbox can preserve source-related state, compiler output, package state, indexes, and services across repeated edit/test/build loops.

Other workload families can retain different state without inheriting repository semantics.

## Destroyability enables aggressive residency

The core correctness property is **destroyability**.

Glaeda should be able to discard a VM, workspace, resident sandbox, cache, index, project disk, prepared materialization, compiler tree, dataset projection, model cache, or service and still know what happened, what remains owned, and how to decide the next safe action.

That property makes persistence powerful. Valuable trusted state can stay resident for hours or days because losing it costs latency and compute instead of execution truth.

Useful hot-state classes include:

- **immutable reusable generations** — prepared images, source/object pools, dependency environments, datasets, model assets, derived artifacts;
- **lease-scoped mutable resident state** — project checkouts, compiler trees, indexes, databases, resident services;
- **task-local mutable state** — edits, debugging state, temporary build/render/output state, task overlays;
- **shared mutable state** — only where an explicit publisher/consumer, poisoning, quota, and invalidation contract exists;
- **disposable physical caches** — package/compiler caches, indexes, downloaded immutable blobs, rebuildable projections.

Each family gets its own lifetime, trust, validation, resource, reset, and eviction rules.

## The current Linux project path

The development workload has already produced one useful concrete path-class lesson.

For clean mostly-read source fan-out, the current leading design is:

```text
persistent Linux project filesystem
-> immutable clean source anchor
-> immutable Git object-pool generation
-> task-private Git metadata
-> inherited reviewed source index
-> task-local OverlayFS upper/work
-> exact Git/source proof
-> ready execution view
```

For write-heavy compiler/build state, private CoW/reflink lineage can outperform placing the warmed tree behind an OverlayFS lower because copy-up becomes expensive.

So the product lesson is not “use OverlayFS everywhere” or “use XFS everywhere.” It is:

> **Use the cheapest safe sharing primitive for each state family, and choose from complete workload measurements.**

This is the kind of result that should guide future data/model/render/service state too.

## A test for scope growth

A proposed addition belongs on the main product path when it satisfies most of these conditions:

- it removes a demonstrated blocker to dependable compute execution;
- it closes a realistic crash, escape, persistence, secret, lateral-movement, resource-exhaustion, cleanup, or operator-burden failure mode;
- it reduces measured time to a useful accepted result or increases useful throughput;
- it improves resource economics or allows valuable state to remain ready safely;
- it makes exact reusable work available earlier through residency, precomputation, caching, or overlapping preparation;
- it generalizes admission, identity, trust, recovery, hot-state lifecycle, placement, or resource ownership without laundering workload-specific semantics into the core;
- it reuses mature components and keeps Glaeda focused on joining them rather than rebuilding their domains;
- it can be expressed as a bounded capability with explicit evidence and acceptance criteria.

Adjacent ideas remain useful when they can succeed independently of the current execution loop or introduce broad product surface before measurements justify it.

## Optimization hierarchy

For a representative trusted workload, prefer optimizations in this order unless measurements point elsewhere:

1. retain valuable trusted state;
2. remove repeated semantic work;
3. reuse exact completed work;
4. overlap independent preparation;
5. share immutable inputs;
6. parallelize genuinely independent work;
7. optimize hot kernels and storage behavior;
8. add more hardware or paid burst capacity.

This keeps Glaeda focused on deleting latency before buying around it.

## Product boundary

Glaeda should keep using mature components for:

- hypervisors and container runtimes;
- guest operating systems and filesystems;
- Git and source-control mechanics;
- compilers and package managers;
- data engines and model frameworks;
- renderers and media tools;
- workflow languages and hosted schedulers;
- networking enforcement and service supervision.

Glaeda should own the layer where those components need shared compute semantics:

```text
workload identity
trust and capabilities
admission and capacity
execution ownership
hot-state validity and lifecycle
backend/host placement
no-replay recovery
resource accounting
performance evidence
settlement / teardown
```

A better backend or domain engine should make Glaeda better without redefining the workload contract.

## The product center

The project should keep returning to the same promise:

> **Work should reach the hottest valid execution state that can produce a trustworthy useful result. Hostile work gets fresh isolation. Trusted work gets aggressive residency and reuse. Durable execution truth survives either choice.**

Today, the Mac is the main control plane and Apple-silicon compute source, Linux is the primary execution environment, GitHub is a major workflow integration, and coding agents are a major interactive workload. None of those current choices needs to become the permanent ceiling of the runtime.
