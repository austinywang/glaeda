# Glaeda

**Blazingly hot compute: start from the hottest state you can prove is valid.**

Glaeda is a trust-tiered execution runtime that minimizes repeated setup on the path from declared work to a useful accepted result. It treats source trees, Git objects, dependency state, compiler outputs, indexes, prepared runtimes, resident services, and other expensive intermediates as typed reusable state whose identity, validity, ownership, and trust boundary must be proven for the workload that wants to consume them.

**Warm means state survived. Hot means Glaeda can prove this workload may reuse it.**

Coding agents and GitHub Actions are major current proving workloads, while the runtime boundary is broader: CI, batch compute, data work, rendering, model workloads, and future typed adapters can use the same execution core.

> **Disposable is a capability. Trust decides residency.**

```text
work becomes known
-> identify exact workload, trust, inputs, and capabilities
-> select eligible compute and the hottest valid reusable state
-> admit capacity
-> materialize only what must change
-> execute
-> return bounded outputs and evidence
-> retain, reset, quarantine, migrate, or destroy physical state
-> recover from durable truth after interruption
```

Hot state is acceleration and working state, never independent authority. A surviving VM, cache, worktree, index, service, or build tree does not get to decide what source it belongs to, whether it is current, who may mutate it, or whether a derived result is acceptable. Ambiguous state is revalidated, reset, quarantined, rebuilt, or rejected.

See [`docs/COMPUTE_RUNTIME.md`](docs/COMPUTE_RUNTIME.md) for the product model and [`docs/BLAZINGLY_HOT.md`](docs/BLAZINGLY_HOT.md) for the hot-execution programme.

## What "blazingly hot" means

Glaeda does not use one universal cache, worker lifetime, or filesystem treatment. It tries to remove the largest repeated cost while preserving the semantics of the state being reused.

Current work follows a few rules:

- **Keep valuable trusted state resident when that wins the complete loop.** Destroyability is a recovery property, not a requirement to throw useful state away after every task.
- **Choose reuse by state family.** Mostly-read source can benefit from immutable resident generations and task-private views; write-heavy build state often wants private copy-on-write lineage; long-lived services need their own scoped lifecycle; hostile work may still be hottest when it starts fresh and isolated.
- **Move small requests toward large state.** A repository, build tree, index, dataset, or model can stay resident while an exact bounded question travels to it instead of repeatedly transporting and reconstructing the state around the request.
- **Prefer native mechanisms.** Linux scheduling, page cache, cgroups, filesystems, mounts, process primitives, Git, and service managers remain the baseline. Glaeda adds semantic identity, admission, validity, recovery, and evidence only where those primitives do not provide them.
- **Keep the cold path complete.** Every promoted hot-state family needs a reset or reconstruction route from canonical inputs. Losing acceleration state may cost time and compute; it must not destroy execution truth.

The optimization order is intentionally boring: avoid destroying valuable valid state; delete avoidable semantic work; reuse exact work; overlap independent preparation; share immutable inputs; parallelize only what is actually independent; optimize hot kernels; buy faster hardware last.

## Current status

Glaeda is pre-alpha and already exercises real systems paths on operator-owned Apple silicon and Linux/x86-64 machines. Current dogfood includes resident repository-evidence execution on Linux; current `main` also contains the durable disposable-worker/control foundations, GitHub Runner Scale Set integration, prepared Lima/VZ worker support, trusted resident-state primitives, hot-state admission, task/source-view work, and bounded verification/performance receipts.

The current production direction still has active gaps around complete hostile-work network/credential hardening and end-to-end composition of the trusted resident task path. Current issues, pull requests, Git history, and [`docs/ROADMAP.md`](docs/ROADMAP.md) own progression; this README describes the present product boundary.

## Measured results

Glaeda keeps performance claims tied to exact workloads and controls rather than treating one benchmark as a universal speed claim.

### Resident repository evidence: about 114× faster

