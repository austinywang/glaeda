# Fleet status

`scripts/glaeda-fleet-status` answers "what is wrong with the fleet, and who fixes
it" in one typed, bounded JSON document. Humans read the page rendered from it;
agents read the JSON. It is slice 9 of the fleet roadmap (cmuxterm-hq#573,
"Seeing and fixing the fleet").

It is read-only. Collection runs SSH probes, `gh api` reads and HTTP GETs; nothing
on any machine changes, except that `glaeda-disk`, like any report run of it, may
rewrite its own size snapshot (`~/.cache/glaeda-disk/sizes.json`).

## Run it

```bash
scripts/glaeda-fleet-status \
  --github-runners orgs/OWNER --queue-repo OWNER/REPO \
  --controller-status ~/Projects/cmuxterm-hq/build-fleet/status.py \
  --cache t5=http://CACHE_HOST:8787/healthz \
  --out fleet-status.json --html fleet-status.html
```

Every source except the manifest is optional. Without a flag the source shows as
`not_collected`, and a source that fails shows as `unavailable` with its error.

| Source | From | Flag |
| --- | --- | --- |
| manifest | `glaeda-mini-fleet` manifest (members, class, roles, `never_touch`) | `--manifest` |
| check, preflight | one `glaeda-mini-fleet` probe per member | on; `--no-fleet-probe` |
| controller | cmuxterm-hq `build-fleet/status.py --json` (needs `CMUX_CI_TOKEN_FILE`) | `--controller-status`, `--controller-json` |
| runners | `gh api orgs/X/actions/runners` or `repos/X/Y/...` | `--github-runners` |
| queue | queued jobs of queued and in-progress runs, 25 runs at most | `--queue-repo` |
| cache | HTTP status of each endpoint | `--cache NAME=URL` |
| lima | `limactl list --json` per member, never on `never_touch` hosts | on; `--no-lima` |
| disk | one read-only SSH probe per member, never on `never_touch` hosts (below) | on; `--no-disk` |
| jobs | the tail of each member's `~/Library/Logs/glaeda-cmux-jobs.jsonl` (below) | on; `--no-jobs` |

Split collection from building to reuse or replay a view:

```bash
scripts/glaeda-fleet-status collect --sources-out sources.json
scripts/glaeda-fleet-status build sources.json --out status.json
scripts/glaeda-fleet-status render status.json --html status.html
```

Exit status: 0 no error findings, 1 at least one, 2 the tool failed.

## The document

`schema: glaeda-fleet-status/v1`, with `generated_at`, `sources` (state, observed
time, age, error per source), `fleet` (counts), `members` and `findings`.

Each finding:

```json
{
  "id": "controller.stale@cmux-austin-mini-0",
  "source": "controller",
  "code": "stale",
  "severity": "error",
  "member": "cmux-austin-mini-0",
  "summary": "controller: stale; ...",
  "observed_at": 1790260594,
  "action": {
    "id": "restore_worker_heartbeat",
    "who": "person",
    "safe_to_apply": false,
    "command": null,
    "note": "worker heartbeat is older than 120 s; ..."
  }
}
```

- `who` is `agent` (the SSH login user can run it), `password` (needs sudo) or
  `person` (a decision, an account, or a physical step).
- `command` runs from the operator machine, already wrapped in `ssh -- HOST ...`
  (`ssh -t --` when it needs a password), and only for members named in the
  manifest.
- A fix becomes a command only if it fits a closed grammar: a known program or a
  path, then plain words, with no shell metacharacters and no prose words.
  Anything else becomes `note`. Fix text comes from the members' own probes, so
  a hostile member cannot plant shell syntax in a command.
- A command that carries anything secret-looking, a home path, or more than 400
  characters is withheld: the action goes to a person with a note, because
  redacting it would change what runs.
- `safe_to_apply` is true only for read-only commands: refreshing this document
  and listing a runner's launchd job. Everything that changes a machine is false,
  including `glaeda-disk` (it refreshes its own snapshot) and
  `glaeda-mini-fleet apply` (it runs every typed fix for a host, `add_key`
  included).
- Members in `never_touch` get no command and no agent action.
- Severity: `error` means broken now (unreachable, stale or unhealthy worker,
  offline runner, a queued job no online runner can take, cache down). `preflight`
  blockers are `warn`: they block onboarding, not today's builds.

A queued job whose labels include `self-hosted` and match no online runner in
scope is its own `queue.no_eligible_runner:<labels>` finding, naming the members
whose runners carry those labels. Hosted and Blacksmith labels are not judged. The
queue is judged only against a fresh, complete runner list; otherwise it is a
`queue.unjudged` info finding.

Findings from `check` and `preflight` that name the same command for the same
member merge into one, keeping the more severe and listing the other in `also`.

## Disk and caches

The `disk` probe reads, per member:

- `~/.local/bin/glaeda-disk --json --top 0`: free and total bytes of the data
  volume, its pressure threshold (`low`), and cache bytes per glaeda-disk family
  with its owner and whether that owner is retired. It runs only over an existing
  size snapshot (without one it would measure every family first) and with a max
  age far past any snapshot, so a status probe never starts glaeda-disk's
  background re-measure; `sizes_at` says how old the sizes are. Item paths never
  leave the member: only family totals do. `cache_bytes` leaves out
  `cargo-target`, which is also counted inside its checkout. The JSON also carries
  `accounting`: df-used, measured bytes, unmeasured bytes, and a cached top-level
  volume walk, so a zero reclaimable total still exposes space outside the catalog.
