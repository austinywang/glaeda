# GitHub resident-node advisory snapshots

Status: v1 bounded read-only contract for issue #970. This surface publishes routing/status evidence only. Glaeda performs fresh local source, profile, admission, reuse, ownership, and execution checks after any dispatch request.

## Consumer question

A GitHub-connected consumer should be able to answer, with one status read plus one slowly changing trust read:

- which reviewed Glaeda nodes have fresh evidence;
- OS/architecture class and exact Glaeda generation;
- which reviewed workload/profile generations are advertised;
- coarse capacity/pressure and a bounded active-work count;
- which explicitly allowlisted repositories and exact Git objects are resident;
- whether project state is `cold`, `warm`, `resident_hot`, or `revalidation_required`;
- whether a related request is `queued`, `preparing`, `running`, `terminal`, or `superseded`;
- the bounded receipt reference for terminal/recent-compatible work where one exists.

Every positive answer is advisory. `available`, `resident_hot`, a recent receipt, or a running request creates zero execution, dispatch, host-selection, result, cleanup, merge, ownership, or reuse authority.

## Chosen GitHub publication model

Use one bounded status branch in the existing Glaeda repository:

```text
main
  config/github-resident-snapshot-trust.json

glaeda-status/v1
  resident-nodes.json
```

`main` owns the reviewed repository allowlist and node public keys. `glaeda-status/v1` contains only the current aggregate fleet snapshot. Each node entry is independently signed with a dedicated node-local Ed25519 SSH signing key, so a writer can merge concurrent node entries without gaining authority to manufacture another node's state.

This model was selected from the issue #970 candidates:

| Model | Read cost | Writer/concurrency behavior | v1 disposition |
| --- | --- | --- | --- |
| Dedicated status repository | extra repository/trust bootstrap plus a second repository read | clean isolation | defer until the extra repository earns measurable value |
| Bounded status ref/file in Glaeda | trust on `main`, one small current fleet file, no source-branch churn | optimistic non-force push with independent signed node entries | **selected** |
| Dispatch/request surface | can reuse request transport | mixes advisory fleet evidence with mutation/request lifecycle | keep separate from #967/#1050 request authority |

The status branch is replaceable convenience state. Glaeda execution truth remains in the owning local/request/result contracts.

## Reviewed trust document

`config/github-resident-snapshot-trust.json` is small and changes through ordinary reviewed repository changes:

```json
{
  "document_type": "glaeda-github-resident-trust",
  "schema_version": 1,
  "repositories": ["teamleaderleo/glaeda"],
  "nodes": [
    {
      "id": "node-0123456789abcdef",
      "key_id": "key-0123456789abcdef",
      "ssh_public_key": "ssh-ed25519 AAAA...",
      "os_class": "linux",
      "architecture_class": "x86_64"
    }
  ]
}
```

The committed v1 file intentionally enrolls zero physical nodes. Enrolling a node requires a reviewed opaque node ID plus its dedicated public signing key. The private key stays on the producer and is never stored in the repository, snapshot, receipt, logs, request input, or consumer view.

Trust-set changes are also reviewed lifecycle events:

- removing a node from the trust document makes its old status entry retired convenience state; the next successful publisher prunes that entry;
- rotating a node key does not block unrelated reviewed nodes from publishing. The old-key entry stays non-authoritative and consumers report that node as `unknown/untrusted` until it republishes;
- the first snapshot under a newly reviewed key must restart `snapshot_sequence` at 1 before it can replace the old-key entry;
- malformed or signature-invalid entries that claim the *current* reviewed key still fail closed and block publication rather than being silently laundered through a trust transition.

A trust change never promotes historical bytes to positive evidence. Current-key signature verification remains mandatory before a node can become fresh again.

Resource safety is byte-bounded, not cardinality-bounded. The schema has no fixed maximum node, repository, profile, request, project, or reusable-state count; collections may grow while the containing canonical document remains inside its reviewed byte budget.

- public node IDs are opaque `node-` plus 16 lowercase hex characters;
- key IDs are opaque `key-` plus 16 lowercase hex characters;
- only Ed25519 OpenSSH public keys are accepted.

## Signed node schema

Canonical JSON uses sorted keys, compact separators, UTF-8, and one trailing newline. V1 applies byte budgets to signed node and aggregate fleet documents so malformed or accidentally enormous inputs stay bounded; those budgets do not encode a fleet-size policy. A 14-node fleet is an ordinary accepted case when its canonical bytes fit the aggregate budget.

