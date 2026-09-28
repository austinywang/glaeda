# cmux fleet rollout: merged means deployed

Status: design. Owner issue: #149 (automated updates with canary checks and rollback).
Builds on [host reconciliation](HOST_RECONCILIATION.md), [fleet distribution](FLEET_DISTRIBUTION.md),
the mini-fleet manifest, host reservations (`scripts/glaeda_reservation.py`), and the journaled
service updater (`src/disposable_launchd_service/upgrade.rs`, #1131).

Desired state (the fleet manifest `build-fleet/mini-fleet.json` and `channels.json`) lives in
`manaflow-ai/cmuxterm-hq`, a private repository. The fleet root on each mini is
`/Users/Shared/cmux-build-fleet` (`deploy-daemon.sh` defaults to `/Users/Shared/cmux-ci`; the
agent always uses the fleet root).

## The problem, measured on 2026-09-24

Six build-fleet PRs merged in cmuxterm-hq in one day (#575, #576, #579, #581, #584, #585).
The deploy of #575, #576, #579 and #584 went out by hand:

- A canary on cmuxs-mac-mini-3, then 6 more minis, each deployed by Leo pasting commands,
  because agent remote writes were blocked by the permission classifier.
- 3 minis were skipped because they were busy. Nothing retries them.
- One experimental host, cmux11s, ran `worker-snapshot-20260921` and was overwritten without a
  pin. Nothing recorded that its wrapper pointed at a binary no deploy had installed.

Underneath:

- Restarting a system LaunchDaemon with `launchctl` needs root, and only 2 of 15 minis have
  passwordless sudo.
- `bin/worker` is a wrapper that points at a different binary on almost every host
  (`worker-policy-<hash>`, `worker-telemetry-<hash>`, `worker-snapshot-*`, `worker-recovery-*`).
  Nobody can say from one place what each host runs.
- The controller runs on cmux-lawrence, which only its owner may touch.
- Three workers were silent for days after a controller token rotation. `cmux_mini_probe.sh`
  already flags a missing `controller_token`, but nothing compares a present, rejected token or
  the running binary against what the host should run.

## Goal

A merged, reviewed build-fleet change reaches every eligible worker without a human copying
files. It reaches one canary first, is promoted when the canary stays healthy, rolls back when
it does not, and never touches a host that is building, reserved by someone else, pinned, or
listed in `never_touch`.

## Design

### 1. Releases are built once, in CI

On every push to cmuxterm-hq `main` that touches `build-fleet/`, a release workflow builds one
release: the worker binary (darwin/arm64), `recipes/`, `with-host-lock`, and the client files.
It writes `release.json` with the source commit, per-file SHA-256, and the controller
capabilities the release requires. The release is published as a GitHub release asset with a
build-provenance attestation (`actions/attest-build-provenance`) and the attestation bundle
shipped next to it.

The LAN mirror (`172.20.21.158:8787`) is the coordinator's cache and is unreachable from hosted
runners. Mirroring runs on a fleet-side runner, which copies the release and its bundle into the
mirror. Minis fetch by hash only, so a mirror can serve stale bytes but never wrong ones.

Operators and agents never build a worker on their own machine.

### 2. Desired state lives in the manifest

Each member of `build-fleet/mini-fleet.json` gains a `worker` block:

```json
"worker": {"ring": "canary", "build_slots": 2, "pin": null, "adopt": null}
```

- `ring`: `canary` or `stable`. One or two `std` minis are canary.
- `build_slots`: `CMUX_CI_BUILD_SLOTS` for that host (2 on `std`, 1 on `light`).
- `pin`: a release hash or `"manual"`. A pinned host is never upgraded automatically.
- `adopt`: the wrapper target the agent may take over on this host (see unknown wrappers below).

The manifest's existing `never_touch` list (already naming cmux-lawrence) excludes hosts from
rollout entirely. The agent is never installed on them.

`channels.json` in the same repository maps rings to release hashes:

```json
{"canary": "<sha256>", "stable": "<sha256>", "paused": false}
```

Changing desired state is a reviewed PR. `paused: true` stops rollout fleet-wide.

### 3. A pull agent on each mini applies it

`glaeda-fleet-agent` runs as the build user (`cmux`) from a LaunchDaemon installed once at
enrollment. It ships in the glaeda candidate bundle, is updated by `glaeda-mini-fleet upgrade`,
and never updates itself.

**Permissions.** `bin/` is owned by `cmux`, but the files `deploy-daemon.sh` installs in it are
owned by root. The agent never edits those files. It writes new files beside them and renames
over directory entries, which only needs write access to the `cmux`-owned directory. `recipes/`
and `secrets/` may be root-owned (created by `sudo mkdir`); milestone 2 records the owner and
mode of `bin/`, `bin/worker`, `recipes/` and `secrets/` per host before any apply is designed
around them.

**Restart.** The worker LaunchDaemon has `UserName cmux` and `KeepAlive true`, so the agent
restarts an idle worker by sending it `SIGTERM` and launchd relaunches it on the new wrapper. It
does not reuse `upgrade-worker-when-idle.sh`, whose `launchctl kickstart system/...` needs root.
A cleaner restart is a narrow sudoers rule set up once at enrollment with
`glaeda-mini-fleet sudo-plan --sudoers`.

**Updater.** Apply, verify and rollback are built on the journaled updater in
`src/disposable_launchd_service/upgrade.rs` (tested by `tests/canary_service_upgrade.rs`), as #149
asks, not on a second implementation. The difference for a LaunchDaemon worker is only the
restart step (`SIGTERM` instead of reloading a user agent), which becomes a hook in that updater.

**Reconciliation loop**, every few minutes:

1. **Observe.** Record the wrapper target and its hash, the running binary hash, the recipe
   release, slot count, reservation marker, and host lock state. Unknown stays unknown and blocks
   mutation (`needs_inspection`).
2. **Plan.** Compare against the manifest and `channels.json` for this host's ring and pin.
3. **Apply**, only when all of these hold:
   - the host is not in `never_touch` and not `pin: manual`;
   - the wrapper target is one the agent installed, or the manifest's `adopt` names it. Any other
     target is `needs_inspection` and left untouched until `adopt` names it, then adopted without
     overwrite, as in host reconciliation. This is the cmux11s case;
   - the target hash is not quarantined on this host (see Quarantine);
   - the release verifies: hashes match and the attestation passes (see Security);
   - the controller advertises every capability the release lists;
   - there is no reservation held by another owner, and the host lock is free.

   Then, in order:
   1. Write a `glaeda-reservation/v1` marker with owner `glaeda-fleet-agent`, purpose `rollout`,
      expiring in about 15 minutes. The worker checks for a foreign reservation before every
      claim (cmuxterm-hq #579), so no new job starts.
   2. Confirm 0 active jobs for this host via the controller's `/v1/jobs/active`. If any, release
      the marker and retry later.
   3. Take `host.lock`, install `bin/worker-<hash>` and the recipe release (`recipe-release.py`),
      write `bin/worker.env`, swap the wrapper by rename, and keep the previous wrapper.
   4. Restart the worker and verify (or roll back), still holding `host.lock`, then release the
      lock and the marker.
4. **Verify.** Within 2 minutes the relaunched worker must run the new hash and heartbeat without
   errors. `worker cache-check` also runs, but it checks the cache, not the worker, so it only
   counts alongside the heartbeat check.
5. **Roll back** on failure: while holding `host.lock`, restore the previous wrapper and repoint
   the `recipes` symlink to `recipe-release.py`'s `previous`, restart, verify, and quarantine the
   failed hash.
6. **Report.** Write a receipt (before, after, verification, rollback) under
   `/Users/Shared/cmux-build-fleet/receipts/rollout/`.

The agent never interrupts a job and never deletes the previous two releases.

**Quarantine.** Failed hashes are recorded per host. The agent never reinstalls a quarantined
hash until `channels.json` names a different hash for that ring (#149 item 9).

**`worker.env` is data.** The agent writes it; the wrapper parses it strictly as `KEY=VALUE`
lines against an allowlist (`CMUX_CI_BUILD_SLOTS`, the controller token path, and similar) and
never sources it.

### 4. Promotion is automatic and gated

A scheduled workflow on a fleet-side runner (it needs the LAN mirror and receipts) reads canary
receipts and controller job history. It opens a one-line `channels.json` PR moving the canary
hash to `stable` once the canary has:

- run the new release for at least 30 minutes,
- completed at least 2 real jobs without a worker-attributed failure, and
- rolled back zero times.

`stable` may only point at a hash with passing canary receipts; a check on the repository
enforces this for every `channels.json` change, human or bot. The PR is opened with a dedicated
bot token that can open PRs but is not exempt from branch protection, and auto-merge only
succeeds when that receipt check passes. The bot can promote only what the canary already ran
from a reviewed commit, so it cannot introduce unreviewed code.

Any canary rollback, or a jump in worker-attributed failures on stable hosts, opens a PR setting
`paused: true` and an issue with the receipts.

### 5. Compatibility rules make order irrelevant

A new worker must run against an old controller and the reverse. From #575 and #584, as a rule
for every build-fleet PR:

- a new capability is announced by the controller (`/v1/capabilities`) and used by clients only
  when present;
- a new job state or enum value is also capability gated. `artifact_pending` is the
  counterexample: it was added as a new job state without a gate, so old clients meet a state
  they do not know;
- a job that needs new worker behaviour carries a worker label (`source-patch-v1`) that only new
  workers advertise;
- new heartbeat or job fields are optional in both directions.

Release hash and `credential_rejected` are reported through agent receipts and
`cmux_mini_probe.sh`, not controller heartbeats: a heartbeat rejected with `401` cannot carry
them, and an old controller drops unknown fields.

### 6. The coordinator is excluded

cmux-lawrence is in `never_touch`. The agent is not installed there and refuses it by name (as
`glaeda-fleet-cas-rollout` already does) and also by address, which that script does not check. Controller changes install only after its
owner approves a specific hash. Until then the capability rules keep new workers useful against
the old controller.

### 7. Security

- A mini accepts a hash only if its attestation signer is the release workflow path on
  `refs/heads/main` of `manaflow-ai/cmuxterm-hq`, verified offline against the bundle shipped
  with the release. A mirror or GitHub compromise alone cannot deliver a worker.
- `stable` only points at hashes with passing canary receipts (section 4).
- Each mini needs a GitHub credential to read the private repository's manifest, channels and
  release assets. Which credential, its scope, and how it reaches the mini is open (below).

### 8. Credentials

The controller token moves from each plist into
`/Users/Shared/cmux-build-fleet/secrets/controller.token` (already used by newer hosts).
Rotation delivers the new token through the agent with its own per-host credential, never
through the release. A worker that gets `401` for 10 minutes records `credential_rejected` in
its receipt, which the probe and fleet status surface.

## What this replaces

| Today | With the agent |
| --- | --- |
| Leo pastes deploy commands per host | Merge the PR; the canary picks it up within minutes |
| Busy hosts skipped and forgotten | The agent retries each loop until the host is idle |
| Hand-built binaries behind per-host wrappers | One release hash per ring; unknown wrappers blocked |
| Settings edited in root-owned plists | `bin/worker.env` from the manifest |
| Silent workers found days later | Probe and fleet status show hash and credential state |

## Milestones

1. **Release workflow** in cmuxterm-hq: build, hash, attest, publish with bundle; mirror from a
   fleet-side runner.
2. **Observe, no agent.** Extend `cmux_mini_probe.sh` and `glaeda-fleet-status`: follow the
   wrapper to its target, hash it, compare with `channels.json`, report `credential_rejected`, and
   record owner and mode of `bin/`, `bin/worker`, `recipes/` and `secrets/` per host.
3. **Agent apply on the canary ring**, on the journaled updater, with reservation, verification,
   rollback and quarantine. Move `CMUX_CI_BUILD_SLOTS` and the token path into `worker.env`.
4. **Automatic promotion** to stable, the receipt check, the pause switch, and the failure issue.
5. **Credential rotation through the agent.** Blocked on the per-host secrets credential.

## Open questions

- The GitHub credential each mini uses to read `manaflow-ai/cmuxterm-hq` (deploy key, GitHub App
  installation token, or mirror-only reads).
- Where the agent's per-host credential for fetching secrets comes from at enrollment.
- Whether enrollment (agent daemon plus optional sudoers rule) can reuse
  `cmux-fleet-bootstrap-macos`, which already needs an administrator once.
- Soak thresholds (30 minutes, 2 jobs) are guesses; set them from the first month of receipts.
