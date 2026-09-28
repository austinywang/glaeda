# Repository agent review, repair, and research workloads

Status: first contract/trial slice for #770, #990, and #975.

Glaeda already has a workload-family-neutral `ComputeWorkloadIdentity`. This adapter defines three repository-agent operations on top of that seam without turning the compute runtime into an agent workflow engine:

- `review`: exact candidate/base source -> bounded findings;
- `repair`: exact source + bounded task-private edit authority -> exact candidate source + repository-owned verification receipts;
- `research`: exact source + bounded evidence/network class -> cited research artifact identity.

The caller continues to own the human work item, worker brief, responsibility, and continuation semantics. Glaeda receives the exact SHA-256 of that task contract rather than prompt/transcript text.

## Request contract

`scripts/repository_agent_work.py` accepts `document_type = glaeda-repository-agent-work-request` with `schema_version = 1`.

Every request binds:

```text
operation
exact repository + head commit/tree
optional exact comparison base commit/tree
task-contract SHA-256
repository scope
network class
mutation class
exact repository-owned verification profiles where applicable
```

The request intentionally has no:

```text
prompt or transcript body
model reasoning
shell / argv / cwd
host or backend selector
cache/state path
credential/environment values
physical attempt id
GitHub publication/merge authority
```

The task-contract digest lets Stensibly, CMUX, or another caller own the actual worker brief while Glaeda can still detect semantic drift and derive a stable workload input identity.

## Operation authority

### Review

Review v1 is read-only.

It requires an exact comparison base and admits `none` or `repository_only` networking. The normal scope is `changed_paths`; a bounded-path or whole-repository scope is also representable when deliberately requested.

A completed result contains at most 32 bounded findings. Each finding binds:

- class and severity;
- repository-relative path and optional line range;
- one-line bounded summary;
- evidence digest;
- optional suggested repository verification profile;
- a recomputed finding digest.

A review can recommend validation. It cannot request that verification execute as part of review authority.

### Repair

Repair v1 is the only mutating operation.

Mutation is limited to `task_private_patch` and requires :

- explicit bounded repository path prefixes;
- maximum changed-path count;
- maximum patch bytes;
- explicit new-file/delete allowance;
- `none` or `repository_only` networking;
- at least one exact repository-owned verification profile.

A completed repair receipt requires:

```text
exact resulting commit/tree, with a tree different from the requested head
clean task-private working copy
bounded per-path modified/added/deleted evidence entirely within scope
patch SHA-256 and byte count within the requested ceiling
new-file/delete statuses admitted by the request
exact requested verification set
every verification result names the exact resulting commit/tree
semantic result = passed for every required verification
```

For CMUX repairs, the first trial verification primitive is `cmux.ci.guard@1`; Mac/source repairs can later name `cmux.macos.dev-check@1`, `cmux.macos.compile-admission@1`, or the app-host shard where the task contract requires them.

The task-private patch mechanism and coding harness remain separately reviewed execution owners. This contract does not create a generic writable checkout API or arbitrary shell.

### Research

Research v1 is read-only.

It can use:

- `none`;
- `repository_only`;
- `public_web`.

A completed research result returns identities, not the full research body:

```text
answered | partial
research artifact SHA-256
citation-manifest SHA-256
citation count
evidence-source count
```

The caller can retain or publish the cited artifact under its own policy. The ordinary Glaeda receipt remains bounded.

## Generic compute projection

`plan` maps each accepted request to the existing generic workload vocabulary:

```text
family = repository_agent_work.v1
semantic_generation = 1
input_identity = sha256(canonical normalized request)
output_contract = operation-specific result-contract digest
```

Trust/capabilities are derived locally:

| Operation | Trust | Capabilities |
| --- | --- | --- |
| review | trusted | agent harness, repository read/query |
| repair | ultra-trusted | review capabilities + task-private write + repository-owned verification |
| research | trusted | agent harness, repository read/query; `network.public` only when declared |

The plan also emits a stable `routing_classification` for #546-style historical
placement evidence. It contains only:

```text
family + semantic generation
canonical repository
operation
trust class
network class
scope class
coarse scope-size class
mutation class
coarse mutation-budget class
required capabilities
exact repository-owned verification profile set
output-contract identity
```

The same canonical object is emitted with
`routing_classification_sha256 = sha256(canonical_json(routing_classification))`
so #546 consumers have one stable equality key.