```json
{
  "document_type": "glaeda-github-resident-node-snapshot",
  "schema_version": 1,
  "payload": {
    "authority": {
      "advisory_only": true,
      "authorizes_dispatch": false,
      "authorizes_execution": false,
      "authorizes_host_selection": false,
      "authorizes_cleanup": false
    },
    "freshness": {
      "snapshot_sequence": 42,
      "observed_at": "2026-09-21T10:00:00.000Z",
      "published_at": "2026-09-21T10:00:01.000Z",
      "producer_generation": 7,
      "maximum_useful_age_seconds": 300
    },
    "producer": {
      "glaeda_generation": "sha256:...",
      "key_id": "key-0123456789abcdef"
    },
    "node": {
      "id": "node-0123456789abcdef",
      "os_class": "linux",
      "architecture_class": "x86_64",
      "glaeda_node_generation": 4,
      "availability_class": "available",
      "pressure_class": "low",
      "capacity_class": "available",
      "active_work_count": 0
    },
    "profiles": [
      {
        "id": "verify-focused/v1",
        "class": "verify_focused",
        "generation": "sha256:..."
      }
    ],
    "projects": [
      {
        "repository": "teamleaderleo/glaeda",
        "source": {
          "commit_oid": "0123456789012345678901234567890123456789",
          "tree_oid": "0123456789012345678901234567890123456789"
        },
        "heat_class": "resident_hot",
        "verification_profiles": ["glaeda.required", "verify-focused/v1"],
        "dependency_build_state_class": "resident_generation",
        "reusable_states": [
          {
            "schema_version": 1,
            "cache_class": "incremental_build_state",
            "generation": "sha256:...",
            "heat": "hot",
            "size": "medium",
            "recent_hit": "within_hour",
            "revalidation_required": false
          }
        ],
        "active_task_count": 0,
        "recent_compatible_receipt_ref": "sha256:..."
      }
    ],
    "requests": [
      {
        "request_id": "req-0123456789abcdef",
        "source": {
          "repository": "teamleaderleo/glaeda",
          "commit_oid": "0123456789012345678901234567890123456789",
          "tree_oid": "0123456789012345678901234567890123456789"
        },
        "profile": "verify-focused/v1",
        "state": "running",
        "terminal_receipt_ref": null,
        "elapsed_class": "10s_to_1m",
        "estimate_class": "under_10s"
      }
    ]
  },
  "signature": {
    "algorithm": "sshsig-ed25519",
    "key_id": "key-0123456789abcdef",
    "namespace": "glaeda-github-resident-node-snapshot-v1",
    "value": "-----BEGIN SSH SIGNATURE-----\n..."
  }
}
```

### Node classes

The current owned-Linux admission observer maps to remote classes without publishing host measurements:

| Local bounded observation | Availability | Pressure | Capacity | Active count |
| --- | --- | --- | --- | ---: |
| `ready / compatible` | `available` | `low` | `available` | 0 |
| `wait / reserved` | `available` | `unknown` | `reserved` | 1 |
| `wait / node_held` | `held` | `unknown` | `unknown` | 0 |
| `wait / node_draining` | `draining` | `unknown` | `unknown` | 0 |
| `wait / pressure_high` | `available` | `high` | `insufficient` | 0 |
| `wait / capacity_unavailable` | `available` | `unknown` | `insufficient` | 0 |
| `refused / observation_unavailable` | `unknown` | `unknown` | `unknown` | 0 |

CPU, memory, PSI, process, and reservation internals remain local. The remote consumer sees classes and a count only.

### Project classes

Projects must appear in the reviewed repository allowlist. Exact commit/tree objects may be published. The v1 heat classes are:

```text
cold
warm
resident_hot
revalidation_required
```

The existing `glaeda-owned-workstation-capability/v1` maps `resident_hot` directly and maps `resident_cold` to `cold`. A narrower local producer can supply `warm` or `revalidation_required` through the explicit project-state input after making that classification locally.

`dependency_build_state_class` is one of:

```text
cold
warm
resident_generation
revalidation_required
unknown
```

Each project can publish exact `ReusableStateStatusSummary` values from the accepted #21 lifecycle for as many generations as fit the signed document's byte budget. The snapshot keeps only the lifecycle's agent-facing fields:

```text
cache_class
generation
heat
size
recent_hit
revalidation_required
```

The closed cache-class vocabulary keeps package-manager state, compiler caches, incremental build state, immutable compiled products, container layers, prepared dependency generations, and approved project-local hot state distinct. `generation` is one canonical SHA-256 identity. Heat is `cold | warm | hot`; size and recent-hit values use the bounded #1066 classes. A revalidation-required summary must report cold heat.

