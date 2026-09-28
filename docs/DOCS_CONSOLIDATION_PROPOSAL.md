# Documentation consolidation proposal

Snapshot: origin/main `f538b57`, 2026-09-23. Proposal only: nothing below has been moved yet.
The generated status prototype is `scripts/status_page.py` and its output is `docs/STATUS.md`.

## Headline numbers

| Measure | Value |
| --- | --- |
| Files inventoried (docs/ tree plus README.md and AGENTS*.md) | 112 (107 Markdown, 5 JSON) |
| Total lines | 18,509 |
| Historical or superseded files | 14 files, 4,624 lines (25.0%) |
| Direction docs that overlap each other (ROADMAP, COMPUTE_RUNTIME, OPERATING_MODEL, PRODUCT_EVOLUTION, ACCELERATOR plan) | 5 files, 1,266 lines (6.8%) |
| Dated experiment records | 7 files, 776 lines (4.2%) |
| Markdown files with zero inbound links from any other doc or code | 73 of 107 |
| Files that code, tests, or CI workflows cite by path | 17 (4 JSON fixtures, 4 Podman docs cited by workflows, the generated module inventory, the history/ pair, and others) |
| Open issues | 185, 1.36M characters of body text (about 340k tokens) |
| Open issues whose bodies carry superseded / original-spec / historical-status structure | 28, 287k characters |
| Open issues not updated since 2026-09-01 | 146, 968k characters (71% of issue body weight) |
| Open issues titled `M6 ...` (the project-disk / resident-sandbox tree) | 57, 522k characters |

The weight is not in the docs tree. It is in open issue bodies: a reader who opens the M6 tree
alone faces roughly 130k tokens, most of it last touched on 2026-08-25.

## Inventory

Columns: lines, last commit date, inbound references from other docs (`doc`) and from
src/tests/scripts/tools/CI (`code`), one-line scope, state, proposed action. Paths are relative
to `docs/` except the three root files. Inbound counts are basename grep hits, so they include
prose mentions as well as links.

State legend: `current` (describes live code or policy), `contract (code/CI ref)` (a file path is
cited by code, tests, or a workflow), `generated`, `decision record` (ADR), `direction` (product
narrative), `superseded` (the file says so), `historical` (the file says it is historical, or its
programme is deferred/sealed), `experiment record` (dated measurement), `fixture (test ref)`.

