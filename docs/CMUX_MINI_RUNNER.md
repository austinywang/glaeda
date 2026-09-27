# cmux Mac mini runner runsheet

Turns one cmux Mac mini into a persistent GitHub Actions self-hosted runner for
`manaflow-ai/cmux`, labelled `self-hosted,macOS,ARM64,glaeda-mini`. The tool is
`scripts/glaeda-cmux-runner`, also reachable as `scripts/glaeda-mini-setup --runner`.
Plan is the default and changes nothing; `--apply` acts; `--uninstall --apply` undoes it.

Registering a runner does not move any job. Jobs move only when a repository
variable points at the `glaeda-mini` label (step 4), and deleting that variable moves
them back to Blacksmith.

## 1. Prerequisites (operator, on the mini)

- Apple silicon Mac mini, logged in as the build user, with the Command Line Tools
  (`xcode-select --install`) and Homebrew.
- `scripts/glaeda-mini-setup --apply` already run, so `glaeda-disk` is on
  `~/.local/bin` (optional: without it the hooks skip disk pressure).
- Either `gh` on the mini, logged in as a `manaflow-ai/cmux` admin (runner
  registration needs admin): `brew install gh && gh auth login`. Or no `gh` on the
  mini at all, with the token minted on the operator's machine (section 2b). The
  second keeps admin credentials off a shared build host.
- A glaeda checkout: `git clone https://github.com/teamleaderleo/glaeda ~/Projects/glaeda`.
- The cmux Xcode pin and `pmset` settings from `glaeda-mini-setup` operator steps, so
  jobs can build once routed. cmux jobs select Xcode by path from repository
  variables (`CMUX_CI_XCODE_APP_PR`, `..._MACOS_15`, `..._MACOS_26`), not through
  `xcode-select`, so that exact path must exist as a real directory (not a symlink).
  Pass it as `--xcode-app /Applications/Xcode_26.6.app` and the plan warns when it
  is missing. `gh variable list --repo manaflow-ai/cmux | grep XCODE` shows the pins.
- Automatic login for the build user (`sysadminctl -autologin status`). The runner is
  a LaunchAgent in that user's GUI session, so it starts again after a reboot only
  when the user logs in automatically.

## 2. One command

Look at the plan first, then apply:

```bash
cd ~/Projects/glaeda
scripts/glaeda-mini-setup --runner            # plan, no side effects
scripts/glaeda-mini-setup --runner --apply    # install, register, start
```

Useful flags: `--name NAME` (default `<short hostname>-glaeda`), `--labels a,b` or `--manifest M --member NAME` (section 2c)
(extra labels), `--org ORG [--group GROUP]` instead of the default
`--repo manaflow-ai/cmux`, `--runner-dir DIR` (default `~/actions-runner-glaeda`),
`--runner-version V --runner-sha256 HEX` to pin, `--replace` to take over an existing
registration with the same name, `--output json` for the receipt.

What `--apply` does:

1. Downloads the latest official `actions/runner` `osx-arm64` tarball, verifies its
   SHA-256 against the release notes and the GitHub asset digest (both must agree),
   and unpacks it into `~/actions-runner-glaeda`.
2. Gets a one-time registration token with `gh api -X POST .../registration-token`
   and runs `config.sh --unattended` (persistent, not ephemeral, so `_work` keeps hot
   state). The token is read from gh's stdout into memory and passed to `config.sh`
   only as `ACTIONS_RUNNER_INPUT_TOKEN` in that child's environment, which the runner
   reads and clears. It is never in an argv, a file, a log, or the output.
