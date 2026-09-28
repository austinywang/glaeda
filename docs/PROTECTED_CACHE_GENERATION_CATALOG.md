# Protected cache-generation catalog

Glaeda needs catalog-wide generation authority before any hot Cargo target can become managed or
reclaimable. Independent per-directory records are insufficient: two different state IDs can both
claim `current` while each record remains internally valid.

`protected_cache_generation_catalog` defines the first bounded document contract for one reviewed
`cargo_target_v1` family:

- one canonical protected namespace identity;
- one positive catalog revision;
- one optional current-state pointer;
- bounded, path-free state and materialized-generation identities;
- mutually consistent `current`, `retired`, and `quarantined` lifecycles;
- explicit recovery debt that blocks correlation across the entire namespace;
- strict canonical JSON encoding and decoding;
- revision-checked, read-only state correlation.

Decoding bytes reports `supplied_document_only`: a JSON document cannot grant itself cache
ownership.

On Unix, `unix_protected_cache_generation_catalog_store` creates the empty revision-one document,
then may build a crate-sealed current-generation successor from the exact clean stored revision.
Every successor advances once, retires the former current generation, adds exactly one new current
identity, and preserves all other entries. The store retains no-follow-opened installation/store
descriptors, binds a canonical private envelope to the exact installation ID, current state-root
generation and namespace, validates `0750`/`0700`/`0600` ownership and modes, serializes operations
with a fresh descriptor on the persistent lock, synchronizes staged bytes, durably checkpoints the
stage directory entry before currentness replacement, publishes with atomic no-replace creation or
atomic replacement, and synchronizes the parent directory after publication.

Recovery accepts only an exact abandoned empty create, an exact one-revision current successor, or
one byte-identical duplicate stage. Missing predecessors, altered history, skipped revisions,
multiple stages, and malformed, mismatched, or conflicting stages remain recovery-required.

The public transition operation requires a sealed authorization type that has no production
constructor. A separately reviewed replacement-equivalence producer must be the first code allowed
to mint it. The store never accepts a caller-supplied catalog document for publication. A
protected-store snapshot is only persistence provenance: it grants no physical cache ownership.
There is still no path discovery, adoption, cache scan, public generation producer, lease inference,
reconstruction receipt, quarantine, restore, deletion, or CLI apply path.

## Required next authority

A physical producer remains blocked until separately reviewed slices supply all of the following:

1. a replacement-equivalence success receipt binding canonical reconstruction inputs, the exact
   plan/validator/toolchain generations, a newly materialized output identity, and the declared
   family semantic digest;
2. fresh personal-worker lease-store visibility. Until leases deliberately gain generation scope,
   any active lease on the protected namespace vetoes every generation; unreadable, corrupt,
   partially observed, or revision-mismatched lease state remains unknown;
3. a read-only adapter that joins the protected catalog, equivalence receipt, lease observation,
   and live lock/mount/open-file/process evidence into the existing pure cache inventory. Missing or
   conflicting evidence must remain `unknown`.

Current Big Red Cargo targets remain unmanaged/unknown. This document grants no mutation or cleanup
authority and does not change the existing `supplied_observation_only` cache report.
