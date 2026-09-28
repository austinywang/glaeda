# Owner-local semantic API v1

Issue #1012 defines the Glaeda-owned semantic seam used by direct owner/chat/CLI callers. Transport
authentication stays outside this contract. Physical execution stays inside the existing Glaeda
admission and owned-Linux task path.

```text
caller authentication / transport
            |
            v
glaeda-semantic-request/v1
            |
            v
reviewed Glaeda operation adapter
            |
            v
#1050 reviewed workload adapter (for source execution)
            |
            v
Glaeda source / admission / capability checks
            |
            v
existing owned execution primitive
            |
            v
glaeda-semantic-receipt/v1
```

The implementation is `scripts/provider_neutral_request.py`. It is deliberately pure: decoding,
semantic compilation, replay identity and receipt projection only. It performs no Git, filesystem,
network, process, scheduler, credential or host mutation.

## First operations

The v1 vocabulary is closed:

| Semantic operation | Existing Glaeda owner | Effect class |
| --- | --- | --- |
| `capabilities` | semantic contract | read-only |
| `status` | `owned_linux_admission.observe()` | read-only |
| `repo_query` | `repo-query/v1` | read-only bounded Git observation |
| `verify_named` | `verify-focused/v1` + owned admission + `owned_linux_task` | bounded source execution |

`command_argv` is absent. Glaeda does not yet own a reviewed general command profile that satisfies
the requested executable/cwd/resource/network/environment/source/replay boundary. The Git transport
may parse an argv-shaped future request, but semantic v1 cannot compile it into execution authority.

The provider-neutral request is canonical JSON:

```json
{
  "document_type": "glaeda-semantic-request",
  "schema_version": 1,
  "request_id": "owner-1012-request-0001",
  "operation": "verify_named",
  "source": {
    "repository": "teamleaderleo/glaeda",
    "commit": "<exact 40-hex commit>",
    "tree": "<exact 40-hex tree>"
  },
  "parameters": {
    "profile_id": "verify-focused/v1"
  }
}
```

`capabilities` and `status` carry `source: null` and empty parameters. `repo_query` carries the
same exact source plus an exact base commit and a patch inclusion ceiling capped at 8 KiB.

## Identity and replay

`request_id` is the explicit Glaeda request identity. It is separate from a Git branch/ref, issue,
chat turn, Stensibly work item, CMUX work ref, MCP call id or CLI process.

The canonical request bytes also produce `request_sha256`.

Rules:

- same `request_id` + same canonical semantics may replay one existing receipt;
- same `request_id` + different source/profile semantics is `request_conflict` for source-executing work;
- different request ids remain different physical work even when source/profile bytes happen to
  match;
- two transports share physical work only when both deliberately carry the same accepted Glaeda
  `request_id` and the same request digest.

For `verify_named`, Glaeda durably binds the accepted semantic request id to the resolved physical
command fingerprint, exact source, and profile in the shared provider-neutral verifier state **before**
returning an admission wait. The verifier repeats that binding check at its launch boundary. Exact
replay keeps the binding; drift fails closed. The direct Git bridge and owner-local adapter use the
same verifier state family, so an intentionally shared accepted request identity converges on the
same physical receipt instead of relying on transport-local inference.

Read-only `capabilities`, `status`, and `repo_query` remain repeatable observations and do not create
a physical execution binding. Their receipts still carry request identity/digest for client-side
correlation and replay validation.

For `verify_named`, the semantic request compiles through #1050's reviewed
`verify-focused/v1` adapter. The accepted semantic request identity is included in the verifier
command fingerprint. Transport-only correlation fields are excluded.

Legacy #1050 callers that omit `semantic_request_id` retain the pre-#1012 fingerprint contract.
This keeps the current CMUX fixture/integration byte-compatible while reviewed callers migrate onto
the explicit accepted identity. New #1012 callers always provide one.

