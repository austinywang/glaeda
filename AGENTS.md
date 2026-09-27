# Glaeda agent entry

Use **Glaeda** for the project and **`glaeda`** for the binary/crate. Use
**SmolRunner** only in history, v1, or migration references.

## Start here

1. Run `./scripts/bootstrap`.
2. Read the assigned issue and all current comments, then inspect the current PR,
   Git state, and overlapping work before editing.
3. Read only the contract for the surface you will change:

| Work surface | Read next |
| --- | --- |
| Hostile/disposable execution | `docs/THREAT_MODEL.md` plus applicable `docs/DISPOSABLE_*.md` |
| Residency, reuse, cache, performance | `docs/BLAZINGLY_HOT.md` plus the owning issue |
| Workspace bootstrap | `docs/WORKSPACE_BOOTSTRAP.md` |
| Ownership, persistence, mutation, recovery, subprocesses, physical experiments | `docs/AGENT_EXECUTION_SAFETY.md` |
| Delegation and multi-agent work | `docs/AGENT_COORDINATION.md` |
| Lease transitions | `docs/adr/0004-lease-lifecycle-core.md` |
| Adding, exporting, or retiring a public module | `docs/MODULE_INVENTORY.md` (generated; do not hand-edit) |

Changing coordination belongs in current issues and PRs. Keep broad document
preloading out of the normal startup path.

## Product boundary

Glaeda is a Rust compute runtime with three execution classes: hostile/unknown,
trusted repeatable, and ultra-trusted resident. GitHub Actions remains the normal
workflow scheduler and check surface; Glaeda owns compute-side admission,
lifecycle, reusable-state policy, and recovery.

Product details live in `README.md` and `docs/COMPUTE_RUNTIME.md`.

## Universal invariants

- Exact identity, bounded authority, fresh observation before action/release,
  durable crash recovery, and rebuildability form the correctness kernel.
- Trust decides residency. Surviving VMs, processes, worktrees, caches, indexes,
  artifacts, or services gain no source, ownership, result, merge, cleanup, or
  mutation authority by surviving. Reuse binds exact trust/project/lease/source/
  toolchain/capability/reset identity.
- Names, paths, PIDs, tags, slots, cache hits, survivors, and lease IDs are
  observations rather than ownership proof. Ambiguous state goes through fresh
  revalidation, reset, quarantine, rebuild, or explicit recovery; protect
  foreign, conflicting, and unknown state.
- Planning and read paths perform no side effects. Mutation requires validated
  preconditions, a complete plan, defined rollback/compensation semantics, and
  fresh post-effect observation. `docs/AGENT_EXECUTION_SAFETY.md` owns the cold
  mechanics.
- Performance observations, diagnoses, optimizer outputs, and desired-state
  recommendations carry advisory authority only. Preserve complete cold/reset
  fallback and prove fixes by reproducing the failure then observing the repair.
- `unsafe` Rust is forbidden. Use locked Cargo resolution and commit `Cargo.lock`.
- Human and JSON output must stay bounded and typed. Keep secrets, credentials,
  raw logs, repository contents, environment dumps, and private paths out of
  public evidence.

## Native Apple agent builds

For initialized native projects, prefer `glaeda-apple submit --operation check`.
After coordinated pulls or branch changes, use `refresh` to ensure dependency
readiness and then check incrementally; `plan-refresh` previews both stages.
Save the returned request id; use `wait-request --request-id <id> --wait-seconds 300`
in an active runner’s asynchronous process facility for event-based completion,
or collect it with `request-status --request-id <id>`
and explicitly `forget-request --request-id <id>` after consuming a terminal result.
Use operation `dependencies` for preparation and `build` for a complete app build.
The worker starts on demand and exits when idle; `wake` resumes pending work after
interruption. Submission targets latest source at execution, not a pinned revision.
Preserve each project's launch rules and coordinate source/branch edits separately.
Never run direct and managed builds concurrently. Foreground callers can still use
`check --wait-seconds 300`; timeout never authorizes killing another builder or
removing its state. See `docs/APPLE_NATIVE_BUILDS.md` for recovery, receipt scope,
source-observation limits and the bounded request history.

## Verify

The full suites run in GitHub Actions (`.github/workflows/ci.yml`, the Verify
workflow), which shards the heavy ones: `scripts/test-glaeda-cmux-runner.py` runs
as four shards there. Do not run full suites on a workstation. Several agent
sessions share one laptop, and a full runner-hook suite spawns dozens of fake
runner processes for minutes; parallel copies of it (and orphans left by a
crashed session) have driven the owner's Mac past load 150.

Locally, run only what the change touches:

```bash
./scripts/verify focused                  # compile, format, repo-query test
python3 scripts/test-<file>.py -k <name>  # only the cases you changed or added
```

Run one test file or `-k` selection at a time, in the foreground, with a
timeout, and never leave a test process behind when the session ends. Then push
the branch and let the Verify workflow run the full suites; its result is the
final verification. Use `./scripts/verify fast`, `full-tests` or `required`
locally only when the owner asks, or on a fleet mini rather than a workstation.

Documentation-only changes follow the repository docs-only policy. Record when
GitHub's normal Verify workflow is intentionally absent for Markdown/`docs/**`
changes.

## Finish

- Complete work in the active session or leave one exact durable blocked
  continuation. Avoid polling loops.
- Review the full diff and exact head. Record relevant checks, authority, and
  provider evidence for the surface changed.
- Claim only evidence observed for the exact tested head and physical effect.
- Low-risk repository edits may use self-review. Privilege, ownership/adoption,
  persistence/recovery, rollback, secrets, concurrency, and destructive changes
  use the independent review required by their owning contract.
- Ordinary repository writes and merges follow repository authority and the
  expected reviewed head. Credentials/access widening, material spend, operator
  service/network changes, releases/signing, external contact, destructive
  non-test data changes, and irreversible migrations require human approval.

## Agent-facing writing

Prefer short commands, concrete nouns, explicit outcomes, and one rule per owner.
Repository scripts and tests are the executable truth.