3. Writes the job hooks into `~/actions-runner-glaeda/glaeda-hooks/`:
   - job-started admits only `push`, `pull_request`, `merge_group`,
     `workflow_dispatch`, `schedule` and `workflow_run` (so `issue_comment`,
     `check_run` and the like, which can act for a fork PR with secrets, are refused).
     It refuses the job (exits 1 before any step runs) for
     `pull_request_target`, for any pull request whose head repository is a fork, is
     missing, or differs from the base repository, for a `workflow_run` from another
     repository, for any repository other than `manaflow-ai/cmux` (at org scope: outside the org, 2e2), and whenever the
     event payload is missing or unreadable. Admitted jobs then run
     `glaeda-disk --pressure --apply --top 0` with a 120 s timeout that never fails
     the job.
   - An admitted job is then held to the fleet host (`/Users/Shared/cmux-build-fleet`),
     so a PR job never lands on a mini that is busy with other work. It is refused fast,
     so the pool picker re-runs it elsewhere, when the host is reserved
     (`reservation.json`, `glaeda-reservation/v1` with integer Unix-second `since` and
     `until`, active while now is before `until`, whoever owns it; an unreadable or
     invalid marker also refuses, an expired one is ignored; parsed by
     `glaeda_reservation.py`, which the installer puts next to the hook), when free disk is below `--min-free-gib` (from the manifest's
     `disk.min_free_gib` with `--manifest`), or when another build holds `host.lock`, the
     `flock` that `with-host-lock` and the build worker take. Otherwise the job takes that
     lock: a small detached holder keeps it until job-completed releases it or the job's
     `Runner.Worker` exits, so fleet builds wait for the PR job and a crash cannot leave
     the lock held. A machine without `host.lock` skips the lock.
   - With `--manifest`, the hook also requires an eligible Glaeda node on its class
     toolchain (`--require-eligible --fleet-class <hardware> --toolchain-xcode <app>`,
     baked in from the member's `hardware` and first verified Xcode). It refuses with
     `refused: node not eligible (...)` unless the node status from the staged
     generation's `cmux_fleet.py status` (the command `glaeda-mini-enroll` uses) says
     `eligible` with `routingCandidateEligible` true and the build role eligible, the
     enrollment references `~/.config/glaeda/cmux-fleet/class-acceptance/<hardware>.json`
     (or this node is that receipt's source: its enrollment references no class receipt,
     and its node id, enrollment generation, Glaeda generation, toolchain generation,
     toolchain identity and semantic result all equal the receipt's),
     that receipt validates (its digest recomputed by the generation's
     `validate_class_acceptance`) and records all six toolchain strings, and `rustc`,
     `cargo`, `zig`, `xcodebuild` and `xcrun --show-sdk-version`, run from `$HOME` on the
     runner's own PATH (the job's PATH), print exactly those strings, all within one
     20 s budget. Other fleet jobs on the same mini (the cmux-ci dev-build worker) flip
     the global `rustup default`, so after taking the host lock the hook first sets it
     back to the receipt's toolchain, provided that toolchain is already installed.
     Under the lock no other fleet job runs, so the default holds for the whole job.
     `RUSTUP_TOOLCHAIN` is deliberately not used: it would override cmux's
     `rust-toolchain.toml` pins (DiffSidecar 1.88.0, cmux-tui 1.95.0, iroh-relay-minter
     1.91.0). The DiffSidecar pin itself is checked with `rustup run`. A refusal after
     the lock is taken releases it. The runner's PATH is exactly cmux's workload PATH
     (`/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`), so a tool under
     the user's home never shadows the accepted one. The default is only changed when
     `rustc --version` differs from the receipt, and `python3` on that PATH must be 3.13
     or newer (cmux's profile runner needs it). To see what a job would get without
     touching anything: `glaeda-cmux-runner-hook check --fleet-class <hardware>
     --toolchain-xcode <app>`, run with the workload PATH, is read-only (no lock, no
     rustup change). The check takes about a second and runs
     on every job, so a mini that drifts or loses eligibility stops taking PR jobs at
     once, and refusal recovery re-runs the job elsewhere.
   - With weighted capacity (`--capacity-units N`, baked in from the manifest class for
     every member, see 2d), the job takes a share of the mini instead of the whole host
     lock. It holds a shared `flock` on `host.lock` (so `with-host-lock`'s `LOCK_EX`,
     the build worker, waits for every PR job, and a held worker lock refuses them),
     plus units and tokens under `/Users/Shared/cmux-build-fleet/capacity`, all as
     `flock`s held by the job's detached holder, so a crash frees them. The cost comes
     from `GITHUB_JOB`: `macos-compile-admission` 2 units plus the `persistent-dd`
     token (one writer of the kept DerivedData at a time), `app-host-unit-tests` and
     `tests-build-and-lag` 1 unit plus the `gui` token (one console session),
     test-e2e's `build` (compile, then the selected tests in the console session) 2 units
     (it takes the `gui` token itself before its tests, with take-gui), test-e2e's `test` 1
     unit plus the `gui` token,
     `cli-product-tests`, `swift-package-tests` and the side lanes `cli-pipe-regressions`,
     `remote-daemon-macos-tests` and `claude-wrapper` 1 unit, and any other job counts
     as a compile. When units or a token are taken it refuses at once with
     `refused: capacity: ...`, which the refusal rescue re-runs elsewhere; it never
     waits. Because `flock` gives no preference to the exclusive waiter, it also
     refuses while another process (the build worker in `with-host-lock`) is waiting
     for `host.lock`, so the worker gets the host as soon as the running PR jobs end.
     Admissions on one mini are serialized for a moment (`capacity/admission.lock`),
     so two jobs never split the free units between them.
   - The toolchain check also requires `gh` on the job PATH: cmux's CI scripts call
     `gh api`, and a mini without it fails jobs midway instead of refusing them.
   - Every admitted job also gets a detached host sampler (`--no-telemetry`, or
     `GLAEDA_RUNNER_TELEMETRY=0` in the LaunchAgent's environment, turns it off). Every
     10 s it reads `ps` and the load average and splits the CPU between this job's
     process tree, the other runner slots' jobs, and everything outside them. It also
     takes one second of `iostat` (host CPU busy %, the ground truth that `ps`'s decaying
     %cpu misses for short-lived compiler processes, and disk MB/s) and counts processes
     running and in uninterruptible wait (disk or memory). Disk MB/s sums every disk
     iostat lists, mounted images included. Two more reasons mark a job contended: the
     host CPU averaged 90% busy while other runner jobs and outside work held a quarter
     of the cores, or uninterruptible waits averaged at least 4 and 30% of the cores. When the
     job ends it appends one line to `~/Library/Logs/glaeda-cmux-jobs.jsonl`
     (`glaeda-cmux-job/v1`): load mean and max, host CPU busy %, disk MB/s, the run queue,
     mean cores per bucket, the top five
     outside processes by estimated core-seconds, the other runner jobs seen, and a
     `contended` or `clear` verdict with its reasons. Processes are named by the kernel's
     executable name (`ucomm`, never argv) and user only. The log keeps its newest half past 16 MiB
     (the trim writes the kept half to a temporary file and renames it over the log under the
     append flock; a writer that locked the replaced file reopens, and a crash never empties the
     log). job-started's lines try the lock for about 200 ms and are skipped rather than delay a
     job. Completed lines' `roots` and `gui` are the last values read while the job's
     Runner.Worker was alive.
     `glaeda-fleet-status` reports it (source `jobs`). The sampler never refuses, delays
     or fails a job.
   - Job log schema. Every line has `schema` (`glaeda-cmux-job/v1`), `event`, `at` (unix
     seconds) and the job's identity: `host`, `runner`, `instance`, `job`, `class`,
     `workflow`, `repository`, `run_id`, `run_attempt`, `event_name`, `ref`, `head_ref` (the
     PR branch), `sha`. `event` is `started` (written at admission), `refused` (written by
     job-started's refusal path) or `completed` (the sampler's host record); a line without
     `event` comes from an older hook and is a `completed` one. Started and refused lines add
     `decision` (the admitted or refused text), `wait_s` (seconds in admission and capacity
     waits; null for a refusal before admission began), `units`, `roots` (root-k names) and
     `gui`. The completed line carries the same fields plus the host record; its `roots` is
     re-read from the runner's `.roots` file during the job, and `roots_admitted` names the
     admission set when take-root `--switch` changed it. With telemetry off
     (`--no-telemetry`, `GLAEDA_RUNNER_TELEMETRY=0`) no lines are written.
   - job-completed stops the sampler (it writes the job's line), releases the host lock
     (or the capacity share), runs the same disk pressure pass and always exits 0.
4. Writes and loads `~/Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist`
   (runs `glaeda-hooks/listen.sh`, restarts on crash, logs to
   `~/Library/Logs/glaeda-cmux-runner.log`). `listen.sh` runs `run.sh` under the listener
   gate (`glaeda-cmux-runner-hook listen`).
   - **Why a gate.** Without it, an idle runner keeps listening while the fleet has the
     host, so GitHub keeps handing it jobs that job-started refuses. Each refusal made
     cmux's rescue cancel and re-run the whole PR run. On 2026-09-25, 7 of 14 refusals
     in two hours were "a fleet build holds the host lock": the fleet-cas reader held
     cmux8s for 47 minutes, and hq worker builds hold a mini for about 20.
   - **When the fleet has the host.**
     - A process holds `host.lock` exclusively. The gate checks every 2 s with one
       non-blocking shared `flock`, which only an exclusive holder blocks.
     - A reservation is active.
     - With weighted capacity only: a process waits for the lock. The gate checks with
       `lsof` at most every 30 s, because each call costs about 0.3 s of CPU on a mini.
       This hook's own processes never count as waiters (another runner's job-started,
       a lock holder, a gate). A stop needs the same waiter pid in a second, fresh
       reading.
   - **When the mini cannot fit the runner's next job** (weighted capacity only).
     - A side runner stops when every capacity unit is taken.
     - A root runner stops unless the mini could admit a compile: at least 2 free
       units, and a free persistent-dd token and canonical root. GitHub hands a root
       label's job to any idle root runner, so before this a root runner beside one
       running compile and a light job took the next compile and refused it (90 of about
       400 refusals on 2026-09-25). The gui token is no reason to stop: a compile needs
       none, and holding for it kept second roots idle while roots were the bottleneck.
       An app-host shard that meets a taken gui token waits up to 240 s for it
       (`--gui-wait`, inside cmux's 360 s refusal window) instead of refusing.
       (cmuxterm-hq#661, Workstream 7.)
     - A gui runner (`--gui-runner`, below) stops while the gui token is taken, every
       canonical root is taken, or every unit is. Only it carries the gui pool label, so
       holding it keeps no compile off the mini, and GitHub hands the GUI job to another
       mini's gui runner instead.
   - **When the console session cannot run GUI tests** (gui runner only). GUI jobs run
     in the console user's session, and a mini whose auto-login session is screen-locked
     (display sleep, then the lock) or sits at the login window fails every one
     (cmuxterm-hq#757). The gate reads `ioreg -n Root -d1` (`IOConsoleUsers`:
     `kCGSSessionOnConsoleKey`, `CGSSessionScreenIsLocked`; Root's `IOConsoleLocked`) at
     most every 30 s and holds the gui runner while the session is locked or no user is
     logged in. The same check refuses at job-started any job on the gui runner or of a
     `gui` class (`refused: console: ...`, before any capacity is taken), so cmux's
     rescue re-runs it elsewhere, and makes `take-gui` give way (exit 3), so test-e2e's
     build leaves its tests to the `test` job. An unreadable state changes nothing;
     `GLAEDA_RUNNER_CONSOLE_GATE=0` in the runner LaunchAgent turns it off.
   - **Stopping.** After two idle polls in a row, and one fresh look right before the
     signal, the gate sends `SIGINT` to the runner's `Runner.Listener`.
     - The listener's graceful exit ends its session, and GitHub shows the runner as
       offline.
     - With no listener running (`run-helper.sh` between listeners), the gate sends
       `SIGTERM` to the whole runner tree instead.
     - A runner with a job (a `Runner.Worker` under its tree) is never stopped, and
       its polls don't count toward the two.
     - A listener that hasn't exited after 60 s is killed, unless a job slipped in.
   - **Restarting and logs.** The gate starts `run.sh` again after two free polls. While
     it waits it logs `glaeda-cmux-runner-gate: holding the listener off: <why>`, and
     `--apply`'s verify accepts that line with or without gh.
   - **Failure is safe.**
     - A failed poll (a `ps` timeout under load) is logged and retried. After 5 in a
       row, the gate hands the runner back: it waits for the runner instead of killing
       it, and still ends it on a SIGTERM.
     - If the gate can't start (a broken hook or interpreter), `listen.sh` runs
       `run.sh` itself, as before the gate.
     - If the gate dies while its runner still runs, `listen.sh` never starts a second
       one. It stops the listener once it has no job, then exits 1 so launchd restarts
       a fresh gate.
     - A runner exit the gate didn't ask for passes through, so launchd's KeepAlive
       behaves as before.
   - **Hook updates.** When `--apply` replaces the hook, the gate re-executes the new
     hook in place and keeps its `run.sh`, so a gate fix reaches every mini without a
     runner restart. It does so only after the new hook accepts the exact argv it will
     get (`--parse-only`). SIGTERM and SIGINT stay blocked across the exec, so a stop
     is never lost.
   - **Busy runners.** An `--apply` that would change a loaded plist while the runner
     has a job fails that runner's agent step, "the runner has a job; nothing changed,
     re-run when idle", so it never boots out a running job.
   - job-started still refuses a job handed over in the moment before a stop.
5. Confirms through the GitHub API that the runner is listed with every label and
   waits up to 90 s for it to report online.

The receipt is `~/.local/state/glaeda/cmux-runner/receipt.json`. A second `--apply`
reports every step as unchanged. A plan with any blocked step applies nothing, and
a blocked `--apply` exits 1. One install per user: an `--apply` with a different
`--runner-dir`, `--name`, `--repo` or `--org` than the receipt is blocked until the
first one is uninstalled.

## 2b. No gh on the mini: pipe the token over SSH

Mint the one-time token on the operator's machine and pipe it in. It travels only
through the pipe: never an argv, a file, or the output on either side.

```bash
ssh MINI '~/glaeda/scripts/glaeda-cmux-runner --token-stdin'    # plan the gh-free path
gh api -X POST repos/manaflow-ai/cmux/actions/runners/registration-token --jq .token \
  | ssh MINI '~/glaeda/scripts/glaeda-cmux-runner --apply --token-stdin'
```

`glaeda-cmux-runner`, `glaeda-cmux-runner-hook` and `glaeda_reservation.py` must sit
side by side on the mini, plus `glaeda_fleet_labels.py` for `--manifest` (section 2c). Without `gh`, the release metadata comes from the public API through curl,
a name that is already registered is refused by `config.sh` itself, and step 5 is
confirmed from the runner's own log (`Listening for Jobs`) and `.runner` instead of
the API. Check the labels from the operator's machine (section 3).

Uninstall works the same way with a removal token:

```bash
gh api -X POST repos/manaflow-ai/cmux/actions/runners/remove-token --jq .token \
  | ssh MINI '~/glaeda/scripts/glaeda-cmux-runner --uninstall --apply --token-stdin'
```

If that removal fails there is no API fallback on the mini; delete it by id from
the operator's machine with `gh api -X DELETE repos/manaflow-ai/cmux/actions/runners/ID`.

## 2c. Labels come from the fleet manifest

For a fleet member, pass the manifest instead of `--labels` (the two are exclusive):

```bash
glaeda-cmux-runner --apply --token-stdin --manifest ~/glaeda-runner/mini-fleet.json --member cmux10s-mac-mini
```

The label rule lives in `scripts/glaeda_fleet_labels.py`, shared with
`glaeda-mini-fleet`, and must sit next to `glaeda-cmux-runner` on the mini. The
manifest is cmuxterm-hq `build-fleet/mini-fleet.json`; see that repository's
`build-fleet/FLEET-MEMBERSHIP.md`. The member's entry decides everything, and the
runner is named `<member>-glaeda` unless `--name` says otherwise:

| Manifest field | Label |
| --- | --- |
| always | `glaeda-mini` |
| `class`: `xl`, `std` or `light` | `glaeda-class-<class>` |
| `availability`: `dedicated` or `opportunistic` | `glaeda-<availability>` |
| each `defaults.xcode.apps` entry (with host `overrides`) that is really installed | `xcode-<version>` |
| the same, only for `dedicated` members | `glaeda-<class>-xcode-<version>` |

"Really installed" means the path is a real directory, not a symlink, and
`xcodebuild -version` under it prints exactly that `Build version`. The combined
`glaeda-<class>-xcode-<version>` label (for example `glaeda-std-xcode-26.6`) is the
one a pool picker routes a whole run to. Opportunistic members never carry it, so
they never receive a required job.

The installer refuses a member whose roles lack `ci-runner`, and refuses class `dev`
(takes no jobs) and `borrowed` (jobs only inside a VM, not through this tool).

GitHub fixes labels at registration, and changing them through the API needs an
admin token that a mini does not hold. So when the manifest's labels differ from
what the receipt registered, `--apply` with a registration token (`--token-stdin`,
or a logged-in admin `gh`) re-registers the same runner in place. It stops the
LaunchAgent, clears the local registration files (including the runner's
`_migrated` copies), runs
`config.sh --replace` under the same name, and starts the agent again. `_work` and
its hot state stay. It refuses while a job is running. If `config.sh` fails midway,
the runner stays stopped with the old registration still listed. Re-running with
a fresh token re-registers the same name with `--replace`; when `gh` is on the mini,
it refuses first if that name now belongs to a different runner id. A re-run that
passes neither `--labels` nor `--manifest` keeps the labels and name it registered
and never relabels. If `--replace` itself was interrupted after GitHub assigned the
new id, the id check blocks; pass `--replace` to take the name back.

The plan names every label drift on the register step: `label drift (receipt)` or
`label drift (GitHub)`, "registered labels differ from what this version would
register: +added -removed". A re-run without `--manifest` still reports GitHub drift,
and says to pass `--manifest` and `--member` to re-register.

**Never relabel from a stale copy.** A copy's labels come from its own code: the Sep 24
copy in cmux15's `~/glaeda-runner/scripts` planned "unchanged" because it predated the
`glaeda-runner-<name>` label. On a mini with an OTA release installed (`glaeda-update`),
a copy whose runner files differ from that release and are older (by its
`.glaeda-source.json` stamp, its release tag, or its git commit date; unknown counts as
older) fails the `scriptCopy` preflight: the plan is not ready, `--apply` is refused, the
note names the release's own copy to run, and the release's plan labels are shown as
`label drift (release ...)`. `--allow-stale` overrides. `glaeda-cmux-runner-fleet`
writes the stamp when it stages; `glaeda-update` refreshes an older staged copy from the
installed release every hour, and keeps a copy an operator staged on the release's day
or later. From an operator Mac, cmuxterm-hq's `fleet runner relabel HOST` stages glaeda's
`origin/main` in a temporary directory and does the whole relabel.

Relabelling keeps the runner's name. Moving an existing `<hostname>-glaeda` runner
to a member whose name `<member>-glaeda` differs is a different install: the
command refuses it as a conflict until `--uninstall --apply` removes the old one,
or pass `--name` with the existing name to relabel it in place.

## 2d. Several runners per mini

A manifest class carries a runner count and the capacity units they share
(defaults: `std` 4 runners and 4 units, `light` 2 and 2, `xl` 8 and 8). Override
them with `defaults.runner.classes.<class>` `{"runners": N, "capacityUnits": U,
"compileSlots": C}`, or per host under `overrides.runner.classes.<class>`.
`compileSlots` (default 1, at most U/2) is how many compiles run at once, one
`persistent-dd` token each (`persistent-dd.token`, `persistent-dd-1.token`, ...).
Raise it only once cmux's compile admission keeps its canonical root and kept
state per runner; with shared paths two compiles would clobber each other. Re-apply
every instance on the mini together: runners that disagree on the count run as many
compiles as the highest one allows. Each runner is one
`--instance K`. From an operator machine (gh as a repo admin, SSH to every member),
one command brings the whole fleet to the manifest, every member and every instance
at once:

    scripts/glaeda-cmux-runner-fleet                 # plan: the gate on every member, read-only
    scripts/glaeda-cmux-runner-fleet --apply --org manaflow-ai --group glaeda-minis --hosts cmux12s-mac-mini
    scripts/glaeda-cmux-runner-fleet --apply --org manaflow-ai --group glaeda-minis   # every instance

The fleet is registered at org scope (section 2e2), so an apply names that scope. Without
`--org`, the apply asks for the repository scope, finds each runner's org-scope receipt, and refuses
every instance ("this Mac already has a runner install ... on manaflow-ai") before changing
anything.

It runs each member's job-started gate read-only first and skips members that are
not eligible yet, so a mini joins the pools on the first run after it is onboarded.
One registration token per instance is minted locally and piped over SSH. By hand,
for one member, the same thing is:

    for k in 0 1 2 3; do
      gh api -X POST repos/manaflow-ai/cmux/actions/runners/registration-token --jq .token |
        ssh MINI "~/glaeda-runner/scripts/glaeda-cmux-runner --apply --token-stdin \
          --manifest ~/glaeda-runner/mini-fleet.json --member MEMBER --instance $k"
    done

Instance 0 keeps the original paths. Instance K gets `~/actions-runner-glaeda-K`,
the LaunchAgent `com.teamleaderleo.glaeda.cmux-runner.K`, its log
`~/Library/Logs/glaeda-cmux-runner-K.log`, its receipt under
`~/.local/state/glaeda/cmux-runner/instance-K/` and the name `<member>-glaeda-K`,
with the same labels, so the pool grows by K runners. An instance past the class's
count is refused. Every instance bakes the same `--capacity-units`, so the mini
never runs more than its units, however many runners pick up jobs. Uninstall one
with `--uninstall --apply --instance K`.

## 2e. Trusted-only runners on a mini that holds a secret

A mini that holds a secret, such as the fleet-cas signing key on the writer mini, must never run PR
code as the user that can read it. Set the host's runner overrides in the manifest:

```json
"cmux7s-mac-mini": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"],
                    "overrides": {"runner": {"trustedRef": "refs/heads/main", "trustedRepo": "manaflow-ai/cmux"}}}
```

Its runners then:

- carry `glaeda-trusted` and the pool label `glaeda-trusted-<class>-xcode-<version>` instead of
  `glaeda-<class>-xcode-<version>`, so a PR run's picker never counts or routes to them;
- bake `--trusted-ref refs/heads/main --trusted-repo manaflow-ai/cmux` into the job-started hook. It
  refuses (`refused: untrusted: ...`) every job that is not a push or schedule of exactly that repository
  on that ref: `GITHUB_REPOSITORY` and the payload's repository, `GITHUB_REF`, a push payload's `ref`
  and a schedule's default branch must all match, and a payload with `pull_request`, `workflow_run` or
  `merge_group` is refused. The hook lives on the host, so this holds even when a PR's workflow names
  the trusted label, and the exact repository keeps another repository in the org off the runner.

workflow_dispatch is refused: a dispatched workflow on main can check out any ref from its inputs (cmux's
app-host-test-rerun takes `refs/pull/N/merge`). pull_request, merge_group and workflow_run are refused too.

What the gate cannot see, and the operator must keep true:

- The trust boundary is whoever can push to main: main's ruleset must block direct pushes, with no
  bypass list, so every commit there is a reviewed merge.
- Trusted jobs must pin every action and reusable workflow by commit SHA, and must restore only caches
  that trusted runs wrote. A cache namespace that PR runs also write (build seeds, sccache, SwiftPM
  manifest caches) brings PR-produced bytes next to the secret.
- Also limit the org runner group to the one repository (an admin setting).
## 2e2. Other repositories on the same minis (org scope)

Registered to `manaflow-ai/cmux`, the runners serve only cmux. To let another repository of the org use
the pools, register them at org scope into a runner group and allow that repository in the group:

1. An org owner creates the `glaeda-minis` runner group (Settings, Actions, Runner groups): selected
   repositories `manaflow-ai/cmux` plus each guest repository, public repositories allowed (cmux is
   public; fork PRs are refused by the hook, not the group). Or the owner grants the operator the
   organization permission to manage runners and runner groups, and the operator does it.
2. `glaeda-cmux-runner-fleet --apply --org manaflow-ai --group glaeda-minis --migrate-from-repo manaflow-ai/cmux`
   deregisters each member's repo runners and registers them in the group, one member at a time, so
   the other members keep taking cmux jobs. Members with a trusted-only runner (2e) are skipped and
   stay on their repository. Before touching any member it checks that the group exists and allows
   the repository, and it mints each registration token before deregistering, so a missing
   permission changes nothing. Each instance's receipt decides what happens: already in the org, it
   is only re-applied; registered elsewhere, it is left alone. Re-running after a partial migration is
   safe, and an instance deregistered but not re-registered reports `NO RUNNER`. Run it when the pool
   is quiet: deregistering stops a job that is running on that member, and the runner directory
   (with `_work`) is recreated, so each instance's first jobs afterwards are cold.
3. Adding a later repository is only a group edit:
   `gh api -X PUT orgs/manaflow-ai/actions/runner-groups/GROUP_ID/repositories/REPO_ID`.

At org scope the hook admits any repository of the org (`--allowed-owner`) under the same event, fork
and disk rules. Job ids in the hook's table are cmux's (`--home-repo`, default `manaflow-ai/cmux`); a
guest repository's job never takes a canonical root or the persistent-DerivedData token, whatever its
id. It costs 2 units (isolated), or 1 unit when its id ends in `-light`, or 2 units plus the one
simulator token when its id ends in `-sim` or `simulator`. A repository the hook cannot identify is a
guest. cmux jobs are refused at once when a mini is full (its rescue workflow reroutes them); a guest
has no rescue, so it waits up to `--guest-wait` (600 s) for room before it is refused. Guests share
the runner user's `$HOME` and `/Users/Shared/cmux-build-fleet`, so the trust boundary is anyone who
can push a branch to any repository in the group (outside collaborators and bots such as Dependabot
included), and these hosts hold no secrets.

## 2f. Canonical roots

Compiles build in a canonical root (/private/tmp/cmux-ci; cmux#14338 adds /private/tmp/cmux-ci-2 and on),
and app-host test consumers restore a product there with `rm -rf <root>/src`, because `#filePath` is baked
in at the producer's root. So one root job per root per mini:

- Root jobs are compile (macos-compile-admission and any unknown job id), compile-gui (test-e2e's `build`:
  a producer that takes the gui token later, in its own step with take-gui, and no persistent-dd: it only
  clones its root's kept state, which the root token already guards), gui (app-host-unit-tests,
  tests-build-and-lag, app-host-test-rerun's `rerun`, test-e2e's `test`) and product (cli-product-tests).
  Each also takes an exclusive `capacity/root-k.token` (k = 1 to `canonicalRoots`), and the hook writes `CMUX_CI_CANONICAL_ROOT=<root k>` to `$GITHUB_ENV` and
  `$RUNNER_TEMP/glaeda-canonical-root`. The root follows the token, never the runner instance.
- Light jobs take no root.
- Seed jobs (seed-derived-data.yml's `seed`, trusted runners only) take 2 units and no token: the seed
  key names the root, so the job holds that root itself with `glaeda-canonical-root take <root>` before
  it clears it. On a trusted mini with 2 runners, 4 units and `canonicalRoots` 2, two seeds (one per
  root) run at once; with one runner nothing changes.
- The nightly app build (nightly.yml's `build-nightly-app`, trusted runners only) is isolated: 2 units,
  no token. It compiles into its own workspace, so it never holds a root a seed on the same mini waits for.
- `defaults.runner.classes.<class>.canonicalRoots` (default 1, at most `runners`) runners per mini
  (instances 0 and up) also carry the root pool label `glaeda-root-<class>-xcode-<version>`. Root jobs
  should run on that label, so GitHub queues them until a root runner is free instead of handing one to a
  runner whose mini is already busy in its root (a "canonical root token is taken" refusal).
- Each root runner also carries `glaeda-runner-<runner name>`, a static label naming only itself. cmux's
  picker reads which root runner kept a warm build of a run's merge base and puts that label in compile
  admission's runs-on, so the routing App only reads runners and nothing writes labels at job time.
- A root runner tries its own root first (instance i, root i+1), then any free one. A
  macos-compile-admission of a pull request tries before that the root whose kept build is warm for it:
  cmux's `owned_build_state.py keep` stamps `<CMUX_OWNED_STATE_ROOT>/[cmux-ci-k/]stamp.json` with
  `warm: [<merge base sha12>, "pr-<number>"]`, and the hook matches the event's `pull_request.base.sha`
  first, then its number. The picker routes by the mini's keys, so this sends the job to the right tree
  on a two-root mini; the admission line ends with `warm for <key>` when it did.
- Before the exact keys, the hook ranks the free roots by predicted compile (cmux#14778,
  `warm_root_costs`): main's app Swift files between each kept build's merge base and the event's base
  (the seed prefetch's blobless mirror `ci/.prefetch/cmux.git`, trees only, 5 s per diff), plus the kept
  pull request's own files from its stamp (`pr_app_swift_files`) unless it is the same pull request, put
  into the tiers of cmux's fitted model (`ci/warm-distance-model.json`, which admission copies there;
  near 140 s, far 267 s, rebuild 401 s by default). The cheapest root goes first, the runner's own root on
  a tie; the admission line ends with `predicted <s> s <tier>`, and the job gets the per-root predictions
  in `GLAEDA_WARM_ROUTE` for cmux's admission record. Any missing commit, stamp field or model, or an
  error, falls back to the exact keys.
- The other runners (instances `canonicalRoots` and up) carry the side pool label
  `glaeda-side-<class>-xcode-<version>` instead. cmux's light side-lane jobs run on it
  (`vars.CI_SIDE_LANE_RUNNER`, cmux#14391), so they never hold a root runner. A class whose
  `canonicalRoots` equals `runners` has no side runner, so do not point that variable at it.
- `guiRunners` (0 or 1, default 0) makes the last instance (`runners - 1`) the mini's gui runner. It carries
  the gui pool label `glaeda-gui-<class>-xcode-<version>` and neither the pool, root, side nor
  `glaeda-ios-sim` label, and its job-started hook passes `--gui-runner`. cmux's GUI jobs (app-host shards,
  tests-build-and-lag) run on it, so GitHub hands each mini at most the one GUI job its gui token allows,
  and a second one waits in GitHub's queue for any mini's gui runner. Before, two root runners shared one
  gui token and GitHub gave the second GUI job to the other root runner, which waited up to 240 s and
  refused (10 of 17 refusals in the hour to 2026-09-26 03:40Z). It is never a root runner, so
  `canonicalRoots + guiRunners` is at most `runners`. Admission is unchanged: a GUI job takes 1 unit, the
  gui token, and the producer's root in its restore step.
- `compileSlots` may not exceed `canonicalRoots`: every compile holds a root.
- `declared_pools` and glaeda-route count only the pool labels; the root and side labels split each mini's
  runners between them.
- With `canonicalRoots` above 1, consumers (gui and product jobs) take no root at job start. Their restore
  step runs `/Users/Shared/cmux-build-fleet/bin/glaeda-canonical-root take <root> --wait 1800` for the
  producer's root (the path from the product receipt, or N). The job-started hook links that path to its
  runner's generated shim, which runs the hook's `take-root`: an exclusive `root-N.token` held by a second
  detached holder tied to the job's Runner.Worker and released by job-completed. A re-take of a root the
  same job already holds (a compile restoring its own product) is a no-op. It exits 1 when the root is
  still busy after the wait, and 2 for a bad root or outside a runner job.
- A second, different root is refused (exit 2): two jobs taking two roots in opposite orders would deadlock.
  `take ROOT --switch` swaps instead.
  - It waits for ROOT while still holding the old root, and lets the old one go only once ROOT is held.
    A timeout (exit 1) leaves the job on its old root, never without one.
  - Two switchers after each other's root both time out, so callers keep `--wait` short.
  - On success it writes the new `CMUX_CI_CANONICAL_ROOT` to `$GITHUB_ENV`.
  - It only works when each held root has a live holder of its own: one taken with `take`, or the
    admission root of a `ROOT_SWITCHERS` class (compile-gui, test-e2e's `build`), which the hook hands to
    a separate holder at admission.
  - A build that finds a product to reuse at another root switches to it. The caller must be done with
    the old root.
- `glaeda-canonical-root take-gui [--wait S]` holds the mini's gui token from that step to the end of the
  job, with a third holder (`<holder>-gui.pid`) that job-completed releases. It is a no-op when the job
  already holds gui from admission. It exits 0 when held, 1 when still taken after the wait, and 2
  outside a job.
  - A job in take-gui holds a root, while a gui job waiting in take-root holds gui: opposite lock orders.
    So every take-root waiter writes `capacity/root-k.want-<pid>`, containing `gui` when its job holds
    the gui token. take-gui exits 3 at once when a gui holder is waiting for a root this job holds. The
    caller then leaves its console-session work to another job and finishes, which frees the root.
    Waiters rewrite their markers every second. take-gui removes a marker older than 10 s or with a dead
    pid, so a killed waiter's leftover cannot make it give way.
- Jobs the hook does not know (seed-swiftpm-manifests, anything new) are pinned to root 1, because they use
  /private/tmp/cmux-ci themselves. Ids that other workflows reuse (`build`, `test`, `lint`) are classed by
  (workflow file, job id) from `GITHUB_WORKFLOW_REF`, so with `canonicalRoots` above 1 test-e2e's jobs are
  not pinned: `build` takes root 1 when free, else any free root (root 1's seeds and caches are the ones main
  publishes; it keeps no per-root state), and reads it from `CMUX_CI_CANONICAL_ROOT`. `test` is a consumer and
  takes the producer's root in its restore step.

## 2g. iOS simulator runners

A host with the `ios-simulators` role carries `glaeda-ios-sim` when the installer also finds every runtime
build the manifest's `ios_simulator.runtimes` declares available (`xcrun simctl list runtimes -j`, matched on
`buildversion`). The build is pinned, not any 26.x: Xcode 26.6 refuses simulators that are not its iOS 26.5
SDK's runtime (23F77), so a 26.3.1-only mini fails every iOS job. No declared runtime means no label. If simctl gives no usable answer, the runner keeps
the label it registered with, so a CoreSimulator hiccup never re-registers it. iOS jobs use
`runs-on: [glaeda-std-xcode-26.6, glaeda-ios-sim]` through the owned-pool picker, so a job never lands on a
mini without a runtime (cmux14 has none). The picker must count ios-sim capacity before workflows depend
on the label, or a job could wait for a label no online runner has.

Hook classes:

- `mobile-core-package` and `ios-simulator-build` are `isolated` (2 units, own DerivedData or SwiftPM
  `.build`, no canonical root).
- `ios-simulator` and `screenshots` are `simulator` (2 units plus the per-mini `simulator` token: they
  reuse, erase and boot named devices in the user's one CoreSimulator service).
- `validate` (ios-streamed-validate) stays on Blacksmith. It binds fixed ports, restarts a local Postgres
  under /tmp, changes the GUI session (open, launchctl setenv, system dark mode) and writes credentials
  to `$HOME`.

## 2h. Test keychain

The cmux user's login keychain is locked in the runner's launchd session, so tests that add keychain items
fail with errSecInteractionNotAllowed. On PR runners (not trusted ones) the job-started hook creates
`~/Library/Keychains/cmux-ci.keychain-db` with an empty password and no lock timeout, unlocks it, and makes
it first in the user search list and the user's default keychain. It holds test junk only, and every PR job
can read it.

**Never store credentials as the runner user on a PR mini** (`gh auth login`, `git credential-osxkeychain`,
`security import`, Keychain Access). Without an explicit keychain they land in `cmux-ci`, and any later PR job
can copy that file and read them. Credentials belong on trusted or signing hosts.

## 2i. Seeds over the LAN from the trusted seeder

`glaeda-seed-prefetch` (the 5-minute seed-prefetch LaunchAgent) asks the trusted seeder for main's
nearest seed over the LAN before it runs cmux's R2 prefetch. The seeder (cmux15, trusted-only, main pushes
only) keeps every seed it builds or adopts (`CI_SEED_KEEP_LOCAL_RUNNERS`, `seed_derived_data.py keep`) as
extracted DerivedData under its per-root seed caches. Measured 2026-09-25, cmux15 to a PR mini on the LAN:
one 9.0 GB seed as `tar | zstd -1 -T0` is 2.3 GB and took 25.5 s; plain tar took 77.6 s. From R2 a mini
takes 190 to 280 s.

- **Only cmux15 serves.** cmuxs-mac-mini-6 builds PR code as the same user, so its seeds may be tainted;
  `glaeda-seed-lan` refuses it as a seeder, and `glaeda-seed-serve` refuses every request unless the host's
  glaeda runner receipt names a trusted ref. If cmux15 joins the PR pool, run `glaeda-seed-lan remove`
  first.
- **One-way.** Each mini's own key is authorized on the seeder only as
  `restrict,from="172.20.20.0/22",command="/usr/bin/python3 -I ~/.local/libexec/glaeda-seed-serve"`: no shell,
  pty or forwarding. The forced command reads the request (`seed-v1 CODEC KEY...`), accepts only keys that
  match the seed key pattern (no path, no option), looks each up as a real directory holding the seed
  manifest, and streams the first it keeps. Nothing a mini sends is written anywhere except the serve log.
  The key on a PR mini is readable by PR jobs; all it grants is reading seeds that are public in R2
  anyway, from the LAN, two at a time.
- **Seeder load, against a hostile client.** One request at a time per client address, at most four
  requests in flight (further ones get `busy` at once), at most two streaming (60 s wait for a slot),
  each cut off after 180 s however slowly the client reads; nice 10 and utility disk I/O. The forced
  command runs `python3 -I` (no user site-packages or PYTHON* variables).
- **Mini load.** While a job runs, the LAN extraction is paced to 64 MiB/s (about 140 s for a seed).
- **Remaining exposure.** A PR job on a mini can rewrite `~/.config/glaeda/seed-lan/config.json` and
  `known_hosts` (same user), pointing that mini's LAN step at another host. That host could only feed
  that mini a seed, which a PR job there can already write directly; the seeder and other minis are
  unaffected. A follow-up could have `glaeda-mini-fleet check` hash both files.
- **Integrity.** The mini extracts into `seeds/.lan-<pid>`, requires exactly one top-level directory
  named by the requested key with `cmux-seed-input-mtimes.json`, caps the stream at 16 GiB, and renames it
  into place. Any failure removes the staging directory. R2's prefetch then runs unchanged: it finds the
  seed already kept (and prunes), or downloads a nearer one. A miss or any error is today's behaviour.
- **Local Network Privacy.** A LaunchAgent whose program is not Apple's cannot reach LAN addresses, and
  neither can its children: a Homebrew python3 agent and its `/usr/bin/ssh` got "No route to host", while
  a `/usr/bin/python3` agent connected (2026-09-25, `launchctl submit` probe on cmux12s). So
  glaeda-mini-setup runs the seed-prefetch agent with `/usr/bin/python3`, and the client uses
  `/usr/bin/ssh` and `/usr/bin/tar` (plus Homebrew zstd to decompress a pipe). The tailnet address of
  cmux15 was not reachable from the PR minis, so the config lists LAN addresses.

Set up, from a checkout at `origin/main` on the operator Mac (plan first, then `--apply`):

```bash
scripts/glaeda-seed-lan install --seeder cmux15 --address 172.20.21.202 --address cmux15.local HOST...
scripts/glaeda-seed-lan install --seeder cmux15 --address 172.20.21.202 --address cmux15.local HOST... --apply
```

It installs the serve script on the seeder, makes each mini's key in `~/.config/glaeda/seed-lan/`, pins the
seeder's host key (read over the operator's own SSH session), authorizes the keys, and pings from each
mini. Add the printed `manifest_keys` to `~/.config/glaeda/mini-fleet.json` (cmux15's authorized_keys
policy is exclusive). Verify under launchd, not an SSH shell, on one mini:

```bash
launchctl kickstart -k gui/$(id -u)/com.teamleaderleo.glaeda.seed-prefetch
tail -n 1 ~/Library/Logs/glaeda-seed-prefetch.jsonl   # results.<root>.lan: fetched true, or its reason
```

A `lan.reason` with "No route to host" means the agent is not running `/usr/bin/python3` yet (glaeda-update
rewrites the plist within the hour). On the seeder, `~/.local/state/glaeda/seed-serve/serve.jsonl` records
each request. Roll back with `scripts/glaeda-seed-lan remove --seeder cmux15 [HOST...] --apply`: without a
config a mini skips the LAN step.

## 2j. Compiled products over the LAN between PR minis

A consumer of the app-host test product (the shards, E2E) downloads ~650-830 MB from GitHub in about
130 s even when another mini already holds the same object in its node-local product cache
(`/Users/Shared/cmux-build-fleet/node-products`). On 2026-09-25 a 648 MB object moved cmux14 to cmux15
over the LAN, including sha256 on arrival, in 5.9 s (~110 MB/s).

- **Serve.** Every PR mini runs `glaeda-seed-serve --role product` as the forced command of a mesh key
  that each other PR mini holds. It answers `product-has-v1 SHA256` and `product-v1 SHA256`, where SHA256
  is the archive digest (metadata `object_digest`). It serves only real `objects/<xx>/<key>/` entries
  with cmux's current metadata schema, a size up to 20 GiB, and a regular `object.tar.gz` of exactly that
  size. It uses the same per-client, queue, slot and 180 s limits as seeds. It never answers seed verbs,
  and the seeder key never answers product verbs.
- **Trust.** Any PR mini may serve, and a PR job on the serving mini can write its cache, so the server
  is not trusted at all. `glaeda-lan-fetch` hashes the bytes as they arrive, compares them with the digest
  GitHub recorded for the artifact (passed by cmux CI), and only then links the file to DEST. cmux then
  checks the digest again and runs its canonical restore validation.
- **Local Network Privacy.** A CI step cannot reach the LAN itself. Every process with a non-Apple
  ancestor in its launchd job is refused (probes 2026-09-25: `/bin/bash` -> Homebrew python3 ->
  `/usr/bin/nc` got "No route to host"; `/bin/bash` -> `/usr/bin/python3` connected), and a runner job
  descends from the listener gate's `~/.local/bin/python3` and `Runner.Listener`. So `glaeda-lan-fetch
  product` hands the request to the `lan-fetch` LaunchAgent (program `/usr/bin/python3 -I`) over a Unix
  socket in `~/.local/state/glaeda/lan-fetch/` (0700). The broker makes the ssh connections.
- **Helper.** cmux calls `/Library/Application Support/glaeda/bin/glaeda-lan-fetch` only when the file and
  every directory above it are root-owned and not group- or other-writable, so a PR job can neither rewrite
  nor rename it. That rules out `/Users/Shared/cmux-build-fleet/bin`, which belongs to the fleet user.
  glaeda-mini-setup installs `~/.local/bin/glaeda-lan-fetch` and prints the sudo step for the root-owned
  copy until it is current. cmux runs the helper with a minimal environment (no tokens).

Set up, from the operator Mac (plan, then `--apply`):

```bash
scripts/glaeda-seed-lan mesh cmux7s-mac-mini cmux8s-mac-mini ... cmuxs-mac-mini-5
scripts/glaeda-seed-lan mesh cmux7s-mac-mini cmux8s-mac-mini ... cmuxs-mac-mini-5 --apply
```

LAN addresses come from `ipconfig getifaddr en0` on each host (override with `--address HOST=IP`). Add
the printed `manifest_keys` to each host's authorized_keys policy in `~/.config/glaeda/mini-fleet.json`.
Then install the root-owned helper on every mesh host by hand, as root, from an admin account other than
cmux or at the console. Do it again whenever glaeda-mini-setup prints the step (the helper changed).
Never pipe a sudo password through the cmux account: its login shell runs files a PR job can write
(`~/.zshenv`), so they could read it. `helper-install` never runs sudo. It checks each host's copy and
directory chain, and with `--apply` it writes the root script and prints the commands:

```bash
scripts/glaeda-seed-lan helper-install cmux7s-mac-mini ... cmuxs-mac-mini-5                     # state per host
scripts/glaeda-seed-lan helper-install cmux7s-mac-mini ... cmuxs-mac-mini-5 --admin ADMIN --apply  # script + commands
# for each host, as printed:
scp ~/.local/state/glaeda/seed-lan/glaeda-lan-fetch-install-<sha>.sh ADMIN@HOST:
ssh -t ADMIN@HOST 'shasum -a 256 glaeda-lan-fetch-install-<sha>.sh && sudo /bin/bash glaeda-lan-fetch-install-<sha>.sh; rm -f glaeda-lan-fetch-install-<sha>.sh'
```

The script carries this checkout's helper and its sha256, never the mini's user-writable copy. It fails
closed unless every existing component of `/Library/Application Support/glaeda/bin/glaeda-lan-fetch`,
from `/` down, is a non-symlink owned by root without group or other write. It creates only missing
directories (root:wheel 0755) and installs through a temporary file whose sha256 must match, then a
rename. It holds a lock directory with its pid: a live holder under 10 minutes old means BUSY, and a
dead or older one is taken over. Rerun `helper-install` without `--apply` afterwards: every host
should be `current`.

Check it on a mini with any digest a peer holds:
`"/Library/Application Support/glaeda/bin/glaeda-lan-fetch" product SHA256 /tmp/x.tar.gz` (the record says
`"via": "broker"`), then remove `/tmp/x.tar.gz`. Remove the mesh with
`scripts/glaeda-seed-lan mesh-remove HOST... --apply`.

## 2k. Idle catch-up: build main into a far root while the mini is idle

A root's kept build is the last pull request's, merged onto a main that has moved on since. After a quiet
spell every root is far from main's head, and the next admission recompiles main's drift as well as its own
diff (warm-distance tiers: near compiles in about 140 s, rebuild about 400 s, cmux
`scripts/ci/warm-distance-model.json`). On 2026-09-26 at 04:00Z, 19 of 22 roots on 12 minis were
rebuild-tier from main's head, and each mini had sat fully idle 17 to 33% of the previous six hours
(`~/Library/Logs/glaeda-cmux-jobs.jsonl`).

`glaeda-idle-warm` (the 5-minute idle-warm LaunchAgent from glaeda-mini-setup) closes that gap:

- **When.** No Runner.Worker or xcodebuild, no job started or ended for 10 minutes, 1-minute load under
  0.25 per core, thermal pressure nominal, at least 136 GiB free (the 100 GiB admission floor plus a cold
  compile), no reservation, no fleet build holding or waiting for the host lock. Never on cmux-mac-mini (hostname cmuxs-Mac-mini-5, the production
  iOS soak box) or Lawrence's machines, never next to a trusted-only runner (a seeder), and only on
  capacity-mode runners. `touch ~/.config/glaeda/idle-warm.disabled` stops it on one mini.
- **Which root.** The hook's own prediction (`warm_root_costs`) of each root's compile for main's head, from
  the seed prefetch's mirror and the job-written model: a rebuild root first, then far, then one it could not
  compare, then one with no kept build (a cold build), skipping any already built or tried for that head. Three failed builds in a row pause it for six hours.
- **How.** It loads the hook the runners run (from their `glaeda-hooks/`) and refuses if that hook predates
  the yield below, so a rollout in either order is safe. Under `capacity/admission.lock` it takes the root's
  token, a persistent-dd token and a compile's units, with the host lock shared, writes
  `capacity/idle-warm.json` (its pid and those names), and lets the admission lock go. Then cmux's
  `scripts/ci/owned_catch_up.sh`, run from a clean checkout of main's head kept under `ci/.catch-up/cmux`,
  runs the steps of a main dispatch's compile admission (check, prefer against kept seeds, adopt, record,
  compile, keep with main's head as `merged_onto`, save). Logs: `ci/.catch-up/logs/`, one line per run in
  `~/Library/Logs/glaeda-idle-warm.jsonl`.
- **Jobs and fleet builds come first.** Every admission (`take_capacity`, under admission.lock) sends the
  catch-up SIGTERM and waits up to 5 s for it to exit, then SIGKILLs it. The catch-up leads its own process
  group and its build runs in it, so one `killpg` ends both and the kernel drops the locks with the last fd;
  the hook kills whatever is left of that group the moment the catch-up is gone, and launchd kills a
  LaunchAgent's group whenever it dies. The listener gate counts what the catch-up holds as free, never
  counts its shared host.lock as a fleet build waiting, stops it when a fleet build holds or waits for the
  host lock, and stops it instead of holding a runner off for load or heat. So a mini never stops listening
  or refuses a job because of it. A build killed with no job behind it counts as a failed try of that head,
  so shedding its load cannot loop. A kill at any step leaves the kept state as it was, or unstamped (the
  next job takes a seed); never a partial build marked warm. The holder file is trusted only while its pid is
  alive and its program is glaeda-idle-warm.
- **Trust.** The build is main's own code, run as the build user on a PR mini. Pull requests admitted to that
  root already share its kept state, so this adds no new boundary. No GitHub token reaches it.

Check one mini: `glaeda-idle-warm` (plan) says whether it would warm now and which root, or why not.

## 3. Verify

```bash
scripts/glaeda-mini-setup --runner                       # every step "unchanged", verify ok
launchctl print gui/$(id -u)/com.teamleaderleo.glaeda.cmux-runner | head -20
gh api repos/manaflow-ai/cmux/actions/runners --jq '.runners[] | select(.name | endswith("-glaeda")) | {name, status, busy, labels: [.labels[].name]}'
tail -f ~/Library/Logs/glaeda-cmux-runner.log
```

Expect `status: "online"` and labels `self-hosted, macOS, ARM64, glaeda-mini`.

## 4. Route

**Fleet members (`--manifest`) route through cmux's PR pool picker**, which already sends jobs
to their pool labels (`CI_PR_POOL_OWNED`, `CI_OWNED_POOL_SLOTS`); nothing is set per runner,
and their plan prints no `MACOS_RUNNER_*` line. Setting those variables to `glaeda-mini`
would bypass the picker and send every PR job to the generic label. A trusted member
(section 2e) prints the nightly pair instead, with its rollback:

```bash
gh variable set CI_SEED_TRUSTED_POOL --body glaeda-trusted-std-xcode-26.6 --repo manaflow-ai/cmux
gh variable set CI_NIGHTLY_TRUSTED_RUNNER --body glaeda-runner-cmux15-glaeda --repo manaflow-ai/cmux
gh variable delete CI_NIGHTLY_TRUSTED_RUNNER --repo manaflow-ai/cmux   # rollback: nightly on Blacksmith
```

The rest of this section is for a single `--labels` runner outside the fleet. The plan
prints these; it never runs them. Start with one variable, watch a few jobs, then add
the rest:

```bash
gh variable set MACOS_RUNNER_15 --body glaeda-mini --repo manaflow-ai/cmux
gh variable set MACOS_RUNNER_26 --body glaeda-mini --repo manaflow-ai/cmux
gh variable set MACOS_RUNNER_PR --body glaeda-mini --repo manaflow-ai/cmux
gh variable set MACOS_RUNNER_DUAL_XCODE --body glaeda-mini --repo manaflow-ai/cmux
```

cmux workflows read these as `runs-on: ${{ vars.MACOS_RUNNER_15 || 'blacksmith-6vcpu-macos-15' }}`.
Until cmux's `runs-on` expressions fall back to Blacksmith for fork pull
requests, routing `MACOS_RUNNER_PR` sends fork PR jobs here, where the job-started
hook refuses them: they fail instead of running on Blacksmith. Route
`MACOS_RUNNER_PR` only after that fallback lands. One mini runs one job at a time, so routing every variable to
it queues work behind it.

## 5. Rollback

Stop routing (jobs fall back to Blacksmith on their next run):

```bash
gh variable delete MACOS_RUNNER_15 --repo manaflow-ai/cmux
gh variable delete MACOS_RUNNER_26 --repo manaflow-ai/cmux
gh variable delete MACOS_RUNNER_PR --repo manaflow-ai/cmux
gh variable delete MACOS_RUNNER_DUAL_XCODE --repo manaflow-ai/cmux
```

Remove the runner:

```bash
scripts/glaeda-mini-setup --runner --uninstall            # plan
scripts/glaeda-mini-setup --runner --uninstall --apply    # deregister and remove
```

Uninstall acts on the directory, name and scope in the receipt; flags cannot redirect it. It unloads and removes the LaunchAgent
only if it still has the bytes this tool wrote, deregisters with a one-time removal
token (falling back to `DELETE .../actions/runners/<id>` when `gh` is present, and only for
the id this install registered), and removes
`~/actions-runner-glaeda` (including `_work`) only if the receipt created it and
the directory's marker still matches. If deregistration fails, the directory and
receipt stay so the command can be re-run. Nothing outside those paths is touched.

To stop a runner without removing it, use `cmux_mini_fix.sh runner_hold` (what
`glaeda-mini-fleet` drains with): it writes a mark under
`~/.local/state/glaeda/mini-fleet/runner-held/`, and `--apply` leaves a marked runner
stopped until `runner_release` starts it (uninstall drops the mark). A bare `launchctl disable` leaves no mark,
so the next `--apply` enables the label before its bootstrap and records `wasDisabled`
in the agent step of the receipt. To take a host out of the pools for good, drop its
`ci-runner` role and uninstall its runners.