It keeps the canonical repository/project identity while deliberately excluding
exact source commit/tree, exact bounded path names,
task-contract digest, and `compute_workload.input_identity`. Two review
requests against different commits therefore retain distinct exact compute
identities while sharing one routing population when their semantic work class
is otherwise the same. Repair requests use coarse path-count and patch-budget
classes, so a one-path/tiny repair can remain comparable across different files
while a 32-path/large-patch repair stays in a different routing population.
Different verification profiles or mutation semantics also remain separate
populations. A later placement adapter may add its reviewed symbolic resource
profile; this contract does not invent one.

`compute_workload.input_identity` remains the exact per-request semantic
currentness/replay key. It carries zero historical workload-class authority.

The plan grants zero host selection, publication, merge, release, deploy, redispatch, or canonical-source mutation authority.

## Receipt contract

`document_type = glaeda-repository-agent-work-receipt` with `schema_version = 1` binds:

- request digest;
- exact operation/source;
- terminal state: `completed | blocked | failed | cancelled | ambiguous`;
- the operation-specific bounded result only for `completed`;
- an all-false authority block.

A non-completed receipt carries no result payload. A successful repair candidate still grants zero branch push, PR creation, merge, release, or deploy authority.

## CMUX trial corpus

`docs/experiments/repository-agent-work/fixtures-v1.json` freezes nine real CMUX tasks: three of each operation.

### Review

1. CMUX #13411 workload/profile corpus: source identity, semantic receipts, benchmark comparability, repository command ownership.

2. CMUX #13399 fleet acceptance: recovery durability, role/profile ownership, path safety, operator runbook behavior.

3. CMUX #13383 persistent compile admission: source/toolchain exactness, cache-state disclosure, fallback, benchmark comparability.

### Repair

1. Accepted #13411 review finding: sanitize inherited Git redirection variables used by source identity checks.
2. Accepted #13399 review finding: make recovery/rollback commands usable from a fresh shell.
3. Accepted #13411 benchmark-state finding: advertise only warm states the repository entrypoint actually preserves.

Each repair fixture binds bounded paths and `cmux.ci.guard@1` as its exact verification requirement.

### Research

1. #13091: determine which changes invalidate resident-hot tagged developer builds and design generated benchmark fixtures.
2. #13198: define a controlled hosted-vs-persistent compile-admission benchmark across cold/dependency-warm/compiler-warm state.
3. #6134: identify the app-host shard/generated fixture best suited to machine acceptance and fleet benchmarking.

The fixtures use historical exact revisions on purpose: they are trial material with known engineering context, not the permanent benchmark-source corpus. Once trial behavior is understood, durable benchmark cases should move toward generated repository fixtures where source aging would otherwise dominate the experiment.

## Trial order

### 1. Review

Run the three review fixtures first. This exercises exact source materialization, repository evidence/query reuse, agent harness latency, and bounded finding receipts with zero mutation.

Measure:

```text
request -> workspace/evidence ready
request -> first finding
request -> terminal receipt
repository reads/query calls and result bytes
finding count and later disposition
operator intervention
```

### 2. Repair

Feed one accepted review finding into a fresh task-private repair workspace. The harness edits only within the admitted path scope, creates an exact candidate source, then requests the exact repository verification profiles from the task contract.

Measure:

```text
request -> workspace ready
request -> first edit
request -> first verification result
complete wall time
changed paths / patch bytes
verification profile timings
canonical source contamination = 0
sibling task contamination = 0
```

### 3. Research

Run the repository-only research fixtures first, then the public-web compile-admission fixture. Preserve a cited artifact separately and return only its digest/citation manifest in the Glaeda semantic receipt.

Measure:

```text
request -> first useful evidence
request -> cited artifact
repository query vs remote/public calls
worker-visible evidence bytes
citation/evidence-source counts
operator intervention
```

## Composition with existing work

This slice is intentionally transport/execution independent.

- #1054 can carry caller provenance, expiry, replay, and durable request lifecycle.
- #1065 can expose review/repair/research as future provider-neutral operations once its stack is accepted.
- `repo-query/v1` can supply exact local repository evidence to review/research.
- #990 can consume the repair contract when task-private source-development execution is connected.
- CMUX #13411 supplies repository-owned verification profiles for repair completion.
- #546 can later route these workload identities using measured queue/heat/resource evidence.
- #975 can compare model/context cost because the task contract and repository evidence are independently digest-bound.

No new scheduler, work ledger, build system, generic shell, or model provider API is introduced by this tranche.

## Local contract proof

```sh
python3 scripts/test-repository-agent-work.py
```

The tests cover operation authority, source/scope validation, task digest binding, path traversal/.git refusal, generic compute projection, repair verification, review finding identity, research citation identity, terminal receipt correlation, CLI round trips, and all nine CMUX fixtures.
