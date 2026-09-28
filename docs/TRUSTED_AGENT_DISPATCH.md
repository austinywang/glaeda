# Trusted-agent dispatch v1

Issue #967 owns the Git-native transport and request lifecycle for one bounded request from a
GitHub-connected trusted caller to a resident Glaeda node. The physical executor stays the existing
reviewed Glaeda workload adapter.

The v1 contract deliberately admits one operation:

```text
verify_named -> verify-focused/v1
```

`repo_query` can join this grammar later by compiling to its existing reviewed adapter.
`command_argv` remains outside this contract until its own closed execution contract is reviewed.
No shell string, generic executable, caller path, environment, host selector, backend selector,
resource override or machine credential appears in v1.

## Ownership split

```text
GitHub transport owns
  immutable request publication and result projection
  outbound-only discovery
  GitHub transport provenance observation
  API/polling measurements

trusted-agent dispatch owns
  bounded request decoding
  request fingerprint and lifetime
  caller/provenance correlation
  exact source + named operation freeze
  transport-independent replay state
  one accepted provider-neutral semantic request identity

provider-neutral Glaeda semantic API owns
  cross-transport request identity/replay semantics
  reviewed operation vocabulary
  typed semantic receipt projection

existing Glaeda workload owns
  workload generation
  local admission and attempt identity
  backend/resource profile
  retained-state/cache eligibility
  process/task lifecycle and recovery
  physical terminal receipt
```

The request cannot grant itself execution authority. The transport adapter supplies independent
`ProvenanceEvidence`; v1 accepts only an exact principal + binding match. A Git commit's author or
committer strings are outside this decision. The first GitHub adapter can truthfully bind the
approved private mailbox writer set. Individual GitHub actor attribution requires a separately
reviewed GitHub API observation instead of reinterpreting author metadata.

## Request

Canonical JSON is capped at 4096 bytes and contains exactly:

```json
{
  "document_type": "glaeda-trusted-agent-dispatch-request",
  "schema_version": 1,
  "request_id": "dispatch-967-proof",
  "source": {
    "repository": "teamleaderleo/glaeda",
    "commit": "<exact 40-hex commit>",
    "tree": "<exact 40-hex tree>"
  },
  "operation": {
    "kind": "verify_named",
    "profile": "verify-focused/v1"
  },
  "caller": {
    "principal": "github-mailbox-writers:teamleaderleo/glaeda-dispatch",
    "provenance_binding": "github-private-repo-write-policy:teamleaderleo/glaeda-dispatch"
  },
  "created_at": "<UTC timestamp>",
  "expires_at": "<UTC timestamp, at most one hour later>",
  "supersession": {"policy": "none"},
  "request_fingerprint": "sha256:..."
}
```

The fingerprint is SHA-256 over the canonical document with `request_fingerprint` omitted. Exact
replay uses the full fingerprint. Reusing an ID with drifted bytes is a conflict. Future creation
time beyond five minutes, expired requests, malformed bytes, unknown fields, and any supersession
policy beyond `none` refuse before acceptance.

Branch or ref movement after publication changes discovery only. Acceptance binds the exact commit
and tree from the request document. Physical admission revalidates those exact source objects again.

## Semantic compilation

After provenance matches, v1 mints one deterministic accepted semantic request identity and
passes it through the caller-neutral #1050 adapter:

```text
trusted transport request + observed provenance
  -> accepted semantic request id
  -> glaeda-semantic-request/v1
       operation=verify_named
       request_id=<accepted id>
  -> #1050 glaeda-external-execution-request/v1
       semantic_request_id=<accepted id>
  -> operation=verify_focused
  -> requested_capability_class=credentialless_project
  -> existing verify-focused/v1 adapter
```

The caller's request ID and provenance remain dispatch identity. They still stay outside the physical
fingerprint. The accepted semantic request ID enters the reviewed workload fingerprint, so two
transport requests with equal source/profile bytes remain separate physical work by default. Exact
replay of one accepted dispatch keeps the same semantic request ID.

