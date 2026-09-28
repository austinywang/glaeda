# Renderprove vision packet profile

Glaeda's first Renderprove vision profile is a pure credentialless contract for the merged `renderprove.vision-check.v1` dry-run command. It does not execute a model and does not add a packet file format.

## Reviewed Renderprove contract

The profile binds:

- repository `teamleaderleo/renderprove`;
- one exact Renderprove commit and executable artifact digest;
- command ID `renderprove.vision-check.v1`;
- command-contract digest `8025df77cfcfd743f3007fd129239bed417ff2095b9f744d23bb7c0e5ec56e4f`;
- request schema `vision-request-v1`;
- prompt policy `vision-prompt-policy-v1`;
- canonicalization profile `rgba8-png-zlib9-v1`.

The Renderprove tool identity is separate from the project-owned repository command identity. `VerificationProfileContract` intentionally requires a repository command to belong to the project repository. This module does not weaken that rule or pretend an external tool is project-owned.

## Inputs

The contract accepts exactly:

1. one PNG screenshot;
2. one UTF-8 operator brief;
3. zero or one receipt-v1 document.

Each slot has one private normalized project-relative path, exact byte length, and SHA-256 identity. Public serialization omits the path. URL, stdin, absolute, traversal, backslash, control-character, repeated, empty, and oversized inputs fail closed.

JPEG, multi-image, OCR, crop or annotation, browser capture, repository scanning, inferred includes, and generic attachments remain separate contracts.

## Fixed plan

The typed plan generates only:

```text
renderprove vision-check . \
  --screenshot <private-slot> \
  --brief <private-slot> \
  [--receipt <private-slot>] \
  --dry-run \
  --json
```

The caller cannot append shell syntax, source include options, model selection, provider output, package installation, local commits, or publication flags through this type.

The first executor must resolve the `renderprove` program to one explicit absolute executable path before process creation. The plan records arguments only; it does not authorise PATH search or an implicit shell.

## Authority

`RenderproveVisionExecutionPolicy::credentialless_packet()` fixes:

- network denied;
- credentials absent;
- workspace read-only;
- local commits forbidden;
- publication forbidden.

A caller cannot construct a wider policy through this module. Resource and timeout values reuse Glaeda's existing validated profile types. The complete profile remains provider-free even when a later executor is added.

## Preview evidence

`RenderproveVisionPreviewEvidence` accepts one bounded public JSON preview and checks its identity-bearing fields against the reviewed tool contract:

- schema URI and version;
- dry-run mode;
- advisory authority;
- command ID and contract digest;
- prompt-policy identity;
- canonicalization-profile identity;
- one lowercase SHA-256 request digest.

Deserialization denies unknown top-level fields and rejects duplicate or missing top-level fields before the reviewed identities are compared. This validates the exact public envelope shape without claiming to reimplement Renderprove's complete nested JSON Schema.

The caller supplies the content digest of the exact retained preview bytes through Glaeda's existing artifact boundary. The source screenshot, canonical image bytes, brief contents, raw receipt, private paths, logs, environment, and credentials are not part of the public profile or preview evidence.

## Admission and terminal result

`RenderproveVisionTerminalEvidence` carries only typed terminal facts:

- the existing `ExecutionAdmissionIdentity`;
- exact tested-source, project-command, and Renderprove-tool identities;
- typed process and cleanup outcomes;
- zero or one already validated public preview.

`finalize_renderprove_vision_result` refuses profile, admission, source, command, or tool drift. A successful result requires a successful process, completed cleanup, and a validated preview. Missing output becomes `malformed_preview`; process, timeout, output-limit, identity, and cleanup failures retain stable codes without raw diagnostics.

The public result includes fixed evidence coverage. It identifies the public preview evidence as included or unavailable and declares screenshot pixels, brief contents, receipt contents, process output, environment, and credentials omitted. A failed packet process cannot claim a validated preview.

## Private packet boundary

Renderprove keeps canonical image bytes in an in-process private store and returns copies only through an integrity-checking accessor. Its dry-run CLI emits a public preview; it does not serialize a reusable private packet.

Glaeda must not invent a packet file, environment payload, cross-process byte bundle, or provider subprocess reconstruction. Provider execution remains blocked until Renderprove selects one live endpoint/output mode and defines a reviewed credential and network boundary. Glaeda must not claim packet/provider phase isolation unless it can enforce that boundary.

## Current scope

The modules contain pure identities, slot validation, fixed argv projection, preview identity validation, admission binding, and terminal result projection. They add no executor, process environment, persistence, provider transport, credential handling, CLI surface, or Glaeda manifest field.

A later issue may add an executor only after it specifies absolute executable resolution, empty environment construction, read-only workspace enforcement, bounded capture, cleanup, and durable receipt storage. Provider execution remains a separate gated contract.