- the last 48 lines of `~/Library/Logs/glaeda-fleet-cas-prune.jsonl`: the newest
  line (result, role, node store and local CAS bytes) and the newest line with a
  gc outcome (gc runs once a day; `not due` is not an outcome) with the dry run's
  count of kept entries naming no stored object, never its text.
- the prune LaunchAgent's install time, and whether the host has fleet-cas
  (`/Users/Shared/cmux-build-fleet/xcode/fleet-cas.env`).
- bytes and deletions with outcome `reclaimed` in the last 24 h, from
  `~/Projects/recovery/disk-reclaim/receipts.jsonl`.

A missing log or receipts file is simply absent from the member's `disk` object.
Findings, all `warn` unless noted:

| Code | When | Action |
| --- | --- | --- |
| `pressure` | free is below glaeda-disk's pressure threshold | `glaeda-disk` report (agent, not safe: it may rewrite its snapshot) |
| `retired_owner:<family>` | a family whose owner is retired holds more than 5 GiB | person: glaeda-disk only reports such families |
| `prune_failed`, `gc_failed`, `gc_skipped` | the last prune or gc result starts with `failed` or `skipped: kept entries` | person: read the prune log |
| `prune_silent` | fleet-cas host, no prune line in 3 h (or none 3 h after the job was installed) | `launchctl list com.teamleaderleo.glaeda.fleet-cas-prune` (safe) |
| `prune_missing` | fleet-cas host without the prune LaunchAgent | person: `glaeda-mini-setup --apply` |
| `summary`, `no_glaeda_disk`, `unreadable`, `probe_failed` (info) | otherwise, a missing tool or snapshot, or a probe that timed out | `glaeda-disk` report, a person to install it, or a refresh |

No disk action ever deletes. The text output adds one `disk:` line per member and
the page a `disk` column, for example:

```text
cmux7s-mac-mini  free 158.6/460.4 GiB (low 69.1); caches 218.8 GiB [user-cache 173.8, hq-build-fleet-cache 21.5, tmp 20.2], RETIRED 21.5 GiB; prune deferred: a build is running 19m ago; gc ran 19m ago
```

## Runner jobs

The runner hook samples the host while every admitted job runs and writes one
`glaeda-cmux-job/v1` line per job to `~/Library/Logs/glaeda-cmux-jobs.jsonl`
([CMUX_MINI_RUNNER.md](CMUX_MINI_RUNNER.md), section 2, step 3). The `jobs` probe tails that
file, reads only its `completed` lines (or lines with no `event`) and skips `started` and
`refused` ones, and keeps the last 24 h: how many jobs ran, how many ran `contended`, and
the reasons for the newest five contended ones. A job is contended when work
outside the mini's runner jobs averaged at least 2 cores and at least 15% of
them; when the host CPU (iostat) averaged 90% busy or more while other runner
jobs and outside work together held a quarter of the cores; when processes in
uninterruptible wait (ps state U: disk, VM faults or memory-compressor stalls)
averaged at least 4 and 30% of the cores; or when pmset reported a thermal limit. The load average
is recorded but is not a reason on its own: a compile alone pushes it past the
core count. That is the question a slow CI job raises first: was it the change,
or the host?

| Code | When | Action |
| --- | --- | --- |
| `contended` (warn) | at least one job in 24 h ran contended | `tail -n 20 ~/Library/Logs/glaeda-cmux-jobs.jsonl` (safe) |
| `console_locked` (warn) | the console session is screen-locked | a person unlocks it and turns off the lock |
| `console_no_user` (warn) | nobody is logged in at the console | a person logs the console user in |
| `testmanagerd_stuck` (warn) | in 24 h the hook refused an XCTest job because the user's testmanagerd outlived SIGKILL ([CMUX_MINI_RUNNER.md](CMUX_MINI_RUNNER.md), 2h2); the probe keeps those `refused` lines apart | `/usr/bin/pgrep -lf testmanagerd` (safe); a person decides on a logout or reboot |
| `probe_failed` (info) | the probe timed out | a refresh |

The same probe reads the console session from `ioreg -n Root -d1` (member field
`jobs.console`: `state` `unlocked`, `locked` or `no_user`, and the user name). The runner
hook refuses GUI jobs while it is locked or has no user (cmuxterm-hq#757), so such a mini
runs no GUI tests until a person fixes it.

The text output adds one `jobs (24 h):` line per member and the page a column
(`console LOCKED` or `console NO USER` at the end when the console cannot run GUI jobs):

```text
cmux13s-mac-mini  41 jobs, 1 contended; latest macos-compile-admission: outside processes averaged 5.1 cores (top: zig (cmux))
```

## Bounds and privacy

At most 128 members, 500 findings, 24 disk families per member, 240 characters
per text field, 400 per command, 512 KiB per document. Member fields are typed (numbers stay numbers,
text is cleaned and capped). Over the byte limit, info then warn findings drop
first and `fleet.truncated_findings` counts them; counts always describe the
findings that remain. An error is never dropped to fit: the tool fails instead.
A malformed sources bundle fails with exit 2, never a traceback.
Secrets (`ghp_`, `github_pat_`, `Bearer`, `token=`) are redacted and home paths
become `~`. Child processes get an allowlisted environment.

## How an agent uses it

1. Read `sources`; refresh anything `stale` or `unavailable` first.
2. Apply actions with `who: agent` and `safe_to_apply: true`.
3. Hand every other action to a person as its `command`, or its `note` when there
   is no command, grouped by `who`.

Scheduled regeneration, and typed fixes emitted by `glaeda-mini-fleet` itself
instead of parsed fix text, are follow-ups.