This preserves the cross-transport rule from #1012: physical work is shared only when callers
deliberately converge on one accepted provider-neutral request identity. The first trusted Git
adapter always mints a transport-scoped accepted identity; a later reviewed adapter can explicitly
import/share one provider-neutral identity when that is intended.

The accepted document records the semantic request ID, the exact semantic request digest, the
Glaeda-resolved workload ID/generation, and the resolved workload command fingerprint. The trusted
transport must carry that fingerprint unchanged into the verifier launch and recovery path. The
caller never supplies the workload generation or command fingerprint.

## Lifecycle and replay

The transport-independent lifecycle is:

```text
accepted
  -> launching
  -> terminal | refused | ambiguous
```

`launching` is a durable pre-launch marker, not proof that a process started. A transport/adapter
must persist it before entering the physical workload. Restart behavior is exact:

| Durable state | Restart disposition |
| --- | --- |
| `accepted` | persist `launching`, then one physical launch may be attempted |
| `launching` | reconcile-only through existing Glaeda durable truth |
| `ambiguous` | reconcile-only; no fresh attempt |
| `terminal` / `refused` | return the stored settlement |

This chooses duplicate avoidance over speculative retry. A crash after `launching` but before the
inner workload created its own intent can strand that request for operator/recovery handling; it
cannot silently become a second execution.

Terminal settlement accepts only a matching #1050 external receipt. Its canonical digest is stored;
replaying the exact receipt is idempotent, while a different terminal receipt conflicts.

## Bounded result

The public result projection is capped at 4096 bytes and includes:

```text
request ID + request fingerprint
accepted semantic request ID
exact source identity
named operation/profile
caller principal
terminal lifecycle class
SHA-256 of the matching external receipt
zero-authority flags
```

It contains no credentials, raw stdout/stderr, arbitrary logs, private paths, environment, PID,
machine secret, host telemetry or cleanup authority. The digest points back to Glaeda's bounded
physical receipt without copying its private implementation context.

## GitHub adapter

The proven `teamleaderleo/glaeda-dispatch` repository remains the smallest Git surface:

```text
refs/heads/glaeda/direct/request/<request-id>
refs/heads/glaeda/direct/result/<request-id>
```

Each request ref points at an immutable one-parent commit that adds one canonical request file. The
resident node observes refs outbound-only, fetches the exact object, rechecks ref identity, persists
request/fingerprint state, and then feeds the accepted semantic request into this contract. Existing
private request journals remain the durable transport bridge; the physical verifier remains the only
source-executing owner.

The deployed direct-v1 envelope carries older transport-policy boilerplate. The compatibility bridge
projects that envelope down to the fields above and calls `accept-projection`; the semantic
fingerprint is derived by this module, so those legacy execution-policy fields never enter the
accepted Glaeda identity. New direct terminal results publish that accepted semantic fingerprint in
`glaeda-github-direct-result/v2` while retaining the immutable transport request fingerprint.


The first provenance class is the deliberately tiny private repository writer set. The local
focused capability remains an independent action-time authorization boundary. The Git object carries
zero authority by itself.

GitHub loss pauses discovery and remote result publication. Once a request reached `launching`, local
Glaeda state remains authoritative and can settle/reconcile without converting publication loss into
another attempt. When GitHub returns, the adapter republishes/reconciles the same terminal identity.

## Measurements

For each physical or synthetic loop record:

- request publication -> node observation;
- observation -> admitted/start;
- execution duration;
- terminal receipt -> result visible to a GitHub connector;
- Git/GitHub polling/API calls;
- bytes transferred where the transport can measure them;
- duplicate executions avoided by exact replay;
- restart behavior before launch, after durable launch mark, and after terminal settlement.

Existing `glaeda-dispatch` evidence remains valid baseline data: immutable request/result refs,
response-loss recovery, request expiry/malformed/conflict refusal, resident polling, held-state
restart continuity, and warm observation/read optimizations. New measurements should preserve exact
source, dispatch, and resident-service generations.

## Scope boundary

This lane adds no heat-aware placement, adaptive routing, generic SSH, remote shell, second execution
engine, GitHub node snapshot mirror, merge authority, or alternate physical scheduler. #970 remains
advisory history; current local admission decides every launch.
