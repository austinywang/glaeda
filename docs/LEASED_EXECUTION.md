# Leased execution and previews

> **Deferred product track:** leased workspaces, previews, and the host-rootless-Podman backend are not on the current CI critical path. See [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md).

## Status

This document records an exploratory product direction. It expands Glaeda's existing runner-steward work without discarding it.

The dependable host, ownership, privilege, rollback, runner-lifecycle, and disposable-execution work remains the foundation. Leased workspaces and previews become optional capabilities built on that foundation after the existing safety model can support mutations.

The first platform-independent implementation slice now exists in `src/lease.rs`: validated lease identities, lease kinds, legal lifecycle transitions, renewal, sleeping, terminal states, and optimistic revisions. [ADR 0004](adr/0004-lease-lifecycle-core.md) records that decision. Persistence, clocks, artifact provenance, host mutation, and preview routing remain future work.

## Product direction

Glaeda can grow from a steward for self-hosted GitHub Actions runners into a small execution host for the machines an operator already owns.

GitHub Actions remains the first scheduler, workflow language, status interface, and log store. Glaeda owns the host-side concerns:

- preparing and reconciling ordinary Linux workers;
- managing official GitHub Actions runner listeners;
- executing repository code in bounded rootless containers;
- retaining selected caches and workspaces;
- keeping selected services alive under an explicit lease;
- assigning temporary preview routes;
- expiring and cleaning up leased resources;
- reporting worker capacity, disk pressure, and lifecycle state.

This is a superset of the current runner-steward design. A user can stop at runner management and disposable verification. The later execution features remain optional.

## Verification first, deployment by policy

Agent activity can create many commits and branches. Verification can run frequently because correctness is the common requirement. Live deployment should require a separate decision.

A useful default policy is:

1. Every eligible push may run checks through GitHub Actions.
2. Successful checks produce immutable references to source, images, static output, logs, or other artifacts.
3. A live preview starts only after an explicit request or repository policy permits it.
4. Preview leases expire automatically.
5. Production promotion stays outside the initial feature set.

Possible preview triggers include:

- an explicit `glaeda preview create` command;
- a manually dispatched GitHub Actions workflow;
- a repository label or trusted pull-request comment;
- promotion of an integration branch;
- an approved request from another trusted local tool.

This separates "prove the change" from "keep a service running" and avoids consuming deployment quotas for every agent iteration.

## Execution vocabulary

### Run

A bounded command execution that returns logs, status, and artifact references, then releases its compute allocation.

### Workspace

A project checkout with selected writable state and caches. A workspace can outlive one run and can be handed from one trusted actor to another.

### Preview

A workspace or immutable artifact with a live network service, an assigned route, and an expiration policy.

### Lease

The ownership and lifetime record for a run, workspace, or preview. A lease has an owner, expiry time, renewal policy, resource request, and cleanup policy.

### Worker

A Linux machine capable of hosting runner listeners, runs, workspaces, or previews. Workers advertise bounded capabilities such as architecture, CPU, memory, browser support, storage pressure, and public-routing support.

### Artifact

An immutable or content-addressed output produced by verification or a build. Initial artifact kinds may include an OCI image digest, a static directory archive, a committed source archive, and a log or test-result reference.

## Warmth and resource cost

"Warm" should remain explicit because each level consumes different resources:

- **Warm cache:** package archives, image layers, and compiler output remain on disk.
- **Warm workspace:** checkout and generated files remain on disk while processes are stopped.
- **Warm process:** the application, browser, or other service continues consuming memory and CPU allocation.

The default should favor warm caches, preserve workspaces only under active leases, and keep processes alive only when a preview requires them.

A later idle-suspension policy may stop a preview process while preserving its workspace and route metadata for restart.

## Local-first execution path

The first execution backend should use the rootless Podman model already selected by Glaeda.

A practical first path is:

1. GitHub Actions runs on an official listener managed by Glaeda.
2. Repository-owned scripts or a Containerfile perform verification and build work.
3. The workflow requests a preview from a verified OCI image digest or static artifact.
4. Glaeda starts the preview with declared CPU, memory, PID, network, and lifetime limits.
5. A reverse-proxy adapter assigns a route.
6. Glaeda records the lease and removes the preview after expiry.

The control socket remains available only to the trusted Glaeda process. Repository code never receives a Podman or Docker socket.

## Deployment targets

Glaeda should begin with one local target and keep target selection behind a narrow interface.

Possible future targets include:

- local rootless Podman on the current worker;
- another enrolled Glaeda worker;
- a static hosting provider;
- Vercel or Cloudflare through explicit provider adapters;
- a managed sandbox provider when local capacity is unavailable.

Provider routing should come after local artifact, lease, and cleanup semantics prove themselves. Glaeda should avoid becoming a compatibility layer for every hosting product.

## Collaboration with Stensibly

Stensibly can remain the shared work ledger while Glaeda owns execution state.

A Stensibly task or claim may reference:

- a Glaeda run;
- a leased workspace;
- a preview URL;
- an immutable artifact;
- logs or screenshots.

A handoff may transfer access to an existing workspace. Each actor should normally receive an independent Git worktree and writable environment. Shared live workspaces should require an explicit collaboration mode because simultaneous writers introduce unclear ownership and conflicting mutations.

The integration should remain optional. Glaeda must work through its CLI and GitHub Actions without requiring Stensibly.

## Daemon boundary

The current CLI-first design remains appropriate for host inspection, planning, preparation, runner lifecycle, and one-shot execution.

A background process becomes justified when Glaeda must own:

- lease renewal and expiry;
- long-running preview supervision;
- idle suspension and restart;
- route reconciliation;
- worker heartbeats;
- capacity reservations;
- asynchronous cleanup.

The daemon should reuse the same typed planning, ownership, journal, and execution library as the CLI. It should arrive after mutation safety and local execution are dependable.

## Initial non-goals

- Replacing GitHub Actions workflow YAML or the official runner protocol.
- Rebuilding Vercel, Cloudflare, Kubernetes, or a generic public PaaS.
- Safe execution of arbitrary public-fork code on persistent personal machines.
- Hostile multi-tenant workloads.
- A custom container runtime, image format, reverse proxy, or TLS implementation.
- Automatic production promotion.
- Model-driven scheduling inside Glaeda.

## Questions to answer through prototypes

- Which artifact contract covers OCI and static previews without adding a new pipeline language?
- Does the first preview lifecycle need a daemon, or can systemd timers and explicit commands prove the model?
- Should preview routes be reconciled through Caddy's API, generated configuration, or another adapter?
- Which workspace state deserves persistence beyond dependency caches?
- How should an opportunistic laptop worker advertise availability without becoming a required control-plane member?
- Which provider adapter, if any, offers enough value after local previews work?