This corrects an easy failure mode in caller-specific adapters: equality of source/profile bytes is
insufficient evidence that two callers meant one physical attempt.

## Operation mapping

### capabilities

Returns `glaeda-semantic-capabilities/v1`: the closed operation vocabulary, contract generation,
reviewed profile names, and the explicit fact that `command_argv` is unavailable.

This is semantic capability discovery. Current-node availability belongs to `status`.

### status

A host adapter asks the existing owned-Linux admission observer and wraps only its bounded
`glaeda-owned-admission-observation/v1` result. The remote semantic request contains no admission
root, host path, cgroup value, process identity or policy override.

Held, draining, pressured, reserved or unavailable states remain Glaeda observations. The status
receipt grants no execution or redispatch authority.

### repo_query

The semantic request names exact repository/base/head/tree identities and a bounded patch ceiling.
A host adapter invokes the existing fixed `glaeda-repo-query` / `repo-query/v1` implementation
against its operator-owned resident checkout mapping.

The local adapter maps semantic `owner/repo` identity into repo-query's canonical
`github.com/owner/repo` project identity. The semantic layer resolves the reviewed
`repo-query/v1` generation and independently derives the producer request digest from the exact
project/base/head/tree/patch ceiling plus the fixed no-auxiliary-query defaults. It accepts only a
matching typed report:

- `document_type=glaeda-resident-repo-query`;
- exact current `repo-query/v1` profile generation;
- exact producer request digest;
- `authority=observation_only`;
- exact canonical project plus requested base/head/tree;
- the closed no-grep/no-blob/no-history/no-object request family;
- bounded typed diff/patch/metrics evidence.

The remote receipt contains a closed semantic projection of that report. Producer-only fields and
unexpected additions cannot flow through the API. No Git argv, ref, fetch, remote URL or checkout
path enters the semantic request.

### verify_named

v1 accepts only `verify-focused/v1`. Glaeda resolves the exact profile generation, fixed resource
class, network-none/minimal-environment policy, deadline and output ceiling.

The existing #1050 adapter remains the workload compiler and the existing verifier remains the
executor. A physical adapter still supplies its own reviewed, operator-installed
repository/state/tool/admission bindings, revalidates exact source locally, and enters the same
owned-Linux admission/task boundary used by other callers.

The semantic receipt projects only bounded content-free terminal evidence from the existing typed
verification receipt: terminal class, physical start/settle times, output digest/byte count, process
settlement, cleanup and the exact workload receipt digest.

## Receipt contract

Every operation returns `glaeda-semantic-receipt/v1` with:

```text
request_id
request_sha256
operation
exact source or null
state
resolved operation/profile
bounded typed result or digest
refusal code where applicable
authority = all false
```

States are closed to:

```text
planned
waiting
refused
ambiguous
succeeded
failed
timed_out
cleanup_incomplete
```

`receipt.inspect` is the thin-connector name for validating this closed receipt. The implementation
is `inspect_receipt()`; it validates the operation-specific resolved contract and typed result,
recomputes the result digest, and recomputes the semantic request digest whenever the receipt carries
enough accepted semantics to do so. Inspection grants no replay, cleanup or execution authority.

## Direct local adapter

`scripts/owner_local_dispatch.py` is the local owner/CLI front door. It reads one canonical
`glaeda-semantic-request/v1` from stdin and returns one semantic receipt. It accepts no cwd,
executable, host path, environment, credential, mount, cgroup, sudo or shell field.

The fixed local installation file is
`~/.config/glaeda/owner-local-v1.json`, mode `0600`, owned by the local account. It binds reviewed
repository identities to resident checkout paths. The query executable is resolved by Glaeda at
the fixed owner install path `~/.local/bin/glaeda-repo-query`; the installation file cannot select
an executable. Repository bindings are machine-owner configuration and never appear in the remote
request.

Example local installation shape:

```json
{
  "document_type": "glaeda-owner-local-installation",
  "schema_version": 1,
  "repositories": [
    {
      "repository": "teamleaderleo/glaeda",
      "checkout": "/operator/resident/glaeda"
    }
  ]
}
```

