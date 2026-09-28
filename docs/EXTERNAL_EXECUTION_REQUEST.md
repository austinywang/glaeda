# External semantic execution request v1

Issue #1050 proves one caller-neutral boundary from an external work surface into an existing
Glaeda workload. The first mapping is the reviewed credentialless `verify-focused/v1` profile.
The external document carries semantic work and exact Git source identity; Glaeda derives the
workload generation, fixed execution profile, physical identity, admission inputs and attempt state.

This is an adapter contract, not a second scheduler, queue or recovery ledger.

## Exact request schema

Canonical JSON is bounded to 4096 bytes. Unknown fields fail closed.

```json
{
  "document_type": "glaeda-external-execution-request",
  "schema_version": 1,
  "external_request_ref": "cmux:exec:1050:fixture-1",
  "source": {
    "repository": "teamleaderleo/glaeda",
    "commit": "0409c2f4e82385d0770bb2b34f34fd3e6e2dbc36",
    "tree": "03613a93ce152aefddbb246613084705043b1397"
  },
  "operation": "verify_focused",
  "requested_capability_class": "credentialless_project",
  "reuse_hint": "prefer_valid_reuse",
  "correlation": {
    "work_ref": "cmux:work:1050"
  }
}
```

Required fields are:

- `external_request_ref`: bounded caller correlation text. It grants zero Glaeda authority.
- `source.repository`: canonical `owner/repository` identity.
- `source.commit` and `source.tree`: exact 40-hex Git object identities.
- `operation`: v1 admits only `verify_focused`.
- `requested_capability_class`: v1 admits only `credentialless_project`.

Optional fields are:

- `reuse_hint`: `no_preference` or `prefer_valid_reuse`. This is advisory only.
- `correlation.work_ref`: bounded caller-owned work identity used only in the external receipt.

The external request reference, caller work reference and reuse hint are deliberately absent from
the physical execution fingerprint. Changing those fields cannot mint another execution identity.
A caller-specific adapter may separately supply a reviewed accepted `semantic_request_id` to the
compiler. That identity is outside this external JSON document and deliberately partitions physical
replay so independent authenticated callers cannot silently alias one attempt.
The complete canonical external document is separately hashed as `request_sha256` so reuse of one
external request reference with drifted semantics is detectable.

## Exact receipt schema

Canonical JSON is bounded to 4096 bytes.

```json
{
  "document_type": "glaeda-external-execution-receipt",
  "schema_version": 1,
  "external_request_ref": "cmux:exec:1050:fixture-1",
  "request_sha256": "sha256:...",
  "correlation": {
    "work_ref": "cmux:work:1050"
  },
  "operation": "verify_focused",
  "source": {
    "repository": "teamleaderleo/glaeda",
    "commit": "...",
    "tree": "..."
  },
  "state": "planned",
  "resolved_workload": {
    "id": "verify-focused/v1",
    "generation": "sha256:...",
    "capability_class": "credentialless_project"
  },
  "workload_receipt_sha256": null,
  "refusal_code": null,
  "authority": {
    "authorizes_execution": false,
    "authorizes_redispatch": false,
    "authorizes_host_selection": false,
    "authorizes_cleanup": false
  }
}
```

The external state vocabulary for this slice is:

- `planned`: schema, semantic operation, capability and exact source identity compiled successfully.
  Physical admission and source revalidation are still pending.
- `refused`: the bounded adapter rejected the semantic request; `refusal_code` names the class.
- `ambiguous`: an execution may have started and durable truth cannot prove a terminal result.
  Redispatch authority remains false.
- `succeeded`, `failed`, `timed_out`, `cleanup_incomplete`: projected only from a matching,
  validated `verify-focused/v1` terminal receipt.

A terminal external receipt binds the SHA-256 of the exact canonical workload receipt. It does not
copy private command output, host state or internal attempt data.

## Mapping to `verify-focused/v1`

