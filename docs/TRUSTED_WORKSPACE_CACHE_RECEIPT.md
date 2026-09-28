# Trusted workspace and cache receipt producer

This document defines the Linux-only trusted producer that supplies runner workspace and cache identity to the pure verification-profile preflight adapter.

## Authority boundary

The only public acquisition function uses Glaeda's current canonical state root at `/var/lib/glaeda` and accepts one `ProjectIdentity` lookup key. It derives both that fixed root and the `GlaedaV2` workspace/cache identity generation from the same closed current-root selection. The legacy `/var/lib/smolrunner` root remains a distinct explicit generation and cannot be relabelled with current Glaeda identity. The acquisition API exposes no selector for a state root, installation ID, record filename, workspace path, cache path, workspace ID, cache ID, namespace digest, or evidence digest.

There is no root-presence probe, fallback, merge, copy, migration, or adoption path. In particular, a missing current root never causes the producer to open legacy state, and this producer grants no authority to retire legacy state.

The producer performs no readiness decision. It does not execute a process, read credentials, create or delete a cache, clean or reset a workspace, publish evidence, access a browser or network, or grant mutation authority.

## Protected durable records

Each enrolled installation may publish exactly two fixed directory resource records beneath its protected `resources` directory:

- `verification-workspace.json` describes the runner-owned verification workspace;
- `verification-cache.json` describes the fixed `cargo-target` cache beneath that workspace.

Both are ordinary strict `ResourceStateDocument` values. Their ownership markers must bind the exact installation and project and must use canonical `ResourceIdentity::directory` evidence. The cache path must be a strict descendant of the workspace path.

The project identity is located by bounded enumeration of protected installation directories. Zero matches fails as missing state; multiple matches fails as ambiguous state. A caller cannot choose one candidate by path or installation ID.

## Descriptor-relative observation

The producer retains descriptors from the canonical state root through the installation, project record, resources directory, workspace record, and cache record. Every open uses no-follow flags. State directories must retain the reviewed owner/group and mode `0750`; state files must be single-link regular files with mode `0600`, reviewed ownership, and bounded size.

The workspace is opened from the filesystem root one normal component at a time. The cache is opened only relative to the held workspace descriptor. The producer does not canonicalise a caller path and then reopen it.

For workspace and cache it verifies:

- regular directory type;
- non-root, identical owner and group;
- no group/other write permission;
- exact canonical directory evidence matching the protected ownership marker;
- strict cache containment beneath the held workspace;
- stable device, inode, owner, group, mode, and link identity on the held descriptor;
- the same identity at the original parent/name entry after observation.

It similarly rechecks every held protected state directory and file against its original directory entry. Symlinks, hard-linked state files, replaced entries, owner or mode drift, containment escape, and path races fail closed with bounded path-free errors.

## Derived receipt

The producer derives:

- installation ID from the protected installation directory and project document;
- repository from the protected project document;
- workspace ID from a domain-separated digest of the installation ID and exact durable workspace identity;
- fixed cache ID `cargo-target`;
- cache owner workspace ID equal to the derived workspace ID;
- cache namespace digest from the installation, workspace ID, and exact durable cache identity;
- trusted evidence digest from protected record bytes plus observed workspace/cache descriptor evidence.

Private workspace and cache paths are retained only for construction of `TrustedRunnerPrivateEvidence`. They are skipped during serialization and redacted in `Debug` output.

`TrustedWorkspaceCacheReceipt::bind_preflight_evidence` combines this descriptor-derived identity with separately observed source, Git, capability, host-resource, command, and requested-authority evidence by calling the existing pure `TrustedRunnerWorkspaceReceipt` constructor. That constructor remains responsible for structural consistency; profile readiness remains a later pure evaluation step.

## Failure and privacy contract

Failures are typed as missing state, ambiguous state, unsafe filesystem, corrupt state, identity mismatch, receipt construction, or I/O. Public errors contain only a bounded stage token and fixed explanation. They exclude private paths, raw state contents, operating-system diagnostics, repository contents, credentials, and process output.

The producer is intentionally not a general protected-file reader or generic descriptor traversal interface. Fixed state layout and record names are part of the reviewed authority boundary.