Operation behavior is composition only:

- `capabilities` returns from the pure semantic contract and needs no installation file;
- `status` invokes the existing read-only owned-admission observer and needs no repository binding;
- `repo_query` invokes only the installed `glaeda-repo-query` with fixed argv derived from the
  semantic source/base/patch ceiling and the local repository binding;
- `verify_named` first durably binds the accepted semantic request identity, pre-observes owned
  admission, then invokes the existing `verify-focused` front door with the same semantic id,
  Glaeda-resolved profile generation and command fingerprint.

New accepted Git requests and local requests use the same fixed
`provider-neutral-verify-v1` verifier state family. Sharing the same accepted semantic request id
therefore reaches the same verifier receipt/intent identity; separate request ids remain separate
physical work. Legacy pre-#967 Git launch journals keep their old state family for reconcile-only
compatibility.

A future MCP/connector adapter only authenticates the owner, serializes this semantic request, sends
it to the fixed local front door, and returns the receipt. It gains no local path or process API.

## Transport adapters

A transport compiles into the same semantic request and returns the same semantic receipt.

Future direct connector/MCP names map as:

```text
local.capabilities -> capabilities
local.status       -> status
local.query        -> repo_query
local.verify       -> verify_named
receipt.inspect    -> inspect semantic receipt
```

The connector owns authentication and transport only. It contains no SSH client, remote shell,
raw filesystem API, resource scheduler or cache authority.

The trusted Git lifecycle in #1054 validates transport-specific lifetime/provenance first, then
mints one accepted semantic request identity and compiles a `glaeda-semantic-request/v1` through
this module. Source execution then flows through #1050 before physical admission. Existing Git
request IDs remain caller correlation; they do not become physical identity. GitHub Actions, CLI,
direct connectors and Stensibly can compile into the same provider-neutral request family.
A transport-specific request/result journal stays transport evidence; physical attempt truth remains
with Glaeda.

## Availability behavior

| Condition | Semantic behavior |
| --- | --- |
| Stensibly unavailable | direct Glaeda semantics are unchanged |
| Convex unavailable | direct Glaeda semantics are unchanged |
| GitHub unavailable | Git rendezvous pauses; CLI/direct connector can still operate |
| connector unavailable | Git rendezvous can still operate |
| node held | status reports wait/refusal; fresh verification does not launch |
| node draining | status reports wait; fresh verification does not launch |
| node pressured/capacity-limited | status reports wait; fresh verification does not launch |
| node offline | Git request stays durable/pending; direct connector/CLI reports transport unavailability; no physical state transition is inferred |
| requested resident source unavailable/cold | local adapter refuses without fetching, cloning or selecting another checkout |
| duplicate exact source-executing request | shared semantic binding + verifier state replay the matching physical receipt |
| source-executing request id reused with drifted semantics | refuse `request_conflict` before a fresh launch |
| surviving intent without terminal truth | return `ambiguous`; never infer permission to redispatch |

Transport loss never changes the physical admission state. A transport retry cannot create a second
uncertain attempt.

## Physical dogfood

The direct Git transport has already demonstrated named focused verification through the existing
Glaeda verifier without a GitHub Actions allocation. #1054 and the #967 dispatch integration own
the authenticated Git lifecycle above this semantic seam; the local adapter now exposes the same
semantic operation family without GitHub. The next same-semantics comparison should carry one
explicit accepted semantic `request_id` through:

1. direct Git/owner-local dispatch;
2. GitHub Actions using the same named verification operation and exact source;
3. receipt comparison for source/profile/physical admission family;
4. confirmation that the direct request has no Actions run/job identity.

Record request-to-admitted, admitted-to-first-command, useful-result, result-return/publication,
transport/API overhead, execution overhead and cleanup separately.

This lane remains independent from adaptive placement and automatic workload selection.
