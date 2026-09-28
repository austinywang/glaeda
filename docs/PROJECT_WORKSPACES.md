# Developer project namespace and workstation recovery

## Outcome

Glaeda should make the operator's development machine feel reproducible instead of precious.

The target experience has two complementary promises:

1. **Project ergonomics:** a human or trusted agent can name a project and enter the right development checkout without remembering its physical directory.
2. **Blank-Mac recovery:** when the previous Mac disappears, a replacement Mac can reconstruct the reviewed developer environment and project catalog from durable declarations plus explicitly reacquired credentials.

The memorable journey should converge on something like:

```text
bootstrap Glaeda
-> restore operator catalog
-> converge developer environment
-> glaeda project enter glaeda
-> work
```

The ordinary path should feel like:

```text
glaeda project enter glaeda
glaeda project enter codex
glaeda project enter quarry
```

`project enter` resolves the logical project, starts the developer environment when required, materializes a checkout when absent, and lands the operator at the repository root.

This document tracks issue [#372](https://github.com/teamleaderleo/smolrunner/issues/372).

## Two deliberately different worlds

The persistent developer experience and the disposable CI product share identities and lifecycle machinery, but they have different trust contracts.

### Developer environment

The developer environment is a persistent, operator-owned workspace for trusted humans and trusted agents. It may contain long-lived Git checkouts and safe development caches. The first managed form should be one persistent Lima/VZ Linux guest; existing Mac-native checkouts can also be adopted in place.

Its purpose is comfort and continuity:

- stable project names;
- persistent checkouts;
- warm development caches;
- repository-native setup;
- editor and terminal integration;
- recovery reporting for local-only work.

### Disposable execution environment

The production GitHub Actions path remains one fresh VM per job. Repository code, workflow steps, dependencies, and tests are treated as hostile. A disposable worker receives no developer checkout mount, developer credential, personal home-directory integration, or persistent writable developer state.

A persistent developer checkout can provide an exact Git source identity to an execution request, but it never becomes the hostile execution boundary itself.

This separation is mandatory. The developer namespace must not weaken `DISPOSABLE_AUTOSCALING_CI.md`.

## The blank-Mac recovery promise

The acceptance thought experiment is intentionally severe: the previous Mac and all machine-local state disappear. The operator obtains a replacement Apple-silicon Mac and wants a useful development environment again with a few memorable actions.

A successful recovery should be able to reconstruct or report:

- the reviewed Glaeda installation identity;
- the operator's accepted workstation/project catalog;
- required host dependencies;
- the persistent developer Lima environment and reviewed profile;
- canonical project identities and aliases;
- essential project checkouts selected for eager restore;
- the remainder of the project catalog as lazy materializations;
- repository-owned bootstrap readiness;
- optional terminal/editor workspace integration;
- local state that cannot be reconstructed from durable remote inputs.

The machine becomes increasingly disposable because the durable recovery inputs are small and inspectable.

### Durable recovery inputs

Expected durable inputs include:

- Glaeda release/install identity;
- versioned operator catalog;
- pinned developer-VM/template inputs;
- repository remotes and immutable source identities;
- committed dotfiles or host configuration where the operator elects to manage them;
- external backup references for data Git cannot reproduce;
- explicit credential requirement classes;
- optional cache identities used only for acceleration.

### Explicitly reacquired inputs

Secrets should be reacquired through reviewed credential flows instead of copied through the project catalog. Examples include:

- GitHub authentication;
- SSH private keys;
- Keychain entries;
- signing keys;
- package-publisher credentials;
- cloud credentials;
- browser or password-manager state.

The catalog may declare that a capability is required. It must never contain secret bytes.

## Canonical identity model

Keep logical identity separate from physical location.

### Project

A `ProjectIdentity` names the logical repository independent of where it is checked out. The GitHub repository slug stays `teamleaderleo/smolrunner` until the repository-rename lane moves it, so the current canonical example remains:

```text
github.com/teamleaderleo/smolrunner
```

Canonical identity should derive from a reviewed source-remote normalization contract rather than a directory basename.

### Alias

An alias is an ergonomic bounded name such as:

```text
glaeda
codex
quarry
```

Aliases never prove repository identity. An alias that maps to more than one project is a conflict and must block until explicitly resolved.

### Source

A source binds the canonical Git remote identity and optional fork/upstream relationships. Source identity remains separate from authentication and from a moving branch name.

### Materialization

A materialization is one concrete checkout or worktree. Its evidence includes the project identity, location class, ownership class, Git worktree identity, observed commit/tree, dirty state, and any required path-safety facts.

Initial location classes should include:

- adopted Mac checkout;
- managed developer-guest checkout;
- disposable execution checkout.

### Developer environment

A developer environment is a persistent trusted operator environment capable of hosting materializations. The first managed environment should remain a narrow Lima/VZ contract.

### Execution

An execution is one disposable attempt bound to exact source and job identities. Execution remains governed by the disposable-worker contracts and never inherits authority simply because its source originated in a persistent developer materialization.

## Operator catalog

Glaeda should define a versioned, portable, secret-free catalog format. The real operator catalog may live in a separate repository chosen by the operator; the public Glaeda repository owns only the schema and redacted examples.

Illustrative schema direction:

```yaml
version: 1
projects:
  - id: github.com/teamleaderleo/smolrunner
    aliases: [glaeda]
    source: https://github.com/teamleaderleo/smolrunner.git
    materialization: developer
    restore: eager

  - id: github.com/openai/codex
    aliases: [codex]
    source: https://github.com/openai/codex.git
    materialization: developer
    restore: lazy
```

The durable catalog should avoid absolute machine-specific paths as identity. Location policy can resolve to reviewed roots such as the managed guest project root or an explicit adopted Mac root.

Unknown schema versions and unknown authority-bearing fields fail closed.

## Command model

Exact names may evolve, but the intended vocabulary is compact:

```text
glaeda home plan
glaeda home converge
glaeda home status

glaeda project list
glaeda project adopt PATH
glaeda project ensure PROJECT
glaeda project enter PROJECT
glaeda project status PROJECT
glaeda project move PROJECT TARGET_CLASS
glaeda project forget PROJECT
glaeda project prune
```

### `home plan`

Observe the accepted catalog and current machine and report the exact successor actions required to reach the declared workstation state. This command is read-only.

### `home converge`

Advance the accepted workstation plan through bounded actions with durable checkpoints and fresh re-observation barriers. Host dependency installation, credential acquisition, destructive cleanup, and other consequential authority stay behind their own reviewed adapters and confirmations.

### `home status`

Report current catalog generation, developer environment state, project materialization state, blockers, recovery debt, and local-only data risk.

One especially useful long-term question is:

> What would be lost if this Mac disappeared now?

Where bounded observation permits it, the answer should identify dirty worktrees, local-only commits/branches, unpushed tags, unique persistent files/workspaces, and other declared unrecoverable data classes.

### `project ensure`

Make one project materialization available according to the catalog. A missing project may be cloned or otherwise materialized under an accepted plan.

### `project enter`

Resolve the project, ensure its development materialization, start the developer environment when needed, and enter a shell at the repository root.

This is the ergonomic center of the feature.

### Shell integration

A standalone child process cannot change the parent zsh working directory. A later tiny shell helper may therefore provide a shorter experience such as:

```text
p glaeda
```

For a Mac-native checkout the helper can perform a real parent-shell `cd`. For a guest-owned checkout it can enter the developer guest at the resolved repository root.

The namespace must work before shell integration exists.

## Generation and reconciliation model

Database ACID terminology does not map cleanly onto Git clones, VM lifecycle, network fetches, and filesystem publication. The useful model is closer to Nix/OSTree generation switching combined with Glaeda's existing compare-and-swap and reconciliation contracts.

The core invariant is:

```text
prepare candidate
-> prove candidate
-> publish generation
-> clean superseded state
```

### Desired-state generation

The accepted project/workstation catalog is an immutable canonical document with an exact revision or generation. Every mutation plan binds the observed predecessor generation.

### Stage away from live state

A new checkout, migrated materialization, or successor metadata document is prepared away from the accepted live namespace. Partial directories never become accepted simply because they exist.

### Prove before publication

Before a successor can become current, Glaeda proves the evidence relevant to that resource, such as:

- canonical repository remote;
- exact Git worktree root;
- expected object identity when one is required;
- filesystem type/owner/mode class;
- no unsafe path alias or unresolved symlink condition;
- required repository markers;
- clean or dirty state according to the operation policy.

### Compare-and-swap publication

Mutation occurs under a single-writer contract and checks that the predecessor generation still matches. Two agents that plan from the same revision cannot both silently win. One publishes; the other receives stale/conflict evidence and must re-observe and re-plan.

### Atomic metadata switch

Where the selected filesystem contract permits it, completed successor metadata is durably staged and then made current through an atomic rename or pointer switch. The prior accepted generation remains available until the successor is durable.

A process failure before publication leaves the predecessor authoritative. A process failure after publication leaves the successor authoritative and turns remaining cleanup into explicit debt.

### Journaled side effects

Network clone/fetch, VM lifecycle, package installation, and other effects cannot participate in one filesystem transaction. They use durable attempt identities, checkpoints, idempotent reconciliation, and compensation where cleanup is safe.

The desired result resembles a saga around a tiny atomic metadata core:

- local accepted metadata changes atomically;
- external side effects are journaled;
- interruption never promotes incomplete state;
- cleanup debt remains observable;
- retry uses exact retained identities instead of guessing from names.

### Rollback

Rollback means switching accepted metadata to a retained prior generation only when its referenced materializations remain proven compatible. Removing a newly cloned directory is cleanup or compensation, not proof that every external side effect has been restored.

## Adopting today's `~/Projects`

The first useful version should work with the operator's existing directory full of clones and forks.

`project adopt PATH` should be conservative and in-place by default. It observes:

- exact Git worktree root;
- source/remotes;
- current commit/tree;
- dirty state;
- local-only branch/commit evidence where bounded observation is available;
- linked worktrees and submodule declarations where relevant;
- owner and path class;
- symlink or path-alias hazards.

The result classifies the checkout as exact, adoptable, conflicting, or unknown.

Adoption does not silently:

- move files;
- reset source;
- clean untracked files;
- fetch or rebase;
- delete a fork;
- rewrite remotes.

The current read-only checkout evidence can be exercised directly on Unix hosts without catalog,
adoption, residency, or mutation authority:

```bash
glaeda-project-observe --checkout /absolute/canonical/checkout --output json
```

This compiled front door runs the existing bounded offline checkout observer once and emits a
path-private typed report. It requires an explicit canonical absolute checkout, disables Git
credentials, hooks, lazy fetch, replacement objects, and ambient configuration; configured
clean/process filters are refused. It requires two coherent snapshots. The report includes exact
commit/tree, canonical GitHub source when one is provable, dirty/untracked/local-only recovery
evidence, worktree/submodule topology, and one opaque physical materialization identity. It does
not establish catalog adoption, a resident-project lease, cache validity, remote freshness,
cleanup authority, or permission to execute inside the checkout.

Once a canonical checkout and exact object IDs are already known, `repo-query/v1` collapses the
common review-evidence sequence into one bounded local observation:

```bash
glaeda-repo-query \
  --checkout /absolute/canonical/checkout \
  --project github.com/owner/repository \
  --base COMPLETE_BASE_OID \
  --head COMPLETE_HEAD_OID \
  --tree COMPLETE_HEAD_TREE_OID \
  --output json
```

It returns exact object and merge-base identity, ancestry, commit count, per-file numstat, a digest
of the complete patch, and the patch itself only when it fits the caller's bounded ceiling. Optional
fixed fields add literal exact-tree grep, bounded blob reads, bounded path history, and object
existence/type/size. It does not accept refs or arbitrary Git arguments, fetch, mutate, publish,
establish remote freshness, or grant reuse authority. See the [Big Red controlled
result](experiments/resident-repo-query-big-red-2026-08-31.md).

A separate migration operation may later stage a successor in the managed developer guest and switch accepted materialization only after the successor is proven.

## Lazy materialization

Catalog membership does not imply disk occupancy.

Projects can declare an initial restore policy such as:

- `eager` for a small essential working set;
- `lazy` for everything else.

`project ensure` or `project enter` can then choose a reviewed materialization method:

- normal clone for ordinary owned repositories;
- partial/blobless clone for large upstream repositories;
- sparse checkout only for an explicitly bounded path set;
- exact immutable commit checkout for research or handoff tasks.

Submodules, LFS, private repository access, package registries, and repository bootstrap each remain explicit capabilities or blockers.

After source materialization, `./scripts/bootstrap` remains the repository-owned readiness boundary described in `WORKSPACE_BOOTSTRAP.md`.

## Agent-facing semantics

The project namespace should give agents a high-level desired-state API instead of arbitrary filesystem authority.

An agent can request:

```text
ensure project X from catalog generation N
```

and receive one of a bounded set of outcomes:

- already satisfied;
- one accepted action applied;
- continuation requiring fresh observation;
- stale/conflict;
- blocked by missing credential/capability/operator decision;
- failed with retained recovery debt.

Agents do not infer ownership from directory names, construct arbitrary checkout roots, delete unknown directories, or silently widen source/network/credential authority.

## Recovery and local-only work

Reproducibility has a hard boundary: Git and declarations can recreate only state that reached a durable source.

Glaeda should make the gap visible. Candidate risk classes include:

- dirty tracked files;
- untracked files outside an approved backup path;
- local commits absent from every declared remote;
- local branches with no declared durable upstream;
- local tags absent from the durable source;
- files in a persistent developer workspace with no declared recovery source.

A future backup integration can reduce these risks, but the project catalog itself is not an automatic backup system.

## Host configuration dependencies

Blank-Mac recovery should reuse mature host configuration tools where they fit.

Research examples:

- Nix/NixOS uses declarative desired state, isolated build outputs, generations, atomic switching, and rollback.
- nix-darwin applies a declarative Nix module model to macOS.
- Home Manager manages user environments and can integrate with nix-darwin.
- OSTree uses Git-like content-addressed filesystem trees and atomic deployment transitions.

Glaeda should borrow their successful contracts without becoming another package manager or operating-system distribution. A future workstation convergence adapter may invoke Nix/nix-darwin, Homebrew, or another mature dependency manager after supply-chain, upgrade, rollback, and uninstall policy is explicit.

Relevant upstream references:

- <https://nixos.org/guides/how-nix-works/>
- <https://nix-darwin.github.io/nix-darwin/>
- <https://nix-community.github.io/home-manager/installation/nix-darwin.html>
- <https://ostreedev.github.io/ostree/introduction/>
- <https://ostreedev.github.io/ostree/atomic-upgrades/>

## Security boundary

The developer project namespace must preserve these rules:

- persistent developer checkouts are never mounted into hostile CI workers;
- developer credentials are never inherited by disposable job workers;
- a persistent writable developer cache never becomes verification authority merely because it exists;
- source identity and execution identity stay distinct;
- compromised job VMs cannot mutate the operator catalog or accepted developer materializations;
- the persistent developer environment has its own trusted-operator threat and backup policy;
- aliases, path basenames, and directory existence never prove project ownership;
- destructive cleanup requires exact accepted ownership and separate authority.

## First implementation slices

### P1 — design and canonical identities

This document and #372 define the initial product contract. P1 also links the track from the roadmap while keeping disposable autoscaling as the production critical path.

### P2 — read-only catalog and discovery

Implement only pure/read-only behavior:

- strict versioned catalog parser;
- canonical Git remote/project identity;
- alias resolution and collision reporting;
- explicit-root checkout discovery;
- `project list` and `project status` human/JSON reports;
- zero filesystem mutation, network access, credentials, or VM lifecycle.

### P3 — in-place adoption and durable generations

Add:

- exact checkout observation;
- adoption planning for existing `~/Projects` checkouts;
- durable accepted catalog generation;
- compare-and-swap publication;
- stale/replay/interruption tests;
- local-only risk reporting.

No move, delete, reset, or clean path belongs in P3.

### P4 — persistent developer guest and `enter`

Add one managed persistent Lima developer environment, separate from disposable CI workers, then support one public-repository `project ensure` and `project enter` journey. Reuse reviewed Lima observation/lifecycle primitives where their authority matches this contract.

Editor/cmux integration follows after the core command works.

### P5 — blank-Mac convergence

Define and prove a reviewed bootstrap journey that can:

- establish Glaeda;
- retrieve and validate the operator catalog;
- converge required host dependencies;
- create/restore the developer environment;
- eagerly restore essential projects;
- leave the rest lazy;
- expose credential blockers;
- report unrecoverable local-only requirements.

Destructive cleanup remains separate.

## Acceptance scenarios

1. An existing Mac with many mixed clones and forks under `~/Projects` can be discovered without mutation.
2. Selected checkouts can be adopted in place while dirty and local-only work remains intact and visible.
3. An empty developer guest can execute `project enter glaeda`, materialize the canonical repository, and enter its root.
4. Killing Glaeda during clone/staging leaves the predecessor generation authoritative and the candidate recoverable or cleanable.
5. Killing Glaeda immediately after generation publication leaves the successor authoritative and cleanup debt explicit.
6. Two agents racing from one catalog revision produce exactly one publication and one stale/conflict outcome.
7. Alias collisions block instead of guessing.
8. A blank replacement Mac can reconstruct the declared developer environment and eager projects after reviewed install/auth steps.
9. A hostile CI job continues to receive none of the persistent developer workspace or credential authority.

## Deferred

The first version does not require:

- a custom distributed filesystem;
- FUSE/FSKit or a Finder-visible virtual mount;
- replacing Git;
- real-time workspace synchronization across Macs;
- automatic backup of arbitrary uncommitted files;
- secret synchronization through the catalog;
- silent relocation of existing repositories;
- custom package/dotfile management where a mature dependency suffices;
- persistent developer materializations as hostile-CI workers;
- unrestricted agent shell authority over the Mac.

A future Finder-visible or `~/Glaeda/...` portal could make the namespace even more magical. Identity, materialization, generation safety, and `project enter` should prove themselves first.
