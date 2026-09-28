# Protected cache replacement equivalence

The protected cache-generation catalog cannot treat “a command exited successfully” or “a target
directory exists” as proof that one reconstructed generation may replace another. Replacement
equivalence needs a closed, path-free identity contract before a physical producer can be reviewed.

`protected_cache_replacement_equivalence` defines that first contract for `cargo_target_v1`. One
canonical receipt binds:

- the protected namespace, cache-state identity, and newly materialized generation identity;
- the exact repository, commit, and Git tree;
- a canonical reconstruction-input digest;
- exact reconstruction-plan, validator, and toolchain-envelope generation digests;
- the declared Cargo-target family semantic digest; and
- the fixed `equivalent` outcome vocabulary.

All fields participate in exact correlation. The codec is bounded, rejects unknown or noncanonical
JSON, and carries no filesystem paths, command arguments, output, logs, environment, credentials,
or lease data.

## Authority boundary

Decoding a document reports `supplied_receipt_only`. Even an exact expected-field correlation
returns only `exact_supplied_receipt`. Neither result can:

- construct the protected catalog's current-transition authorization;
- adopt or publish a physical cache generation;
- prove a filesystem path, producer, validator execution, or successful personal-worker attempt;
- authorize cache reuse, reset, quarantine, eviction, deletion, or cleanup; or
- weaken catalog-wide recovery or namespace-wide lease vetoes.

There is intentionally no public or crate-wide success constructor. A later physical producer must
materialize a fresh candidate, derive its output identity, run the exact validator under the bound
source/plan/toolchain inputs, durably retain the receipt, and then transform that proof through a
separately reviewed crate-sealed correlation boundary. Caller-authored JSON never becomes that
authority.

## Remaining gates

This vocabulary completes only the schema/equality portion of the replacement-equivalence gate.
Current Big Red Cargo targets remain unmanaged and unknown. Before any generation can be current,
reused, or reclaimed, Glaeda still needs:

1. the repaired and independently accepted protected-store transition from PR #884;
2. a descriptor-bound physical reconstruction and semantic-validation producer;
3. fresh namespace-wide personal-worker lease visibility;
4. durable receipt persistence and recovery binding; and
5. a read-only adapter joining catalog, equivalence, lease, and live lock/mount/open/process evidence
   into cache inventory.

Missing or conflicting evidence remains a cold reconstruction or `unknown`, never an optimistic hit.
