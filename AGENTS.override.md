# Glaeda: agent hot path

Glaeda is a Rust runtime for blazingly hot, trust-tiered Linux work on operator-controlled compute.
Use **Glaeda** / `glaeda`; keep **SmolRunner** only in truthful historical or v1 identities.

## Product boundary

- Hostile/unknown work: one fresh isolated Linux worker, one bounded job, exact teardown.
- Trusted CI: prepared workers and reviewed reusable generations with a clean job boundary.
- Ultra-trusted project work: resident project state under explicit lease, validity, reset, and
  revalidation rules.
- GitHub Actions remains the GitHub scheduler/workflow language. Glaeda owns local admission,
  execution policy, identity, lifecycle, recovery, and measured hot-state decisions.
- Do not build a new pipeline language, runner protocol, container/hypervisor runtime, Kubernetes
  controller, public multi-tenant platform, generic cloud scheduler, or automatic deployment tool.

## Correctness kernel

- Exact identity and fresh observation decide ownership, adoption, reuse, mutation, and cleanup.
  Names, paths, tags, PIDs, surviving services, lease IDs, and emitted metadata do not.
- Hostile jobs receive no host integrations or unrelated secrets; constrain network/resources and
  destroy useful hostile writable persistence after the job.
- Resident state carries no independent workflow-result, source, ownership, merge, or cleanup
  authority. Ambiguity means revalidate, reset, quarantine, or cold reconstruction.
- Every hot-state family needs canonical rebuild inputs and bounded reset/revalidation/eviction.
  Losing hot state may cost time, never truth.
- Invalid or irreversible mutation plans fail before the first executor call. Mutations need exact
  ownership, plan/dry-run behavior, rollback or accurately named compensation, and fresh
  post-action evidence.
- Never expose secrets, credentials, tokens, private repository contents, arbitrary logs, raw
  environment dumps, or unrelated machine data. Redaction is defense in depth.
- No unsafe Rust. Use absolute executable paths and argument vectors; child environments start
  empty and receive only allowlisted values.

## Work

- Read the smallest relevant code/doc range. Use [`AGENTS.md`](AGENTS.md) only for one named deep
  route: current priorities, hot-state policy, exact verification, implementation rules, physical
  experiments, coordination, or merge/review policy.
- Bootstrap read-only capability evidence with `./scripts/bootstrap --output json`.
- Fast feedback: `./scripts/verify fast`. Before publishing code: `./scripts/verify required`.
  Compact output is the default: it preserves the exact phase argv, emits one decision-focused line
  per phase, and shows a bounded tail on failure. Use `--output-mode stream` only when live child
  output is itself needed. Documentation-only changes may use the existing docs-only policy. Never
  upgrade a failed or blocked result into a pass.
- Measure complete comparable agent loops. Record baseline/candidate identity, fixed work,
  semantic validation, latency, resource/storage effects, and reset/fallback. Physical experiments
  need explicit boundaries and cleanup/rebuild evidence.
- Tests must not require privileged or physical infrastructure unless explicitly isolated and
  opted into. Planning must not mutate host, GitHub, fleet, provider, release, or durable state.
- Prefer mature OS/filesystem/service-manager/runtime primitives. Add dependencies or competing
  control machinery only for a measured gap and narrow maintenance rationale.

## Repository workflow

- Inspect the complete worktree and existing issues/PRs. Preserve concurrent work. Keep changes
  reviewable and report security impact, commands, and host assumptions.
- Ordinary in-scope issues, branches, commits, PRs, comments, reviews, and merges are allowed.
  Routine low-risk exact heads may be self-reviewed and merged after required checks. Security,
  privilege, ownership/adoption, persistence/recovery, destructive, rollback, or race-sensitive
  changes require implementation-independent exact-head acceptance.
- Merge only the reviewed expected head when GitHub reports it mergeable and required checks pass.
  External credentials, services, paid capacity, releases, and physical machine/network/storage
  changes still require their own authority.
- Do not schedule/poll implementation agents or use unavailable external review services. If work
  is delegated, follow [`docs/AGENT_COORDINATION.md`](docs/AGENT_COORDINATION.md).