| External semantic field | Glaeda mapping |
| --- | --- |
| `source.repository` | existing `verify_focused_impl.Request.repository` |
| `source.commit` | existing exact commit identity |
| `source.tree` | existing exact tree identity |
| `operation=verify_focused` | resolves to Glaeda-owned `verify-focused/v1` |
| `requested_capability_class=credentialless_project` | must match the profile's reviewed execution identity |
| caller refs / reuse hint | correlation/advisory only; excluded from physical fingerprint |
| adapter-supplied accepted `semantic_request_id` | internal compiler input; included in physical fingerprint and absent from caller JSON |

Glaeda resolves the current `verify-focused/v1` generation by hashing the checked-in fixed profile
spec. The current profile owns its four-CPU / 8 GiB ceiling, task-private build state, read-only
source, network-none credentialless execution, fixed `scripts/verify focused` recipe, 600-second
deadline and bounded terminal receipt. None of those implementation fields enter the external
request.

The adapter derives an internal `command_fingerprint` from exact source, semantic operation,
capability class and the Glaeda-resolved workload id/generation under the domain
`glaeda-external-verify-focused-binding-v1`. A reviewed caller adapter may additionally bind one
accepted `semantic_request_id`; that identity is included in the fingerprint without widening the
external document schema.

Immediately before physical work, the existing verifier re-resolves the exact commit/tree and
canonical repository origin. The installed admission adapter independently observes current local
capacity/policy and reserves its fixed slot. The external caller cannot provide the admission root,
repository checkout, Cargo/rustup roots, machine, backend, cgroup/systemd settings or physical
attempt identity.

A #970-style node capability/heat snapshot can help a caller decide whether a request is worth
sending. It remains advisory. The local source and admission checks always decide whether work can
run.

## Replay, ambiguity and recovery

Four identities stay separate:

1. caller correlation: `external_request_ref` and optional `correlation.work_ref`;
2. external adapter document: `request_sha256` over the canonical external document;
3. adapter-owned accepted Glaeda identity: `semantic_request_id`, when the authenticated adapter supplies one;
4. Glaeda physical execution: the derived workload request/fingerprint plus Glaeda durable state.

Exact replay of an existing external receipt requires the same external request reference and the
same canonical request digest. Reusing the reference with drifted semantics is a conflict.

The existing verifier already supplies the physical no-duplicate rule. A matching durable terminal
receipt is returned without executing again. A durable intent without a valid terminal receipt is
classified as ambiguous and the verifier refuses redispatch. Recovery proceeds through its exact
local durable truth and fresh settlement observation.

## Deliberately excluded fields

The v1 external envelope excludes these caller-controlled facts:

- cwd, checkout path, project directory, workspace/window/pane/surface ids, presentation labels,
  focus, attention and return-placement policy;
- shell text, command strings, argv, executable names, scripts or generic process launch;
- environment variables, HOME/XDG values, credentials, SSH agents, tokens or publisher identity;
- arbitrary host paths, mounts, cache roots, state roots, admission roots, Cargo/rustup roots;
- machine/node/hostname, backend/provider, placement, CPU affinity or target selection;
- Glaeda profile id/generation, CPU/memory/PID values, deadlines, cgroup/systemd properties,
  process units, reservations or physical attempt ids;
- cache keys, resident-state generations, reuse leases, cleanup/recovery policy;
- raw logs/output, PIDs, process listings, host telemetry or heat snapshots;
- retry, redispatch, cleanup, preemption, supersession, merge or publication authority.

If a future request needs those fields merely to mirror an internal Glaeda workload manifest, the
adapter boundary has dropped too low and should stop there.

## Repository/GitHub-only synthetic transport proof

The repository contains a paired fixture under
`docs/experiments/external-execution-request/`:

```text
cmux-request.json
  -> scripts/external_execution_request.py plan
  -> glaeda-result.json
```

The request and result are ordinary bounded repository files that a GitHub-connected caller can
write/read without assuming a workstation is online. The result is `planned`, which proves decode,
validation, semantic mapping and bounded correlation without claiming physical admission.

Physical execution later uses the already-reviewed `verify-focused/v1` path and its durable replay
contract. No second mailbox lifecycle or external retry loop is introduced.