A controlled Big Red dogfood run asked one real review-evidence question against an exact Glaeda candidate. The GitHub baseline needed five outer calls and a 4,444 ms median; the landed resident `repo-query/v1` path answered the registered next-action question in 39.008 ms internal / about 40 ms through the wrapper with one outer call.

| Arm | Median | Outer calls | Worker-visible/result bytes |
| --- | ---: | ---: | ---: |
| GitHub baseline | 4,444 ms | 5 | 75,713 after optimistic projection; 227,827 transported |
| Glaeda resident | 39.008 ms internal; ~40 ms wrapper | 1 | 21,452 |

The landed path was **about 114× faster**, removed four remote calls, and produced a **71.7% smaller** worker-visible result than the optimistic GitHub projection. Internally it used 28 bounded local Git processes while keeping their intermediate output out of model context.

A separate control kept the attribution honest: perfectly composed direct local Git took a 10.51 ms median, while a discarded narrow Glaeda wrapper took 16.58 ms. The product win is therefore the resident, bounded, identity-bearing operation replacing repeated remote reads and procedural rediscovery — not a claim that wrapping Git makes Git itself faster. See [`docs/experiments/resident-repo-query-big-red-2026-08-31.md`](docs/experiments/resident-repo-query-big-red-2026-08-31.md).

### Resident developer loop: 3.95× versus fresh local

On the frozen Big Red Rust edit-to-verification workload, fresh local execution took 43.67 s at the median while the Glaeda resident path took 11.06 s: **3.95× faster than fresh local**.

The ordinary warm-worktree control took 10.31 s and still beat Glaeda by 0.75 s, or 7.3%, on that edit class. That result is retained as the current target: Glaeda is already near ordinary warm latency while adding private writable state and exact resident identity, but it has not yet beaten a normal warm Cargo worktree for this workload. See [`docs/DEVELOPER_LOOP_BENCHMARK.md`](docs/DEVELOPER_LOOP_BENCHMARK.md).

## Execution classes

| Trust class | Normal execution posture |
| --- | --- |
| Hostile / unknown | Fresh isolated worker, bounded capabilities, one workload, exact teardown and absence evidence. |
| Trusted repeatable compute | Prepared workers or warm pools may consume reviewed reusable generations while preserving a clean execution boundary. |
| Ultra-trusted resident compute | Long-lived project/compute state and selected services may remain resident under exact owner, generation, validity, capability, reset, and eviction rules. |

Resident state is acceleration and working state. It gains no independent source, ownership, result, merge, cleanup, or mutation authority by surviving. Durable decision state plus fresh observation drives recovery, and every hot-state family keeps a complete reset or cold-rebuild path.

Hostile-work details live in [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md). Ownership, recovery, mutation, subprocess, and physical-experiment rules live in [`docs/AGENT_EXECUTION_SAFETY.md`](docs/AGENT_EXECUTION_SAFETY.md).

## What Glaeda owns

Glaeda owns the compute-side layer shared across workload families:

- exact workload, attempt, ownership, and reusable-state identities;
- trust/capability and host-capacity admission;
- backend/host selection and lifecycle control;
- durable no-replay recovery across controller or machine interruption;
- hot-state validity, reuse, reset, retention, and teardown policy;
- bounded human/JSON evidence for results, recovery debt, and performance decisions.

Workload adapters own domain semantics and result authority. GitHub Actions remains the ordinary workflow scheduler, check/status surface, and hosted log owner for GitHub jobs. Backends such as Lima/VZ, native Linux, operator-owned fleet nodes, and future VM/container or burst providers remain mechanisms selected through capability and complete-loop evidence. When several orchestrators share one physical node, caller identity, semantic workload identity, and the physical execution lease remain independent; every participating adapter converges on one local collision boundary before consuming scarce resources. See [`docs/MULTI_ORCHESTRATOR_INTEROP.md`](docs/MULTI_ORCHESTRATOR_INTEROP.md).

The first production host/backend is an operator-owned Apple-silicon Mac with Linux execution through Lima/VZ. macOS is the trusted control plane; filesystem-heavy workload execution happens inside Linux. See [`docs/LINUX-ACCEPTANCE.md`](docs/LINUX-ACCEPTANCE.md) and the threat model for the exact boundary.