| File | Lines | Last commit | doc | code | Covers | State | Action |
| --- | ---: | --- | ---: | ---: | --- | --- | --- |
| `ACCELERATOR_BURST_EXECUTION.md` | 108 | 2026-09-07 | 0 | 0 | Provider-neutral rented-GPU burst plan | direction | reference/ (design plan) |
| `ADAPTIVE_VERIFICATION_COMPILER.md` | 181 | 2026-09-21 | 0 | 0 | Pure reducer for #547 verification optimization | current | reference/ |
| `AGENT_COORDINATION.md` | 148 | 2026-08-26 | 2 | 0 | Multi-agent delegation protocol | current | keep (AGENTS route) |
| `AGENT_EXECUTION_SAFETY.md` | 90 | 2026-09-01 | 2 | 0 | Ownership, mutation, recovery, subprocess rules | current | keep (AGENTS route) |
| `APPLE_NATIVE_BUILDS.md` | 520 | 2026-09-21 | 2 | 0 | glaeda-apple managed native builds and recovery | current | reference/ (AGENTS route) |
| `BLAZINGLY_HOT.md` | 998 | 2026-09-22 | 4 | 1 | Residency, reuse, hot-state policy | contract (code/CI ref) | keep path; trim dated receipt history to archive |
| `CMUX_FLEET_ENROLLMENT.md` | 337 | 2026-09-22 | 1 | 1 | CMUX fleet node enrollment lifecycle | contract (code/CI ref) | keep path (scripts/fleet_bundle.py) |
| `CMUX_FLEET_EXECUTION_ROLES.md` | 334 | 2026-09-21 | 0 | 0 | CMUX fleet roles and workload capabilities | current | reference/cmux/ |
| `CMUX_WORKLOAD_PROFILES.md` | 190 | 2026-09-22 | 0 | 0 | CMUX repo-owned workload profiles | current | reference/cmux/ |
| `COMPUTE_RUNTIME.md` | 249 | 2026-08-31 | 5 | 0 | General compute runtime product model | direction | merge into ARCHITECTURE.md |
| `DESCRIPTOR_BOUND_LINUX_LAUNCHER.md` | 67 | 2026-07-28 | 0 | 0 | Module contract: descriptor-bound launcher | current | reference/ |
| `DEVELOPER_LOOP_BENCHMARK.md` | 1100 | 2026-08-30 | 2 | 0 | Developer-loop benchmark contract plus dated results | superseded | split: contract to reference/, dated sections to archive/ |
| `DISPOSABLE_AUTOSCALING_CI.md` | 254 | 2026-08-26 | 9 | 0 | One-job disposable VM autoscaling CI | current | keep (AGENTS route, 9 inbound) |
| `DISPOSABLE_DEPENDENCY_BOUNDARY.md` | 340 | 2026-08-26 | 0 | 0 | Decision candidate for old smolrunner#368 | historical | convert to ADR or archive/ |
| `DISPOSABLE_INSTALLED_SERVICE_ACCEPTANCE.md` | 239 | 2026-08-26 | 0 | 0 | Installed disposable service physical acceptance | current | keep (DISPOSABLE_* route) |
| `DISPOSABLE_VZ_NETWORK_SELECTOR_ACCEPTANCE.md` | 235 | 2026-08-26 | 0 | 0 | VZ network selector physical acceptance | current | keep (DISPOSABLE_* route) |
| `EXECUTION_ADMISSION.md` | 13 | 2026-07-27 | 0 | 0 | Module contract: admission/reservation | current | reference/ |
| `EXECUTION_RECEIPTS.md` | 91 | 2026-08-26 | 0 | 0 | External execution receipt contract | current | reference/receipts/ |
| `EXECUTION_RECEIPT_STORAGE.md` | 58 | 2026-08-26 | 0 | 0 | Durable receipt storage | current | reference/receipts/ |
| `EXTERNAL_EXECUTION_REQUEST.md` | 189 | 2026-09-21 | 0 | 0 | External semantic execution request v1 | current | reference/ |
| `FLEET_DISTRIBUTION.md` | 108 | 2026-09-23 | 1 | 0 | Fleet candidate distribution | current | reference/cmux/ |
| `HOST_PREPARATION_RECEIPT_BINDING.md` | 82 | 2026-07-27 | 0 | 0 | Host-prep receipt binding | current | reference/receipts/ |
| `HOST_PREPARATION_RECEIPT_MAPPING.md` | 80 | 2026-07-27 | 0 | 0 | Host-prep receipt mapping | current | reference/receipts/ |
| `HOST_RECONCILIATION.md` | 52 | 2026-08-26 | 0 | 0 | Desired/observed/planned host state model | current | reference/ |
| `LEASED_EXECUTION.md` | 168 | 2026-08-26 | 0 | 0 | Deferred leased workspaces and previews | historical | archive/ |
| `LIMA_DEVELOPMENT.md` | 86 | 2026-08-26 | 0 | 0 | Historical persistent Lima dev guest | historical | archive/ |
| `LINUX-ACCEPTANCE.md` | 101 | 2026-08-26 | 1 | 0 | Linux acceptance test runbook | current | reference/runbooks/ |
| `LINUX_HOST_OBSERVATION.md` | 71 | 2026-08-31 | 1 | 0 | Linux host observation contract | current | reference/ |
| `M6_HOT_RUNTIME_HANDOFF.md` | 574 | 2026-09-22 | 2 | 0 | M6 continuation snapshot (2026-08-25) | historical | archive/ |
| `MACBOOK-RUNNER-BOOTSTRAP.md` | 124 | 2026-08-26 | 0 | 0 | Manual Lima bootstrap before host prepare | historical | archive/ |
| `MACBOOK-RUNNER-QUICKSTART.md` | 288 | 2026-08-26 | 1 | 0 | Legacy smolrunner dev VM operator path | historical | archive/ after Quarry fast lane moves |
| `MACBOOK-RUNNER.md` | 323 | 2026-08-26 | 0 | 0 | Historical persistent-guest operator guide | historical | archive/ |
| `MACOS_RESOURCE_OBSERVATION.md` | 34 | 2026-07-27 | 0 | 0 | macOS resource observation contract | current | reference/ |
| `MANIFEST.md` | 104 | 2026-08-26 | 1 | 0 | glaeda.yml manifest reference | current | reference/ |
| `MODULE_INVENTORY.md` | 293 | 2026-09-23 | 3 | 1 | Generated public-module inventory | generated | keep path (tests/module_inventory.rs) |
| `MULTI_ORCHESTRATOR_INTEROP.md` | 278 | 2026-09-21 | 1 | 0 | Multi-orchestrator lease interop (#1057) | current | reference/cmux/ |
| `OPERATING_MODEL.md` | 314 | 2026-08-26 | 0 | 0 | Control loop and fleet operating direction | direction | merge into ARCHITECTURE.md |
| `OWNED_FLEET_BENCHMARK.md` | 435 | 2026-09-22 | 0 | 0 | Owned-fleet benchmark receipt contract | current | reference/benchmarks/ |
| `OWNER_LOCAL_SEMANTIC_API.md` | 306 | 2026-09-22 | 0 | 0 | Owner-local semantic API v1 | current | reference/ |
| `PERSONAL_WORKER_ACCEPTANCE.md` | 616 | 2026-08-10 | 0 | 0 | Persistent worker alpha acceptance plan | superseded | archive/ |
| `PERSONAL_WORKER_ALPHA.md` | 325 | 2026-08-10 | 1 | 0 | Persistent worker alpha contract | superseded | archive/ |
| `PERSONAL_WORKER_STORE_WRITER_CONTRACT.md` | 35 | 2026-07-27 | 0 | 0 | Personal worker store writer | current | reference/personal-worker/ |
| `PNPM_TASK_DEPENDENCY_BENCHMARK.md` | 112 | 2026-09-21 | 0 | 0 | pnpm task dependency benchmark | current | reference/benchmarks/ |
| `PODMAN_CONTAINER_CLOSURE_EVIDENCE.md` | 153 | 2026-08-09 | 0 | 1 | Partial Podman container fixture evidence | contract (code/CI ref) | archive/podman/ with workflow path update |
| `PODMAN_CURRENT_EXECUTION_CLOSURE_AUDIT.md` | 107 | 2026-08-28 | 0 | 2 | Podman closure audit, current | contract (code/CI ref) | archive/podman/ with workflow path update |
| `PODMAN_EXECUTION_CLOSURE.md` | 498 | 2026-08-10 | 0 | 2 | Podman execution closure audit | contract (code/CI ref) | archive/podman/ with workflow path update |
| `PODMAN_EXECUTION_CLOSURE_EVIDENCE.md` | 305 | 2026-08-10 | 0 | 1 | Partial Podman package evidence | contract (code/CI ref) | archive/podman/ with workflow path update |
| `PRODUCT_EVOLUTION.md` | 189 | 2026-08-26 | 0 | 0 | Product history and scope discipline | direction | merge scope rules into ARCHITECTURE.md; history to archive/ |
| `PROJECT_DISK_HOST_OBSERVATION.md` | 83 | 2026-08-26 | 0 | 0 | M6 #565 project-disk observer | current | reference/project-disk/ |
| `PROJECT_DISK_PHYSICAL_RECEIPT.md` | 235 | 2026-08-26 | 0 | 0 | M6 #565 project-disk receipt tool | current | reference/project-disk/ |
| `PROJECT_WORKSPACES.md` | 529 | 2026-08-31 | 0 | 0 | Developer project namespace and recovery | current | reference/ |
| `PROTECTED_CACHE_GENERATION_CATALOG.md` | 58 | 2026-08-30 | 0 | 0 | Protected cache catalog contract | current | reference/protected-cache/ |
| `PROTECTED_CACHE_NAMESPACE_LEASE_VISIBILITY.md` | 55 | 2026-08-30 | 0 | 0 | Protected cache lease visibility | current | reference/protected-cache/ |
| `PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE.md` | 51 | 2026-08-30 | 0 | 0 | Protected cache replacement equivalence | current | reference/protected-cache/ |
| `QUARRY_HOT_PROJECT_DOGFOOD.md` | 242 | 2026-08-26 | 0 | 0 | Quarry hot-project experiment contract (sealed) | historical | archive/ |
| `QUARRY_PARALLEL_VERIFICATION_RECEIPT.md` | 70 | 2026-08-30 | 0 | 0 | Quarry parallel receipt | current | reference/receipts/ |
| `RENDERPROVE_ARTIFACT_BINDING.md` | 18 | 2026-07-27 | 0 | 0 | Renderprove artifact receipt binding | current | reference/renderprove/ |
| `RENDERPROVE_VISION_PROFILE.md` | 99 | 2026-08-26 | 0 | 0 | Renderprove vision profile | current | reference/renderprove/ |
| `REUSABLE_STATE_LIFECYCLE.md` | 415 | 2026-09-22 | 0 | 2 | Reusable-state lifecycle | contract (code/CI ref) | keep path (src + tests cite it) |
| `ROADMAP.md` | 406 | 2026-08-26 | 3 | 1 | Product goals and M1-M7 milestones (08-26) | direction | merge goals into ARCHITECTURE.md; milestones to STATUS/issues |
| `THREAT_MODEL.md` | 66 | 2026-08-26 | 5 | 0 | Threat model | current | keep (AGENTS route) |
| `TRUSTED_AGENT_DISPATCH.md` | 223 | 2026-09-22 | 0 | 0 | Trusted-agent dispatch v1 | current | reference/ |
| `TRUSTED_WORKSPACE_CACHE_RECEIPT.md` | 62 | 2026-08-31 | 0 | 0 | Trusted workspace/cache receipt producer | current | reference/receipts/ |
| `WORKSPACE_BOOTSTRAP.md` | 235 | 2026-08-31 | 3 | 0 | Workspace bootstrap contract | current | keep (AGENTS route) |
| `WORKSPACE_BOOTSTRAP_PROFILE_COMPATIBILITY.md` | 78 | 2026-08-26 | 0 | 0 | Bootstrap to verify-profile compatibility | current | reference/ |
| `adr/0001-privilege-adoption-and-rollback.md` | 95 | 2026-07-24 | 0 | 0 | Privilege lanes, adoption, and rollback | decision record | keep; add adr/README index |
| `adr/0002-durable-ownership-state.md` | 125 | 2026-07-24 | 0 | 0 | Durable ownership and host state identity | decision record | keep; add adr/README index |
| `adr/0003-canonical-resource-evidence.md` | 101 | 2026-07-24 | 0 | 0 | Canonical locators and resource evidence | decision record | keep; add adr/README index |
| `adr/0004-lease-lifecycle-core.md` | 116 | 2026-07-24 | 3 | 0 | lease lifecycle core | decision record | keep; add adr/README index (routed) |
| `adr/0005-lease-store-and-artifact-identity.md` | 73 | 2026-07-24 | 0 | 0 | Atomic lease stores and immutable artifact identity | decision record | keep; add adr/README index |
| `adr/0006-preview-slot-coalescing.md` | 46 | 2026-07-24 | 0 | 0 | Preview slot coalescing | decision record | keep; add adr/README index |
| `adr/0007-rootless-podman-preview-command-planning.md` | 81 | 2026-07-25 | 0 | 0 | Rootless Podman preview command planning | decision record | keep; add adr/README index |
| `adr/0008-podman-preview-inspection-authorization.md` | 62 | 2026-07-25 | 0 | 0 | Podman preview inspection and mutation authorization | decision record | keep; add adr/README index |
| `adr/0009-bounded-subprocess-output.md` | 45 | 2026-08-09 | 0 | 0 | Bounded subprocess output capture | decision record | keep; add adr/README index |
| `adr/0010-podman-inspect-execution-receipts.md` | 47 | 2026-07-25 | 0 | 0 | Podman inspect execution receipts | decision record | keep; add adr/README index |
| `adr/0011-podman-state-aware-mutation-authorization.md` | 53 | 2026-07-25 | 0 | 0 | Podman state-aware mutation authorization | decision record | keep; add adr/README index |
| `adr/0012-podman-state-reconciliation-plans.md` | 47 | 2026-07-25 | 0 | 0 | Podman state reconciliation plans | decision record | keep; add adr/README index |
| `adr/0013-durable-linux-lease-store.md` | 52 | 2026-07-25 | 0 | 0 | Durable Linux lease-store publication | decision record | keep; add adr/README index |
| `adr/0014-durable-execution-journal-checkpoints.md` | 55 | 2026-07-25 | 0 | 0 | Durable execution-journal checkpoints | decision record | keep; add adr/README index |
| `adr/0014-remediation-applicability.md` | 146 | 2026-08-13 | 0 | 0 | remediation applicability is separate from diagnosis and authority | decision record | keep; add adr/README index |
| `adr/0015-typed-privilege-lane-executors.md` | 54 | 2026-07-25 | 0 | 0 | Typed privilege-lane executors | decision record | keep; add adr/README index |
| `adr/0016-debian-package-preparation-planning.md` | 53 | 2026-07-25 | 0 | 0 | Conservative Debian-family prerequisite package planning | decision record | keep; add adr/README index |
| `adr/0017-runner-account-preparation-planning.md` | 73 | 2026-07-25 | 0 | 0 | Dependency-aware runner account preparation planning | decision record | keep; add adr/README index |
| `adr/0018-runner-account-observation.md` | 79 | 2026-07-25 | 0 | 0 | Conservative runner account preparation observation | decision record | keep; add adr/README index |
| `adr/0019-debian-package-recovery-classification.md` | 72 | 2026-07-25 | 0 | 0 | Debian package attempt and recovery classification | decision record | keep; add adr/README index |
| `adr/0019-rootless-podman-readiness-observation.md` | 119 | 2026-07-25 | 0 | 0 | Rootless Podman readiness observation | superseded | keep; add adr/README index |
| `adr/0020-rootless-podman-preflight-and-smoke-boundary.md` | 85 | 2026-07-25 | 0 | 0 | Split rootless Podman preflight from first-run smoke verification | decision record | keep; add adr/README index |
| `adr/0021-subordinate-id-reconciliation-planning.md` | 59 | 2026-07-25 | 0 | 0 | Subordinate-ID reconciliation planning | decision record | keep; add adr/README index |
| `adr/0022-mac-availability-transition-planning.md` | 82 | 2026-07-26 | 0 | 0 | Mac availability transition planning | decision record | keep; add adr/README index |
| `experiments/cmux-workload-profile/README.md` | 17 | 2026-09-21 | 2 | 0 | Synthetic CMUX workload plan | fixture (test ref) | keep |
| `experiments/cmux-workload-profile/plan.json` | 1 | 2026-09-21 | 2 | 1 | Fixture | fixture (test ref) | keep |
| `experiments/cmux-workload-profile/request.json` | 1 | 2026-09-21 | 4 | 2 | Fixture | fixture (test ref) | keep |
| `experiments/codex-exact-commit-handoff-2026-07-26.md` | 76 | 2026-07-26 | 0 | 0 | Retired exact-commit handoff experiment | experiment record | archive/ |
| `experiments/external-execution-request/README.md` | 56 | 2026-09-21 | 2 | 0 | Synthetic external request loop | fixture (test ref) | keep |
| `experiments/external-execution-request/cmux-request.json` | 1 | 2026-09-21 | 2 | 1 | Fixture | fixture (test ref) | keep |
| `experiments/external-execution-request/glaeda-result.json` | 1 | 2026-09-21 | 2 | 1 | Fixture | fixture (test ref) | keep |
| `experiments/fleet-compilation-cache-2026-09-23.md` | 117 | 2026-09-23 | 0 | 1 | Fleet Xcode compilation cache measurements | experiment record | keep (tools README cites it) |
| `experiments/owned-fleet-benchmark-v1-preexecution.md` | 221 | 2026-09-22 | 0 | 0 | Owned-fleet benchmark pre-execution report | experiment record | keep |
| `experiments/quarry-parallel-big-red-2026-09-11.md` | 169 | 2026-09-11 | 0 | 0 | Quarry parallel Big Red result | experiment record | keep |
| `experiments/quarry-sweep-big-red-2026-09-10.md` | 108 | 2026-09-10 | 0 | 0 | Quarry sweep Big Red result | experiment record | keep |
| `experiments/resident-repo-query-big-red-2026-08-31.md` | 84 | 2026-08-31 | 2 | 0 | Resident repo query result | experiment record | keep |
| `experiments/resident-repo-query-narrow-control-receipt.json` | 1 | 2026-08-31 | 1 | 0 | Receipt for the repo-query result | experiment record | keep |
| `history/IMPLEMENTATION_MAP.md` | 274 | 2026-09-22 | 1 | 1 | Retired capability map | superseded | archive/ (update generator text) |
| `history/RETIRED_FEATURE_ISLANDS.md` | 45 | 2026-08-16 | 2 | 1 | Retired feature islands | historical | archive/ (update tests/module_inventory.rs) |
| `local-admission-cli.md` | 36 | 2026-09-05 | 0 | 0 | Local interference admission CLI | current | reference/cli/ |
| `owned-linux-admission.md` | 160 | 2026-09-22 | 0 | 0 | Owned-Linux admission | current | reference/ |
| `personal-worker-cancel-cli.md` | 46 | 2026-08-26 | 0 | 0 | Personal worker cancel CLI | current | reference/cli/ |
| `personal-worker-read-cli.md` | 39 | 2026-08-26 | 0 | 0 | Personal worker read CLI | current | reference/cli/ |
| `personal-worker-submit-cli.md` | 64 | 2026-08-26 | 0 | 0 | Personal worker submit CLI | current | reference/cli/ |
| `README.md` | 164 | 2026-09-22 | 2 | 0 | Product front door, install, commands | current | keep |
| `AGENTS.md` | 115 | 2026-09-22 | 5 | 1 | Agent entry: routes, invariants, verify, finish | current | keep; add STATUS/ARCHITECTURE/reference routes |
| `AGENTS.override.md` | 66 | 2026-08-31 | 0 | 0 | Agent hot path: product boundary, correctness kernel | current | keep |

ADR numbering has two collisions (`0014-*` twice, `0019-*` twice). Do not renumber: links and
issue comments cite the existing names. The ADR index records both and new ADRs start at 0023.

## Issue bodies

One `gh issue list` call (185 open issues). Detection used body regexes.

| Pattern | Issues | Body characters |
| --- | ---: | ---: |
| Mentions "supersede" | 21 | 222k |
| Heading naming history / original / previous scope | 8 | 89k |
| `<details>` collapsed original spec | 6 | 65k |
| Dated living-status heading ("Current status", "Landed ...", "Implementation boundary") | 6 | 43k |
| Union of the structures above | 28 | 287k |

Heaviest bodies: #548 (38k), #547 (37k), #644 (27k), #546 (22k), #1047 (18k), #750 (17k),
#1058 (17k). 37 issues exceed 10k characters. The median body is about 6k characters.

The six newest RFC/epic issues (#525, #1056, #1057, #1058, #1068, #1095) already use a good
shape: a dated status section on top, then `<details><summary>Original scope ...` holding the
original spec. The problem is accumulation. #1056 stacks four dated status sections above the
fold, and the older issues (#546 to #548, #365, #723, #727) interleave superseded text with
current text and have no fold at all.

## Target structure

```text
README.md                 product front door (unchanged)
AGENTS.override.md        hot path (unchanged)
AGENTS.md                 routes; one row per surface, points into the tree below
docs/
  ARCHITECTURE.md         living product model: trust classes, boundary, kernel, hotness
  STATUS.md               generated by scripts/status_page.py; never hand-edited
  THREAT_MODEL.md         stays top level (routed, small)
  AGENT_*.md              stay top level (routed from AGENTS.md)
  DISPOSABLE_*.md         stay top level (routed family)
  BLAZINGLY_HOT.md, REUSABLE_STATE_LIFECYCLE.md, WORKSPACE_BOOTSTRAP.md,
  CMUX_FLEET_ENROLLMENT.md, MODULE_INVENTORY.md
                          stay in place: code, tests, or routes cite these paths
  adr/                    decisions; add README.md index with status column
  reference/              contracts for modules, receipts, CLIs, benchmarks, runbooks
    cli/ receipts/ cmux/ benchmarks/ protected-cache/ project-disk/
    personal-worker/ renderprove/ runbooks/
  experiments/            dated measurement records and synthetic fixtures (unchanged)
  archive/                superseded and historical docs, each with a one-line banner
    podman/               the four Podman closure docs (with workflow path updates)
```

Rationale: four places to look (ARCHITECTURE for "what is it", STATUS for "where is it",
adr for "why", reference for "exact contract"), plus archive and experiments that nobody reads
by default. Routed and code-cited files stay where they are, so no path a test, script, or
workflow reads changes in phase 1.

## Move list

The per-file action is in the inventory table. Grouped into three PRs, each docs-only unless noted.

### Phase 1: archive and index (docs-only, no cited path changes)

1. Create `docs/archive/` and move, each with a one-line banner naming its successor:
   `M6_HOT_RUNTIME_HANDOFF.md` (574), `PERSONAL_WORKER_ACCEPTANCE.md` (616),
   `PERSONAL_WORKER_ALPHA.md` (325), `MACBOOK-RUNNER.md` (323), `MACBOOK-RUNNER-BOOTSTRAP.md`
   (124), `LIMA_DEVELOPMENT.md` (86), `LEASED_EXECUTION.md` (168), `QUARRY_HOT_PROJECT_DOGFOOD.md`
   (242), `DISPOSABLE_DEPENDENCY_BOUNDARY.md` (340), and
   `experiments/codex-exact-commit-handoff-2026-07-26.md` (76). Fix the inbound links
   (`MACBOOK-RUNNER-QUICKSTART`, `M6_HOT_RUNTIME_HANDOFF`, `PERSONAL_WORKER_ALPHA` have 1 or 2).
2. Split `DEVELOPER_LOOP_BENCHMARK.md` (1,100 lines): keep the workload contract, control matrix,
   and next decisions (roughly lines 1 to 146) as `reference/benchmarks/DEVELOPER_LOOP_BENCHMARK.md`;
   move the ten dated 2026-08-30 sections and the superseded storage attribution to
   `archive/DEVELOPER_LOOP_BENCHMARK_RESULTS_2026-08.md`. Check
   `.github/workflows/developer-loop-benchmark.yml` for prose references first.
3. Add `docs/adr/README.md`: number, title, status (accepted / superseded), superseded-by. Note
   the 0014 and 0019 collisions.
4. Land `scripts/status_page.py`, its test, and a first `docs/STATUS.md` (this branch).

### Phase 2: ARCHITECTURE.md and reference/ (docs-only)

5. Write `docs/ARCHITECTURE.md` from `COMPUTE_RUNTIME.md` (product test, workload model),
   `ROADMAP.md` (goals, boundary, trust classes, durable kernel, hotness hierarchy, non-goals),
   `OPERATING_MODEL.md` (control loop), and the scope-discipline rules of
   `PRODUCT_EVOLUTION.md`. Target under 400 lines. Move the four sources to `archive/` with
   banners. ROADMAP's milestone sections (M1 to M7) go to archive; live milestone state comes
   from issues via STATUS.md. `tests/module_inventory.rs` renders text that names `ROADMAP.md`;
   update that string and regenerate `MODULE_INVENTORY.md` in the same PR (that makes this PR
   touch code).
6. Move the 40 module, receipt, CLI, benchmark, and runbook contracts listed as `reference/...`
   in the table. Update `README.md` and `AGENTS.md` links. None of these paths is cited by code.
7. `MACBOOK-RUNNER-QUICKSTART.md` moves to archive once the Quarry fast lane section has a
   current home (it is the only live content in the file).

### Phase 3: code-cited paths (touches code, needs Verify)

8. Move `docs/history/*` to `docs/archive/` and update `tests/module_inventory.rs`
   (`RETIRED_FEATURE_ISLANDS` path in doc comments and generator text), then regenerate
   `MODULE_INVENTORY.md`.
9. Move the four `PODMAN_*` docs to `docs/archive/podman/` and update the `paths:` filters in
   `.github/workflows/podman-closure-probe.yml` and `podman-container-closure-probe.yml`
   (the Podman track is deferred; #291 is labelled deferred).
10. Trim dated receipt-version history out of `BLAZINGLY_HOT.md` (998 lines) into
    `archive/`, keeping the path because `tests/reusable_state_hot_state_admission.rs` cites it.

Nothing is deleted. Every contract cited by code or tests keeps its path or moves in the same PR
that updates the citation.

## Rules that keep it this way

- **Issue bodies hold current status only.** Top section: `## Current status (YYYY-MM-DD)` with
  one paragraph (what landed, what is next, who owns it). When status changes, replace that
  section; post the old paragraph as a comment. The original spec stays folded in one
  `<details><summary>Original scope</summary>` block. No stacked dated status sections.
- **History lives in comments, Git, and `docs/archive/`.** A doc that says "superseded" or
  "historical" in its first screen moves to `archive/` in the same PR that supersedes it.
- **Dated measurements go to `docs/experiments/<topic>-<date>.md`.** Contract docs link to the
  newest record instead of accumulating dated sections.
- **One owner per rule.** ARCHITECTURE.md owns product model and boundaries; ADRs own decisions;
  reference/ owns exact contracts; STATUS.md owns nothing (generated).
- **Close or fold stale trees.** An open issue untouched for 21 days either gets a one-paragraph
  current status or is closed with a pointer. The M6 tree (57 issues) is the first candidate:
  one parent status paragraph, children closed as "tracked in parent" where no work is live.
- **New docs need an inbound route.** A new file under `docs/` is linked from AGENTS.md,
  ARCHITECTURE.md, an ADR, or `reference/README.md`. 73 of 107 Markdown files have no inbound
  reference today.

## How agents find things

AGENTS.override.md stays the hot path. AGENTS.md "Start here" gains three rows and the table
points into the new tree:

| Question | Read |
| --- | --- |
| What is in flight, what just landed | `docs/STATUS.md` (regenerate with `scripts/status_page.py --stdout` if stale) |
| What the product is and is not | `docs/ARCHITECTURE.md` |
| Why a decision was made | `docs/adr/README.md` |
| Exact contract for a module, receipt, or CLI | `docs/reference/README.md` (one line per file) |
| Existing surface rows (threat model, hot state, bootstrap, safety, coordination, lease ADR, module inventory) | unchanged |

Agents do not read `docs/archive/` or `docs/experiments/` unless an issue links them.

## Status page prototype

`scripts/status_page.py` (Python 3 stdlib, `gh api graphql` through subprocess):

- One GraphQL call fetches open issues (page 1), closed milestones, merged PRs from the last 14
  days, open PRs with the head commit's check rollup, and issues closed in the last 30 days. One
  more call per extra 100 open issues (capped at 5 pages). Read-only.
- Open RFC/epic issues: title starts with RFC / Programme / Epic / North star / Product
  direction, or label `epic` / `rfc` / `current-critical`, or the body has the
  `<summary>Original` fold. Each shows the first heading plus first paragraph above the fold,
  capped at 360 characters.
- Landed milestones: closed GitHub milestones (the repo has none today) plus RFC/epic issues
  closed in the last 30 days.
- Bounded (25 epics, 40 merged PRs, 30 open PRs, 20 landed items), path-free (local paths are
  redacted, links reduced to their text), deterministic (sorted by number or date then number;
  `--as-of` pins the window). Output has no em dashes: issue headings are normalized.
- `scripts/test-status-page.py` covers status extraction, epic classification, redaction,
  ordering stability under shuffled input, and bounds. Wired into `ci.yml` beside the other
  script tests.

First run (2026-09-23): 17 epics of 185 open issues, 68 PRs merged in 14 days, 24 open PRs
(5 failing CI), 1 landed epic. Six of the 17 epics (the M2 to M5 `current-critical` children)
have no status paragraph, only "Parent: #..." metadata, which is itself a signal for the
issue-body rule above.

Generate STATUS.md on demand or from a scheduled workflow that opens a PR; do not commit it on
every merge.
