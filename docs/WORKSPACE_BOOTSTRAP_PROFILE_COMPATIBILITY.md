# Workspace bootstrap to verification-profile compatibility

## Boundary

`./scripts/bootstrap` remains the canonical repository-owned preflight from issue #117. It observes a checkout and emits repository readiness without claiming runner ownership.

The repository contains no identity-bearing compatibility command and accepts no `--runner-context` path. Repository code never opens an installation descriptor, runner receipt, workspace record, or cache identity source.

`scripts/workspace_bootstrap/profile_bridge.py` is a pure mapper. It accepts an already validated `ValidatedRunnerContext` from a Glaeda-owned adapter together with the repository receipt. It performs no file acquisition, JSON parsing, path selection, environment lookup, or readiness decision.

## Typed input boundary

The trusted producer supplies:

- exact installation ID and workspace ID;
- exact repository and private workspace root;
- exact cache ID, owner workspace ID, namespace digest, private cache path, and presence;
- a trusted evidence digest;
- descriptor-derived provenance facts for protected-state acquisition and independent filesystem observation.

The mapper emits either one bounded observation candidate or typed blockers. Its result deliberately contains no `ready` field. Only the Glaeda-owned adapter may construct the merged #153 Rust observation types and decide whether the mapped observation proceeds to profile preflight.

## Required trusted producer

A later Glaeda-owned adapter must:

1. open the installation descriptor through the canonical descriptor-relative state root;
2. resolve the enrolled workspace and cache from protected durable state;
3. bind installation ID, workspace ID, repository, workspace root, cache ID, namespace digest, and cache path from that state;
4. observe filesystem type, ownership, containment, and aliasing independently;
5. retain the opened descriptor while checking replacement and path-race identity;
6. reject writable or wrong-owner state, symlinked parents, hard links where relevant, replaced files, and path races;
7. pass the typed context directly to the pure mapper;
8. keep repository code unable to choose, rewrite, or reopen the identity source.

Pathname checks alone never establish this trust boundary.

## Pure mapping

For a validated context, the mapper produces:

- `workspace`: exact installation ID, workspace ID, repository, and clean state; the root remains private;
- `capabilities`: one flat list of `{ capability, available }` using `python3`, `git`, `cargo`, `rustc`, `rustfmt`, `clippy`, `nextest`, `just`, and `podman`;
- `resources`: non-null available memory and swap byte counts;
- `cache`: exact cache ID, owner workspace ID, namespace digest, and observed presence;
- `trusted_evidence_digest`: the digest supplied by the trusted producer;
- `observation_digest`: a digest of the bounded mapped observation.

Verification backend and formatter rows are checked against the same canonical capability IDs. Duplicate mappings fail.

Private workspace roots, cache paths, protected-state paths, device/inode identities, numeric owners, modes, and descriptor details remain outside the mapped observation.

## Provenance refusal contract

The mapper refuses typed contexts reporting:

- caller-selected JSON or repository-controlled identity sources;
- arbitrary identity values unbound from durable state;
- acquisition outside the canonical descriptor-relative state root;
- writable or wrong-owner protected-state parents;
- unsafe installation owner or mode;
- symlinked parents or filesystem aliases;
- hard-linked installation state;
- replaced installation files;
- descriptor/path identity races;
- unresolved workspace/cache durable-state bindings;
- missing independent filesystem observation;
- wrong filesystem type or owner;
- escaped or unproven cache containment;
- absent cache, unknown resources, dirty checkout, repository drift, or duplicate capability mappings.

These checks validate typed evidence supplied by the trusted adapter. They do not turn repository code into a trusted producer.

## Test boundary

The compatibility fixture constructs typed contexts directly for pure mapping tests. It proves refusal for forged receipts, arbitrary identities, writable parents, wrong owner and mode, symlinked parents, hard links, replaced files, path races, and an otherwise valid repository-created document. It also asserts that the removed compatibility command stays absent and that the mapper contains no context-file acquisition path.

Actual protected-state acquisition and race-resistant descriptor traversal belong to the later Glaeda-owned adapter and require their own integration tests.
