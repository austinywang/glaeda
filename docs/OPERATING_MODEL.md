# Reliable control loop and fleet operating model

## Status

This document records product direction and sequencing. It is not an accepted mutation protocol and does not grant the current executable any new authority.

Glaeda remains pre-alpha. Today it provides diagnostics, deterministic plans, bounded observations, ownership models, and durable execution primitives. Live runner installation, registration, upgrade, autonomous repair, remote telemetry, backup, and fleet coordination remain future work.

Tracked implementation contracts:

- [#110 — release channels, draining, upgrade, and rollback](https://github.com/teamleaderleo/smolrunner/issues/110)
- [#111 — bounded telemetry, incident bundles, and upstream feedback](https://github.com/teamleaderleo/smolrunner/issues/111)
- [#112 — autonomous remediation levels and fleet policy directives](https://github.com/teamleaderleo/smolrunner/issues/112)
- [#113 — backup, restore, and disaster recovery](https://github.com/teamleaderleo/smolrunner/issues/113)

## Product principle

The operator surface and deployment footprint should stay small without sacrificing reliability machinery.

A human or agent should eventually be able to use a small set of predictable commands while Glaeda handles version identity, state validation, draining, journals, rollback, incident evidence, retention, and recovery behind that surface.

Complexity is justified when it removes repeated operator work or prevents data loss, host compromise, silent drift, or unrecoverable runner failure. It is not justified when it invents another workflow language, hides authority, or turns Glaeda into a general-purpose cloud platform.

## System boundary

The intended layers remain separate:

```text
GitHub Actions
    workflow scheduling, status, and canonical job logs

Official GitHub Actions runner
    authenticated listener for one repository or organization scope

Glaeda
    desired state, ownership, lifecycle, isolation policy, diagnostics,
    release management, recovery evidence, and later fleet coordination

Rootless Podman
    bounded disposable execution of repository-controlled code

Project deployment target
    live application state and project-specific persistence
```

A runner host is not automatically a production deployment host. Sharing one machine may be an explicit personal policy, but runner jobs must not inherit deployment credentials, writable service state, container-control sockets, or unrelated project data.

## A living system without self-granted authority

Glaeda may become self-observing, self-healing, and improvement-oriented, but those terms have strict meanings:

- **self-observing** means collecting bounded typed facts and public receipts;
- **self-healing** means executing pre-authorised, narrowly classified repairs through the same ownership, lane, journal, and fresh-verification boundaries as operator-triggered work;
- **self-improving** means turning repeated incident evidence into proposed policy changes, tests, issues, patches, or pull requests through the ordinary repository workflow.

It does not mean:

- an agent may expand its own permissions;
- a host may treat a model diagnosis as observed fact;
- a process may replace its privileged control logic while work is active;
- a generated patch may merge or deploy itself because it claims to fix an incident;
- remote coordination may bypass host-local ownership or safety checks.

## Environments and release channels

The initial operating topology should have three roles.

### Development

Development builds run only in disposable or explicitly experimental environments such as the MacBook Lima lab VM.

They may exercise native ARM64, systemd, rootless Podman, interruption, reboot, and sleep/wake behaviour. They are not the only repair path for the project and are not silently promoted to other hosts.

### Canary

A canary runs one exact release candidate and digest against a bounded set of trusted repositories or workloads.

Canary promotion requires:

- exact build and source identity;
- successful hosted verification;
- clean state compatibility checks;
- bounded real-host health evidence;
- no unresolved incident above the configured threshold;
- a defined rollback target.

One canary failure can pause promotion. Repeated failures across canaries should open a fleet circuit breaker.

### Stable

Stable hosts run tagged, verified releases selected by explicit policy. A stable channel is not a mutable `latest` identity: each host records one exact Glaeda version and binary digest.

The official GitHub runner version is tracked independently from the Glaeda version.

## Independent repair path

Glaeda must not depend exclusively on the fleet it manages to produce its next repair.

The Glaeda repository should retain GitHub-hosted CI or another separately managed path capable of building, testing, and publishing a recovery release. Self-hosted canaries can add systemd, ARM64, Podman, and real-host evidence, but they are supplementary until an independent escape hatch is proven.

## Version and state identities

A trustworthy host records at least:

- Glaeda semantic version;
- exact Glaeda binary digest;
- supported durable-state schema range;
- official GitHub runner version and archive digest;
- installation identity;
- current release channel and exact pinned target;
- previous verified working binary;
- manifest and policy digests;
- last completed upgrade or rollback journal;
- current operator hold or quarantine state.

State schemas, binary releases, official-runner releases, project manifests, container images, and deployed application releases are separate identities. Updating one does not silently update the others.

Unknown future schema versions fail closed.

## Release control loop

A safe upgrade is a state transition, not hot reload.

```text
observe
  -> plan
  -> drain
  -> checkpoint
  -> stage and verify
  -> switch atomically
  -> restart
  -> verify freshly
  -> stabilize
  -> retain or garbage-collect prior version
```

The runner must stop accepting new work before control-plane replacement. Active work finishes normally or requires explicit cancellation policy. The old binary remains available during a bounded stability window.

A failed post-upgrade health check may authorise rollback only when:

- the exact previous binary still exists and verifies;
- state remains readable by that binary or an exact compatible snapshot exists;
- no external identity changed incompatibly;
- no active job or unrelated reconciliation batch would be interrupted;
- the rollback action itself is journaled and freshly verified.

An irreversible state migration, changed GitHub registration, failed rollback, or uncertain ownership causes quarantine and escalation rather than repeated automatic attempts.

## Host control loop

All automated and operator-triggered reconciliation follows the same sequence:

1. **Observe** fresh host, service, GitHub, workload, journal, and capacity state.
2. **Classify** evidence as matching, absent, unknown, conflicting, degraded, or unsafe.
3. **Plan** a deterministic response bound to exact observation identities.
4. **Authorise** against local policy, optional fleet directives, action class, maintenance window, and repair budget.
5. **Checkpoint** before the first mutation and at every uncertain boundary.
6. **Execute** only through reviewed typed lanes and absolute commands.
7. **Verify** through new observations; process success is attempt evidence, not reconciliation evidence.
8. **Rollback or compensate** according to the declared action class.
9. **Quarantine and escalate** when evidence becomes uncertain, verification fails, or a circuit breaker opens.

## Autonomy levels

There is no single `autonomous=true` setting.

Authority is granted per host class, project, action kind, release channel, and risk level.

- **observe** — collect local bounded facts and incidents;
- **suggest** — produce plans and explanations only;
- **approve** — queue a plan for operator or policy approval;
- **repair** — execute allowlisted reversible or compensating actions within a bounded budget;
- **quarantine** — drain and stop new work while preserving evidence;
- **escalate** — prepare an incident or upstream issue proposal.

Early automatic work should be narrow: restart an exactly managed stopped service with no active job, clean an exactly owned expired disposable resource, quarantine a conflicting runner, or roll back a just-promoted binary during its verified stability window.

Package installation, account mutation, registration or removal, destructive storage repair, credential rotation, state migration, production deployment, and irreversible actions remain operator-confirmed until separately proven safe.

## Fleet coordination

A later coordinator may distribute versioned policy directives, but it is not an unrestricted remote shell.

A directive may select:

- allowed Glaeda and official-runner versions;
- release channel and maintenance window;
- maximum unavailable hosts;
- capability labels and workload admission limits;
- concurrency, CPU, memory, PID, and disk policy;
- incident retention and export policy;
- permitted automatic action classes and budgets;
- project or host holds and quarantines.

Each directive needs a schema version, stable identity, generation, issue time, expiry, fleet scope, target selector, and content digest. Hosts reject stale, malformed, incompatible, out-of-scope, or locally forbidden directives.

Host-local ownership conflicts, unknown state, active jobs, operator holds, and recovery mode remain vetoes.

## Repair budgets and circuit breakers

Automatic repair needs explicit ceilings:

- actions per cycle;
- cycles per time window;
- service restarts;
- rollback attempts;
- concurrently unavailable runners;
- retained incident bytes and count;
- retry delay;
- repeated identical failures before quarantine.

A circuit breaker opens when a repair repeatedly fails, rollback fails, observations change unexpectedly, or a promoted release causes similar regressions on multiple hosts.

When open, the system preserves evidence, drains affected runners when safe, stops mutation, and prepares escalation.

## Bounded incident evidence

Operational learning should be based on typed incident bundles, not broad surveillance.

Local health observations, execution receipts, incident bundles, and upstream issue proposals are separate data classes.

A valid incident bundle may contain bounded facts such as:

- normalized incident and failure codes;
- Glaeda and official-runner versions;
- distribution, architecture, and declared capability class;
- pseudonymous installation or fleet identity;
- relevant public plan, journal, action, run, and artifact identities;
- readiness, disk, resource, and service findings;
- repair, rollback, quarantine, and verification outcomes;
- first/last timestamps, occurrence count, schema version, and digest;
- explicit truncation and omission counts.

It must not contain raw repository files, arbitrary command logs, full environment dumps, tokens, credentials, unrelated usernames, browser data, cloud metadata, or unrestricted process inventories.

Collection stays local by default. Upload, aggregation, and GitHub submission require explicit policy.

## Upstream improvement loop

The first feedback feature is proposal-only:

```text
observe failure
  -> persist bounded incident
  -> coalesce duplicates
  -> explain evidence and uncertainty
  -> prepare duplicate-search terms and issue draft
  -> operator or policy accepts submission
  -> ordinary issue, branch, review, CI, canary, and promotion flow
```

An agent may use accepted incident evidence to propose a failing test, patch, issue, or pull request. The patch is verified against the original failure and the normal repository gates. Incident evidence grants no merge, release, or host-mutation authority.

Automatic issue creation may come later only with destination allowlists, durable submission journals, duplicate detection, rate limits, redaction validation, and explicit operator policy.

## Backup and restore

Durable ownership records, journals, lease records needed for cleanup, release metadata, runner registration identity, incident metadata, and fleet policy need a versioned backup contract.

Temporary tokens, active secrets, `/run` state, disposable worktrees, mutable caches, temporary containers, and reproducible downloaded artifacts are not authoritative backup state.

Project databases and user uploads remain project-owned persistence domains. Glaeda may later invoke project-specific backup adapters, but cannot claim that one generic host snapshot safely backs up every application.

Restore begins quarantined. Restored documents do not authorise adoption by themselves. Glaeda re-observes host resources and GitHub registrations, classifies drift or conflict, and produces a recovery plan before new host-specific evidence is written.

A VM snapshot is useful recovery material but is never current proof of external registration or managed-resource ownership.

## Public interface

The detailed internal model should eventually support a small public surface:

```text
glaeda doctor
glaeda status
glaeda reconcile
glaeda upgrade
glaeda rollback
glaeda incident
glaeda backup
glaeda restore
glaeda quarantine
```

Each high-level command may expose `plan`, `apply`, `status`, or `verify` subcommands where authority or recovery requires the distinction. Human output and stable JSON come from the same typed reports.

Agents receive the same contracts rather than screen-scraping terminal prose.

## Sequencing

The reliable control loop is not the next monolithic implementation task.

1. Finish durable host preparation and dedicated runner-account readiness.
2. Install, register, inspect, drain, update, disable, and remove one official runner safely.
3. Run one repository job inside a bounded disposable environment and retain a public receipt.
4. Define release and rollback plans before enabling self-update.
5. Add local incident bundles before remote telemetry or issue submission.
6. Add backup formats and tested restore before relying on one host's durable state.
7. Add fleet inventory and read-only directives before any remote mutation.
8. Enable one narrow automatic repair class at a time with budgets, canaries, and circuit breakers.
9. Add agent-assisted upstream proposals only after evidence, authority, and review boundaries are stable.

## Non-goals

This direction does not require:

- a mandatory always-on cloud service;
- a mandatory daemon for single-host use;
- Kubernetes or a generic multi-tenant scheduler;
- automatic production deployment after CI success;
- unrestricted remote shell access;
- broad behavioural telemetry;
- model-controlled privilege escalation;
- removal of GitHub Actions as the first scheduler and status surface.

Reliability may add substantial internal machinery. The operator contract should remain explicit, inspectable, recoverable, and small.