These summaries are advisory routing evidence for #546/#970. They grant no reuse, publication, validation, retention, cleanup, or execution authority.

No dependency names, cache contents, cache paths, dirty filenames, package inventory, or local-only source bytes enter the projection.

### Request classes

Request IDs are opaque (`req-` plus 16–64 lowercase hex characters). Source is exact repository/commit/tree. `profile` must name one advertised reviewed profile generation. States are:

```text
queued
preparing
running
terminal
superseded
```

A terminal request requires a `sha256:` receipt reference. Other states carry no terminal receipt. Elapsed/estimate evidence is intentionally coarse:

```text
under_10s
10s_to_1m
1m_to_5m
over_5m
unknown
```

Use `unknown` unless an accepted #21-style observation supports the class. The current #1050/#1051 external request adapter can later feed this projection after it lands; v1 introduces no mailbox, retry loop, queue, or execution ledger.

## Freshness and rollback rules

Freshness uses the oldest observation contributing to a snapshot. Republishing an old project capability never refreshes its `observed_at`.

Rules:

- `snapshot_sequence` starts at 1 and advances exactly by one inside one producer generation;
- `producer_generation` increases after producer replacement/reinstallation/reboot policy and a new generation restarts sequence at 1;
- lower `(producer_generation, snapshot_sequence)` cannot replace a higher one through the publisher;
- equal version + equal bytes is idempotent;
- equal version + different bytes refuses;
- `published_at` cannot precede `observed_at`;
- timestamps more than 30 seconds in the future refuse;
- `maximum_useful_age_seconds` is 60–600 seconds, default 300;
- age is calculated from `observed_at`, so delayed GitHub publication can arrive already stale;
- signature failure, malformed fields, missing node entry, duplicate node entry, stale data, or future timestamps produce an `unknown` consumer view for that reviewed node.

The sequence advances on publication, not on attempt. `publish` checks `sequence == current + 1` before
it checks refresh suppression, so a suppressed attempt refuses without changing remote state and the
caller must reuse the same sequence on its next attempt. A caller that increments per attempt refuses
forever after its first idle refresh, and the only escape is a new `producer_generation`, which is
reserved for reboot/reinstallation. Persist the last sequence the caller actually published, not the
last one it tried.

A dead/sleeping node can therefore remain remotely `available` only until its bounded useful age expires. Dispatch still performs fresh local admission immediately before physical execution.

## Publication triggers and write ceiling

Publish immediately for meaningful semantic transitions:

- node hold/drain/availability class change;
- pressure/capacity class change;
- active-work count change;
- producer/Glaeda/node generation change;
- profile generation change;
- project source/heat/build-state/receipt change;
- reusable-state generation/heat/size/recent-hit/revalidation change;
- request queued/preparing/running/terminal/superseded transition.

For unchanged semantics, suppress the write until the 240-second default refresh interval. The publisher requires at least a 30-second stale margin between refresh interval and maximum useful age. This keeps CPU-percentage fluctuations and raw host telemetry out of GitHub. With the default, one continuously idle node performs at most 15 successful refresh writes/hour. A suppressed attempt performs a read and zero writes.

## Concurrent Git publication

The publisher uses the fixed branch `glaeda-status/v1` and file `resident-nodes.json`.

For each attempt:

1. fetch the current status ref;
2. validate the bounded fleet and every existing node signature;
3. merge exactly one candidate node entry;
4. create a Git blob/tree/commit containing only `resident-nodes.json`;
5. push the new commit without force;
6. on a non-fast-forward race, refetch, revalidate, remerge, and retry a bounded number of times.

Separate nodes can therefore publish concurrently without a shared per-node filename race or one node rewriting another node's signed payload. The v1 publisher performs no automatic routing or physical execution.

Normal successful update cost: 2 network round trips (fetch + push). Suppressed unchanged update: 1 fetch. One compare-and-swap retry adds 2 round trips.

## Producer inputs

The script composes from already bounded local evidence:

```text
glaeda-owned-workstation-capability/v1
+ glaeda-owned-admission-observation/v1
+ optional glaeda-github-project-heat-input/v1
+ optional glaeda-github-request-state-input/v1
-> signed glaeda-github-resident-node-snapshot/v1
```

The optional project input carries only the project fields present in the public schema, including optional bounded reusable-state summaries that exactly match #1066's `ReusableStateStatusSummary` JSON contract. The optional request input carries only request ID, exact source, advertised profile, state, optional terminal receipt, and bounded duration classes. Unknown fields refuse.

If request state claims `preparing` or `running`, the local admission evidence must show at least as much active work. Published project active-task counts also cannot exceed the local bounded active-work count.

