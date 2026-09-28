# Protected cache namespace lease visibility

Protected Cargo-target generations are not independently leased today. Personal-worker jobs lease
one complete `RepositoryBuild` cache namespace. Until the durable worker schema deliberately gains
generation scope, any active lease on that namespace must veto every generation in its protected
catalog.

`protected_cache_namespace_lease_visibility` derives that conservative observation from the
existing config-bound read-only personal-worker store:

```text
validated current store snapshot
+ exact expected store revision
+ adapter-owned before/after capture timestamps
+ exact RepositoryBuild namespace whose digest matches the protected namespace
-> active | inactive_at_expected_revision | unknown
```

The personal-worker store already proves that every active reservation owns exactly one matching
durable cache lease and that conflicting leases within one exact namespace cannot coexist. The
adapter does not duplicate those rules. It counts exact read/write/exclusive leases and treats all
three as one namespace-wide veto.

The adapter owns the config-bound read and brackets it with wall-clock observations. That prevents a
caller from retaining an old opened document and presenting it later as a fresh inspection. Only a
matching current revision captured between nondecreasing clock observations with zero exact or
colliding leases reports `inactive_at_expected_revision`. The durable queue timestamp is reported
separately; it describes the last queue state transition and is not treated as snapshot age. The
following remain `unknown`:

- missing, unavailable, busy, unsafe, corrupt, future-version, recovery-required, or unsupported
  personal-worker store state;
- a store revision other than the exact expected revision;
- an unavailable capture clock or one that moves backwards around the read;
- another personal-worker namespace carrying the same protected namespace digest; or
- impossible lease-count overflow.

Both `active` and `unknown` require a conservative namespace veto. The inactive result merely avoids
that veto for the captured snapshot; it grants no positive authority.

## Authority boundary

The result has `read_only_store_snapshot_only` authority. The shared read lock used by the
config-bound store opener is released before the report can be consumed, so even an exact inactive
report is not mutation authority. It cannot:

- infer that one generation rather than the namespace is inactive;
- authorize protected catalog transition, cache reuse, reset, eviction, deletion, or cleanup;
- construct replacement-equivalence success or transition authorization; or
- replace the final fresh lease-store revalidation required by any future mutating composition.

Current Big Red Cargo targets remain unmanaged and unknown. The next read-only cache-inventory
adapter must join this result with a protected catalog snapshot, authenticated replacement
equivalence, and live lock/mount/open-file/process observations. Physical mutation remains a later,
separately reviewed boundary.
