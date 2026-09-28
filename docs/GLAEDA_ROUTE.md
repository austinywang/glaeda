# Glaeda CI routing

Glaeda decides where cmux CI jobs run (glaeda#1174). This page is the contract for what
Glaeda publishes, how a workflow asks it for a `runs-on` target, how reservations and rescue
work, and how an operator installs it. `scripts/glaeda-route` is the executable truth.

**Order, for every job type:** `std` minis (48 GB), then `light` minis (16 GB), then Blacksmith,
then GitHub-hosted. Priority orders jobs inside the owned pools: pull request and dev work first;
nightly and cache warming only on idle slots, never ahead of pull request work waiting for one.
**Trust:** only a same-repository run on the repository itself (pull request from a branch of the
repository, push, merge group, schedule, dispatch), attempt 1, ever gets an owned machine. Fork
runs never do, and never even ask.

## Design

```
 fleet (SSH probe) --+                                   +--> vars.GLAEDA_POOL_STATE (cmux)
                     +--> glaeda-route agent (every 20 s)+
 GitHub runners -----+     (always-on operator host,     +--> watch ledger: release, rescue
                            outbound HTTPS only)                 (cancel + re-run on overflow)

 cmux job step: ask glaeda (composite action, small Linux step)
   fork / retry / GLAEDA_ROUTE off / no state  -> default (Blacksmith), no call at all
   read vars.GLAEDA_POOL_STATE (<= 60 s old)   -> stale or missing: default
   read ledger, decide, compare-and-swap commit -> owned pool label, or default
```

Three pieces, and no new inbound endpoint anywhere:

1. **Publish.** `glaeda-route agent` (or `publish`) observes the fleet and the repository's
   runners and writes the pool state to a repository variable (below).
2. **Route.** A step inside the cmux job that already picks the macOS pool runs
   `glaeda-route route` from a pinned Glaeda commit. It decides from the pool state, subtracts
   the slots other runs already reserved, and reserves its own slots in the **ledger** with a
   compare-and-swap commit. Any failure answers with the caller's default.
3. **Rescue.** The same agent watches every live reservation through the GitHub API. It releases
   a reservation when its jobs start or its run ends. A pool job queued with no runner for 30 s,
   or refused by the runner's job-started hook, gets its run cancelled and re-run; the re-run is
   attempt 2, which never takes an owned machine, so it lands on overflow as a whole run.

### How the issue's open questions were resolved

- **Where the route decision runs.** Inside the workflow, on the Linux runner the asking job
  already uses. Nothing listens on the fleet, and nothing runs on cmux-lawrence. The only
  always-on piece is the agent, which makes outbound GitHub API calls only; it can run on any
  always-on host with the fleet manifest and SSH to the members.
- **Auth between the route step and Glaeda.** There is no endpoint to authenticate to. The route
  step reads the pool state from `vars` (no token) and writes the ledger with a GitHub App token
  that can touch only the private state repository. A fork run gets neither variables nor
  secrets, and the composite action returns the default before any call.
- **Latency.** The route step costs about four GitHub API calls (two reads, three writes when it
  reserves) and runs inside the existing `changes` job, so it adds seconds to a job that already
  runs, and no extra job. JIT runners are Phase 2 (below), not a prerequisite.
- **Correct reservations without a server.** The ledger is one JSON file on one branch. A write
  is a new commit whose parent is the head the writer read, then a fast-forward-only ref update
  (`force: false`). If anyone moved the branch in between, GitHub rejects the update (422) and
  the writer reads again. Two runs therefore can never both take the last free slots.
- **Stale state.** The pool state is used only while its oldest input is at most 60 s old. A
  stopped agent, a hung probe, or a missing variable routes everything to Blacksmith.

Alternatives considered: a public HTTPS endpoint that verifies GitHub OIDC tokens is the cleanest
auth (no secret in cmux) and could hold reservations in one process, but it is a new always-on
inbound service that must live somewhere other than cmux-lawrence; it stays the upgrade path if
the ledger's API cost ever matters. A Tailscale action with an ephemeral key would put a tailnet
key in cmux secrets and still need an endpoint on the fleet.

## Pool state (`glaeda-pool-state/v1`)

`glaeda-route publish --yes` writes one compact JSON document to the repository variable
`GLAEDA_POOL_STATE` on `manaflow-ai/cmux`. A workflow reads it as `vars.GLAEDA_POOL_STATE`,
with no token and no tailnet. A fork pull request's run gets no repository variables, so it
never sees the document and must never route to an owned pool anyway.

```json
{
  "schema": "glaeda-pool-state/v1",
  "repo": "manaflow-ai/cmux",
  "generated_at": "2026-09-24T15:40:20Z",
  "observed_at": "2026-09-24T15:40:12Z",
  "max_age_seconds": 60,
  "order": ["glaeda-std-xcode-26.6", "glaeda-light-xcode-26.6"],
  "pools": {
    "glaeda-std-xcode-26.6": {
      "class": "std", "xcode": "26.6", "rank": 0,
      "declared": 11, "conforming": 10, "reserved": 1, "locked": 0,
      "runners": 11, "online": 11, "busy": 2, "idle": 8
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `generated_at` | When the publisher assembled the document (RFC 3339, UTC). |
| `observed_at` | The oldest observation the document rests on: the fleet probe or the GitHub runner listing. Freshness is measured from here. |
| `max_age_seconds` | The publisher's promise. Readers use their own limit, 60 s by default. |
| `order` | Owned pools in routing order: class `std` (48 GB minis), then `light` (16 GB), newest Xcode first within a class. Blacksmith and GitHub-hosted come after, and belong to the caller. |
| `declared` | Members the fleet manifest puts in the pool. |
| `conforming` | Members the fleet probe saw reachable with the pool's exact Xcode, not reserved, not holding the fleet host lock. |
| `reserved` | Members with an active `glaeda-mini-fleet reserve` marker (0 until the probe reports it). |
| `locked` | Members whose fleet host lock was held at observation (0 until the probe reports it). |
| `runners`, `online`, `busy` | Self-hosted runners on the repository that carry the pool label, as GitHub lists them. |
| `idle` | Runners that are online, not busy, and belong to a conforming member (`<member>-glaeda`). The routable capacity at `observed_at`. |

The document carries counts only, never host names or node ids, because cmux workflow
logs are public.

### Reader rules

A reader may route a job to an owned pool only when every one of these holds; otherwise the
job keeps its Blacksmith route. `glaeda-route check --state FILE` applies the same rules.

1. `schema` is exactly `glaeda-pool-state/v1`. A future `v2` is a different document.
2. `repo` is the reader's repository.
3. `generated_at` and `observed_at` parse as dated UTC times, neither in the future by more
   than 30 s, and `observed_at` is at most 60 s old.
4. `order` and `pools` name the same labels, every label is `glaeda-<std|light>-xcode-<version>`,
   and every count is a non-negative integer, with `idle` and `busy` at most `online`.
5. The run is trusted: a same-repository pull request, or a push, merge group, schedule or
   dispatch on the repository itself, and attempt 1. Fork runs never read the document.

`idle` is a snapshot. A reader that places more than one job must count what it has already
placed since `generated_at`, and must still expect a job to queue: GitHub never re-routes a
queued job, so a rescue that re-runs a stuck run on Blacksmith is required (glaeda#1174).

### Overflow load (`overflow`, optional)

The agent also publishes what the Blacksmith pools are doing, so cmux's Blacksmith picker reads
live counts instead of the queue janitor's 10 to 30 minute old snapshot:

```json
"overflow": {"observed_at": "...", "complete": true,
             "pools": {"blacksmith-6vcpu-macos-26": {"running": 9, "queued": 2}}}
```

- `running` and `queued` count the jobs of the repository's queued and in-progress runs, per the
  first `blacksmith-*` label each job asks for.
- A run's jobs are listed again when the run's `updated_at` moves or its listing is 150 s old
  (cache in `~/.local/state/glaeda/route/overflow-jobs.json`), at most 10 runs a tick, oldest
  first, and not while the credential has under 1,500 requests left, so the rescue watch keeps
  its budget. Anything skipped keeps its last listing and sets `complete` to false.
- A failed listing leaves the section out; the owned pools still publish. Readers without the
  section keep their own source.
- On by default for `agent`; `state` and `publish` take `--overflow-load`.

The counts say where jobs are waiting, not how many machines a pool has: Blacksmith ran at most
about 5 concurrent jobs on `blacksmith-12vcpu-macos-26` and 18 on each 6vcpu pool (2026-09-24),
so a reader needs its own capacity per label.

Compatible changes (new fields, new pools) keep `v1`. Removing or redefining a field bumps the
schema, and the publisher then writes both documents until readers move.

## Asking for a route

```bash
glaeda-route route --kind ci-macos --priority pr --slots 3 \
  --xcode /Applications/Xcode_26.6.app --default blacksmith-6vcpu-macos-26
```

It reads the request from the workflow environment (`GITHUB_REPOSITORY`, `GITHUB_EVENT_NAME`,
`GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT`, plus `HEAD_REPO` and `HEAD_SHA` from the pull request),
the pool state from `GLAEDA_POOL_STATE`, and the ledger token from `GLAEDA_LEDGER_TOKEN`. It
writes `runs_on`, `owned`, `pool`, `reservation` and `reason` to `GITHUB_OUTPUT`, adds a step
summary, and always exits 0.

The decision:

1. Refuse owned machines to anything untrusted (above), to attempt 2 and later, and to a request
   with no known head commit or no Xcode. Answer `--default`.
2. Apply the reader rules to the pool state. Answer `--default` if it fails them.
3. A run asking again (same run, attempt and kind) gets its existing live reservation back.
4. Consider only the pools whose Xcode is the one the jobs pin, in `order`.
   `free = idle - slots still pending in reservations on that pool`. A held reservation's
   slots count until its jobs start; a slot whose job started, or a reservation that ended,
   after the pool state was observed still counts, because that state shows the runner idle.
   A rescuing reservation keeps its slots. A ledger already holding 400 live reservations
   answers the default.
5. `pr` and `dev`: the first pool with `free >= slots`. `nightly` and `warm`: only while no
   `pr` or `dev` reservation is still pending anywhere, and only a pool with
   `free >= slots + 1`, so one slot always stays open for a pull request.
6. Reserve by compare-and-swap; on a lost race, read and decide again (4 tries, 12 s budget).

A run's macOS jobs all land on one pool (cmux#14163), so `--slots` is the number of owned
machines the run uses at once.

## Ledger (`glaeda-route-ledger/v1`)

`ledger.json` on branch `ledger` of the private repository `manaflow-ai/glaeda-route-state`:

```json
{"schema": "glaeda-route-ledger/v1", "reservations": [
  {"id": "manaflow-ai/cmux#123.1/ci-macos", "repo": "manaflow-ai/cmux", "run_id": 123,
   "run_attempt": 1, "head_sha": "<40 hex>", "kind": "ci-macos", "priority": "pr",
   "pool": "glaeda-std-xcode-26.6", "slots": 3, "started": 1, "state": "held",
   "created_at": "2026-09-24T15:40:21Z", "hold_until": "2026-09-24T15:55:21Z"}]}
```

States: `held` (watched until its run ends; slots pending until its jobs start), `rescuing`
(a rescue was decided and written; the cancel follows), and the finished `released`, `rescued`
and `failed`, kept for an hour after `ended_at`. Live reservations are never pruned. A hold stops
counting against capacity after 15 minutes even if no agent saw it, so a stopped agent cannot
strand capacity for long.

The ledger is a claim, not authority. The agent cancels or re-runs a run only when GitHub itself
confirms it: the run is on the routed repository, a trusted event, not from a fork, at the
reserved head commit and attempt, and (for a pull request) the pull request is open at that head.
A forged ledger entry can therefore only rescue a run that really is stuck on an owned pool.

## Rescue

Each agent tick, for every live reservation on the routed repository:

- `held`: list the run's jobs for that attempt that carry the pool label and count the ones with
  a runner as started. Keep watching until the run finishes, is re-run by someone else, or two
  hours pass, so pool jobs that start late (a `needs` chain) are still covered. A job queued with
  no runner for 30 s (measured from the latest of its creation, the reservation, and the first
  time the agent saw it waiting, so a job created before its `needs` finished never counts as
  late), or a job the runner's job-started hook refused (failed on a runner before any real
  step), decides a rescue once GitHub confirms the run and the pull request is open at the same
  head. The agent writes `rescuing` first and cancels second, in the same tick, so a lost ledger
  write never leaves a cancelled run without its re-run.
- `rescuing`: every effect is checked against GitHub again, because the ledger is only a claim.
  If this agent has not cancelled the run (its own record, in the agent's state file), it
  cancels only a trusted run that is still stuck. If it has: force-cancel after 90 s, give up
  (`failed`) after 180 s. Once the run finished, it is re-run only if this agent cancelled it,
  it ended `cancelled`, and (for a pull request) the pull request is still open at that head (a
  push during the cancel starts its own run, which must not be overwritten). The re-run is
  attempt 2 and lands on overflow as a whole run.

This is cmux#14234's rule, moved into Glaeda and driven by the ledger instead of a marker
artifact and a watcher workflow per run.

## Operator runbook

### One-time setup (Leo)

1. **State repository.** `gh repo create manaflow-ai/glaeda-route-state --private --add-readme`.
2. **Ledger App** (`glaeda-route-ledger`), installed on `manaflow-ai/glaeda-route-state` only:
   Contents read and write, Metadata read. Nothing else, and never installed on cmux (an App's
   permissions apply to every repository of its installation, so Contents write on cmux would
   let it push code). Store its client id as the cmux repository variable
   `GLAEDA_LEDGER_APP_CLIENT_ID` and its private key as the cmux repository secret
   `GLAEDA_LEDGER_APP_PRIVATE_KEY`. Also copy the key to the agent host.
3. **Agent App** (`glaeda-route-agent`), installed on `manaflow-ai/cmux` only: Actions read and
   write (jobs, cancel, re-run), Administration read (runners), Variables read and write
   (`GLAEDA_POOL_STATE`), Pull requests read, Metadata read. Its key lives on the agent host only.
4. **Keys on the agent host:** `~/.config/glaeda/route/agent.pem` and `ledger.pem`, `chmod 600`,
   owned by the agent's user. Never in argv, logs, or a repository.
5. **Create the ledger:**
   `glaeda-route init-ledger --ledger-app-id <ID> --ledger-app-key-file ~/.config/glaeda/route/ledger.pem --yes`
6. **Dry run the publisher**, then check it:
   `glaeda-route publish --app-id <ID> --app-key-file ~/.config/glaeda/route/agent.pem`
7. **Run the agent every 20 s** on the agent host (launchd shown; a systemd timer works the same):

```xml
<!-- ~/Library/LaunchAgents/com.teamleaderleo.glaeda.route-agent.plist -->
<plist version="1.0"><dict>
  <key>Label</key><string>com.teamleaderleo.glaeda.route-agent</string>
  <key>ProgramArguments</key><array>
    <string>/usr/bin/python3</string><string>/Users/USER/glaeda/scripts/glaeda-route</string>
    <string>agent</string>
    <string>--app-id</string><string>AGENT_APP_ID</string>
    <string>--app-key-file</string><string>/Users/USER/.config/glaeda/route/agent.pem</string>
    <string>--ledger-app-id</string><string>LEDGER_APP_ID</string>
    <string>--ledger-app-key-file</string><string>/Users/USER/.config/glaeda/route/ledger.pem</string>
  </array>
  <key>StartInterval</key><integer>20</integer>
  <key>StandardOutPath</key><string>/Users/USER/.local/state/glaeda/route/agent.log</string>
  <key>StandardErrorPath</key><string>/Users/USER/.local/state/glaeda/route/agent.log</string>
</dict></plist>
```

   The agent host needs the fleet manifest (`~/.config/glaeda/mini-fleet.json`) and SSH to the
   members, because it runs `glaeda-mini-fleet pools`. Never cmux-lawrence.
8. **Turn it on in cmux:** set the repository variable `GLAEDA_ROUTE=1`. Leave
   `CI_PR_POOL_OWNED` (the cmux stopgap) unset: the two must not route at once.

### Turning it off

Delete or change `GLAEDA_ROUTE`. Every run goes back to the cmux picker and Blacksmith at once.
Stopping the agent has the same effect within 60 s (the pool state goes stale). Held
reservations lapse on their own.

### Checks

- `glaeda-route check --state <(gh variable get GLAEDA_POOL_STATE -R manaflow-ai/cmux)`: usable?
- `gh api repos/manaflow-ai/glaeda-route-state/contents/ledger.json?ref=ledger --jq .content | base64 -d`
- The `changes` job's step summary names the route, the reason and the reservation.

### API cost

The route step: 2 reads, plus 3 writes when it reserves. The agent, per 20 s tick: 1 runner
listing, 2 run listings plus one job listing per run whose `updated_at` moved (overflow load),
1 variable write, 2 ledger reads, 2 reads per live reservation, 3 writes when something
changed. The two Apps have separate rate budgets (5,000 requests an hour each).

## Replaces in cmux

Once `GLAEDA_ROUTE=1` has run cleanly, cmux deletes the stopgap: the owned-pool parts of
`scripts/ci/pr_runner_pool.py` (owned labels, `CI_OWNED_POOL_SLOTS`, jobs-per-run headroom,
`persistent` output) and of `scripts/ci/queue_janitor.py` (owned-label counting), plus
`scripts/ci/owned_pool_rescue.py`, `.github/workflows/ci-owned-pool-rescue.yml`, the marker
upload in `ci.yml`, their tests (`tests/test_ci_owned_pool_rescue.py` and the owned cases in
`tests/test_ci_pr_runner_pool.py`), and the owned-pool section of `docs/ci-runners.md`. The
Blacksmith spreading in `pr_runner_pool.py` (cmux#14205) stays: it picks among Blacksmith pools
when Glaeda answers with the default.

## Phase 2: just-in-time runners

Persistent runners are why a routed job can still queue: GitHub hands the job to whichever
matching runner is idle, and a busy one makes it wait. Phase 2 removes that:

- The agent (or a small per-member agent) registers one JIT runner per reservation with
  `POST /repos/{repo}/actions/runners/generate-jitconfig` (name `glaeda-<member>-<reservation>`,
  a unique label `glaeda-r-<reservation id hash>`, runner group default) and starts it on the
  chosen member under the host lock. The route step answers with that unique label, so the job
  can only land on that one machine, and the runner exits after one job.
- Reservations become per member instead of per pool; `idle` becomes the members the agent can
  start a JIT runner on right now. Rescue shrinks to "the JIT runner did not come online within
  30 s": deregister it and re-run on overflow.
- Credentials: the agent App gains Administration write on cmux (JIT configs), or each member
  holds its own App key and mints only its own configs. The JIT config is a secret passed to the
  runner on stdin, never argv.
- The persistent `glaeda-<class>-xcode-<version>` runners stay registered as the fallback until
  JIT has run a week without a rescue.

The Linux VM runners in cmuxterm-hq#573 already use this pattern (`scripts/owned_linux_jit_task.py`).