## Commands

Compose and sign one entry:

```text
python3 scripts/github_resident_snapshot.py compose \
  --capability <bounded-capability.json> \
  --admission <bounded-admission.json> \
  --trust config/github-resident-snapshot-trust.json \
  --private-key <dedicated-node-signing-key> \
  --public-node-id node-0123456789abcdef \
  --producer-generation 1 \
  --snapshot-sequence 1
```

Publish it through the current repository remote:

```text
python3 scripts/github_resident_snapshot.py publish \
  --snapshot <signed-node.json> \
  --trust config/github-resident-snapshot-trust.json \
  --repository-root <glaeda-worktree>
```

Turn a downloaded fleet file into the small agent view:

```text
python3 scripts/github_resident_snapshot.py consume \
  --fleet <resident-nodes.json> \
  --trust config/github-resident-snapshot-trust.json
```

The consumer verifies the reviewed node identity/class, exact schema, bounds, SSH signature, timestamps, and freshness before returning positive facts. A GitHub-connected agent needs two repository reads on first use: the trust file from `main` and the fleet file from `glaeda-status/v1`. The trust file changes slowly and can be cached by generation/commit, reducing steady-state target selection to one fleet read.

## Privacy ceiling

Snapshot bytes never contain:

- host/OS account usernames or private hostnames;
- IP or MAC addresses;
- filesystem paths;
- PIDs or process lists;
- commands/argv;
- environment values;
- credentials or signing private keys;
- dirty filenames or unpushed source contents;
- arbitrary package inventory;
- cache contents.

Canonical repository identity (including its GitHub owner component), exact Git object IDs, reviewed profile IDs/generations, opaque IDs, bounded classes/counts, timestamps, and digest references are the intended public vocabulary.

## Failure cases exercised by the contract suite

`python3 scripts/test-github-resident-snapshot.py -v` covers:

- a node dies after publishing `available`: expiry yields `unknown`;
- resident project heat disappears: next sequence is a semantic transition to `cold`;
- GitHub publication is delayed: age remains tied to local observation and can be stale on arrival;
- publication is lost: the previous state expires to `unknown` instead of claiming an unobserved terminal transition;
- an old producer/sequence attempts overwrite: publisher refuses;
- manual/forged payload edits: SSH signature failure yields `unknown`;
- Glaeda/node generation changes after reboot: producer generation restarts sequence at one and publishes a transition;
- two nodes publish concurrently: local bare-Git integration test runs concurrent publishers and verifies convergence;
- malformed reusable-state classes and duplicate class/generation identities refuse;
- reusable state requiring revalidation cannot claim warm/hot heat;
- request terminal state requires a bounded receipt reference;
- `running`/`preparing` request state cannot exceed locally observed active work.

## Measurements and experiment counters

Hosted contract run 35595612668 on exact head `52add98eca2e0fdcfe02a4a3cc97072f2b9213fd` measured a representative signed node carrying one reusable-state summary at **2,516 bytes** and the corresponding one-node fleet file at **2,594 bytes**. The current contract additionally exercises 14 nodes and 24 reusable generations without a cardinality rule; only serialized byte budgets bound those documents.

Transport/accounting properties are deterministic from the protocol:

```text
agent reads to select a target
  cold trust cache: 2
  warm trust cache: 1

successful publication
  fetch + push: 2 remote round trips

unchanged suppressed publication
  fetch only: 1 remote round trip, 0 writes

idle default write ceiling
  refresh every 240s: <= 15 writes/hour/node
```

Operational measurements that require real resident dispatch remain separate from the read-only implementation:

```text
stale-selection/refusal rate
duplicate dispatch avoided
dispatch-to-result improvement from visible hot state
```

Record those during an explicit #967/#1050-compatible dogfood loop using exact request/source/profile identity. This lane itself starts no workload and cannot manufacture those physical measurements.

## Consumer interpretation

A consumer can make statements such as:

```text
node-0123456789abcdef
  freshness: fresh / signature verified
  state: available
  pressure: low
  capacity: available
  Glaeda: sha256:...

teamleaderleo/glaeda
  exact source: <commit>/<tree>
  heat: resident_hot
  verify-focused/v1: generation sha256:...
  incremental_build_state: sha256:... hot medium within_hour

req-0123456789abcdef
  same source/profile: running
```

Then it can choose whether to submit an existing reviewed request contract. The node re-observes local truth before admission. If the project disappeared, the node went to sleep, capacity filled, the Glaeda generation changed, or local ownership/reuse became ambiguous, current local policy wins and remote evidence becomes a routing miss.