## Commands

Inspect the current machine and plan repository work without applying it:

```bash
cargo run --locked -- --output json doctor
cargo run --locked -- plan --file examples/quarry.yml
cargo run --locked -- host plan --file examples/quarry.yml
```

`doctor` reports local readiness. `plan` validates a manifest and computes project actions. `host plan` adds host preparation decisions. Planning is read-only.

Host preparation is an explicit privileged path:

```bash
cargo build --locked
sudo ./target/debug/glaeda host prepare --file examples/quarry.yml
sudo ./target/debug/glaeda host prepare --file examples/quarry.yml --confirm EXACT_CONFIRMATION
```

Review the exact built binary and plan before granting privilege. Repository code, scripts, and ordinary agent work never gain privilege merely because Glaeda can prepare a host.

Project manifests describe project identity and desired execution policy; project repositories retain executable verification and build behavior. See [`docs/MANIFEST.md`](docs/MANIFEST.md) and [`examples/`](examples/) for the current schema and examples.

## Design principles

- **Exact identity before effects.** Names, paths, PIDs, tags, surviving processes, and cache presence are observations rather than ownership proof.
- **Fresh observation before mutation or release.** External state may change after any prior read or command response.
- **Durable truth survives physical loss.** VMs, caches, worktrees, disks, indexes, and resident services remain replaceable unless explicitly classified otherwise.
- **Trust decides reuse.** Hostile work stays fresh; trusted work may reuse reviewed state; ultra-trusted work may retain mutable state under explicit leases and reset policy.
- **Execution authority and result authority stay separate.** A successful process or reusable artifact carries only the meaning granted by its workload contract.
- **Cold fallback stays complete.** Hot-state failure, drift, or ambiguity must converge through reset, quarantine, rebuild, or explicit recovery debt.
- **Native mechanisms come first.** Glaeda composes mature operating-system, Git, filesystem, cgroup, service-manager, VM/container, and provider primitives instead of replacing them without measured need.
- **Measure complete useful-result paths.** Performance claims bind exact comparable inputs, validation, resource cost, and fallback behavior.

## Development

Glaeda uses Rust 2024 on stable Rust and commits `Cargo.lock`.

```bash
./scripts/bootstrap
./scripts/verify fast
./scripts/verify full-tests
./scripts/verify required
```

`fast` is the compact inner loop, `full-tests` runs the complete test suite, and `required` is the repository-required final verification profile for code changes. The verification helper also supports `--plan-json` and bounded receipts.

Documentation-only changes follow the repository's docs-only verification policy; `.github/workflows/ci.yml` intentionally ignores Markdown and `docs/**` changes.

## Project documents

- [`docs/COMPUTE_RUNTIME.md`](docs/COMPUTE_RUNTIME.md) — general compute-runtime boundary and typed workload seam.
- [`docs/MULTI_ORCHESTRATOR_INTEROP.md`](docs/MULTI_ORCHESTRATOR_INTEROP.md) — caller/request/physical-lease interoperability for shared CMUX-owned hardware.
- [`docs/BLAZINGLY_HOT.md`](docs/BLAZINGLY_HOT.md) — trusted residency and hot execution.
- [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md) — hostile/unknown execution boundary.
- [`docs/AGENT_EXECUTION_SAFETY.md`](docs/AGENT_EXECUTION_SAFETY.md) — ownership, durable state, mutation, subprocess, and experiment safety.
- [`docs/WORKSPACE_BOOTSTRAP.md`](docs/WORKSPACE_BOOTSTRAP.md) — repository bootstrap contract.
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — current sequencing and detailed programme state.
- [`docs/MANIFEST.md`](docs/MANIFEST.md) — project manifest model.
- [`docs/MODULE_INVENTORY.md`](docs/MODULE_INVENTORY.md) — generated: every exported module and what reaches it.
- [`AGENTS.md`](AGENTS.md) — repository instructions and task routing for agents.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
