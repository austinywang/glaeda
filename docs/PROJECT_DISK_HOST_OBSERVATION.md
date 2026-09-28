# Project-disk host observation

Status: #565 P2 read-only host observer

`project_disk_host_observation` encodes the Lima 2.2.0 standalone-disk schema physically established by the retained #634 operator-Mac fixture. It executes no `limactl` command and carries no disk mutation, P1 lease-transition, or project-filesystem proof authority.

## Accepted physical schema

The private implementation requires an exact private Lima home and an explicitly supplied disk directory. Every authoritative filesystem component is opened with descriptor/no-follow semantics and rebound before acceptance. The accepted direct-entry forms are:

- detached: exactly one current-user, single-link, mode-0600 regular backing file;
- attached: that backing file plus exactly one current-user symlink whose target agrees with the independently supplied Lima inventory attachment and the held instance directory.

Entry names remain private. Extra, missing, duplicate-role, directory, or special entries fail closed. Physical identity binds the held disk directory plus backing identity. Backing allocation is observed separately because legitimate guest writes change it.

On macOS, `/var/...` is accepted only through the specifically proven root-owned `/var -> private/var` compatibility alias. Arbitrary intermediate symlinks remain refused.

## Inventory boundary

The caller supplies bounded pre-captured JSON-lines output from the installed Lima 2.2.0 read-only disk inventory operation. The parser accepts only the field set physically established by #634, selects one exact disk locator, and checks the reported directory, raw format/size, and detached or attached instance fields.

Inventory, disk names, instance names, attachment symlinks, and paths are correlation/locator evidence only. They never establish Glaeda ownership.

## Public authority boundary

The public module is a fail-closed facade around a private raw descriptor engine. It exposes only:

- bounded Lima standalone-disk locator/request types;
- descriptor-bound planned-locator absence observation;
- unbound existing-disk observation;
- sanitized physical/backing identities and logical/allocated byte observations;
- detached/attached/conflicting physical disposition;
- fresh descriptor/inventory revalidation.

The public API deliberately has no constructor that combines an arbitrary `ProjectDiskLeaseRecord` with an observed physical digest, no resident-sandbox-to-Lima binding constructor, and no `bind_to_project_disk`/`Exact`/`CurrentAttachment` projection. A previously returned plain enum therefore cannot become P1 mutation authority after descriptor drift.

The underlying implementation retains its original internal projection code only as private implementation/test material. Sibling/product modules cannot name that child module or call those methods.

## P3 handoff

P3 receives observation data only:

```text
planned-locator absence proof
or
unbound physical identity
+ unbound backing identity
+ logical/allocated byte observation
+ raw-format inventory correlation
+ detached/attached/conflicting disposition
```

The backing identity has a strict canonical SHA-256 decoder so a later durable P3 record can reload its accepted host-controlled binding.

P3 becomes the first layer allowed to create ownership provenance. The accepted path is:

```text
durable accepted project-disk generation
-> fresh P2 absence
-> durable no-replay CreateStarted checkpoint
-> exact create executor
-> fresh uninterrupted P2 post-create observation
-> durable physical + backing binding
```

After controller death, a fresh same-name observation is never equivalent to that uninterrupted live sequence and cannot be adopted as owned state.

## P4 handoff

P4 attachment/reconciliation must later combine:

```text
durable P3 provenance
+ fresh held P2 revalidation
+ current P1 attach intent/witness
+ exact resident-sandbox-generation -> Lima-source proof
```

inside one short-lived boundary before P1 publishes `Attached`. P2 itself never manufactures that authority.

## Excluded authority

P2 adds no create, format, attach, detach, start, stop, unlock, resize, delete, repair, cleanup, guest mount, OverlayFS, P1 transition, or `TrustedProjectFilesystemCorrelationProof` constructor. The #634 fixture remains unbound research evidence and can never become a Glaeda project-disk generation by observation alone.
