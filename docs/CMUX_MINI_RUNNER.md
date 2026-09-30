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
     so a PR job never lands on a mini that is busy with other work. It is refused, so
     cmux's rescue re-runs it on Blacksmith, when the host is reserved
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
     token (one writer of the kept DerivedData at a time), `tests-build-and-lag` 1 unit
     plus the `gui` token (one console session), `app-host-unit-tests` 1 unit (on a mini
     with more than one root it takes the `gui` token itself with take-gui in the step
     before its restore, so its product fetch leaves the console session to other GUI
     jobs; a one-root mini gives it the token at job start),
     test-e2e's `build` (compile, then the selected tests in the console session) 2 units
     (it takes the `gui` token itself before its tests, with take-gui), test-e2e's `test` 1
     unit plus the `gui` token, `cli-product-tests` 1 unit plus the `gui` token (its XCTest
     run shares the user's testmanagerd with the GUI jobs, see 2h2),
     `swift-package-tests` and the side lanes `cli-pipe-regressions`,
     `remote-daemon-macos-tests` and `claude-wrapper` 1 unit, and any other job counts
     as a compile. When units or a token are taken a cmux job waits for them, with no
     time limit, and never fails for capacity: GitHub has already handed the job to this
     runner and cannot move it. Every 2 s it tries again; it logs what it waits for and who
     holds it (`waiting (N s so far): all 2 canonical root tokens are taken (...); held by
     root-1: RUNNER JOB (run N); ...`) when that changes and every minute. It leaves a
     marker, `capacity/admit.want-<pid>` (the token kinds, or `units`, it lacks, since
     when, and who it is), so a later admission that needs one of those kinds waits
     behind it instead of taking a token the moment it frees, and the listener gate holds
     the runners whose next job would compete for them (see below). A job that waits too
     long in its runner's setup is cmux's owned-pool rescue's to move to Blacksmith, as a
     job queued too long is. Before 2026-09-28 it refused after 240 s: cmux PR #15160's
     compile admission was refused on cmux10s while two E2E builds on the mini's non-root
     runners held both canonical roots, and a newer compile took the root that freed
     during its wait. `--gui-wait N` caps the wait (then it refuses), for tests. Because `flock` gives no preference to the exclusive waiter, it also
     takes nothing while another process (the build worker in `with-host-lock`) is waiting
     for `host.lock`: it retries through the same wait, so the worker gets the host as soon
     as the running PR jobs end.
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
     (or the capacity share), kills orphaned `cmux DEV.app` processes whose executable was
     under a finished runner `_work/_temp/cmux-derived-data-tests-*` directory, runs the same
     disk pressure pass and always exits 0. The two-minute `glaeda-mini-health` agent repeats
     that ownership check when a post-job hook was skipped.
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
     - Except a yielding build: a fleet build that writes its pid to `host.lock.yield`
       while it waits for or holds the lock (cmuxterm-hq's catch-up fill on the writer
       mini) neither stops the listener nor counts as a waiter, as long as `lsof` shows it
       and its own processes (descendants and their process groups: the fill passes the
       lock fd to its build) are the only fleet processes with the lock open. A fill
       waiting behind another fleet build yields nothing, so that build still stops the
       listener. The gate keeps that verdict for 30 s, since `lsof` is not free. A job that meets a
       yielding holder writes `host.lock.preempted` (`{by, at, pid}`, so the fill can tell
       the note is about itself), sends it SIGTERM and waits for the lock: 15 s under the
       capacity admission lock (half of what another admission waits for it), 90 s on the
       single-runner path. Then it runs. A pid without the lock open is never signalled.
       A fresh DerivedData seed saves more PR compile time than one more main fill, which
       a quiet tick redoes from the compile cache.
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
       An app-host shard that meets a taken gui token waits for it instead of refusing.
       (cmuxterm-hq#661, Workstream 7.) A side runner listens with one unit free, so
       a 2-unit side lane (cmux's release-build, reload-build, cmux-tui) that finds
       fewer units than it needs waits for them instead of refusing, as does a root
       runner that lost a race for its units.
     - Every runner stops while a job already admitted to this mini waits for units
       (`admit.want-<pid>`), a root runner also while one waits for a canonical root or
       persistent-dd, and a gui runner while one waits for the gui token or a root. That
       job goes first, so a job taken now would only queue behind it.
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
   - **When free disk is under the floor** (every runner). The gate reads the
     `--min-free-gib` of the runner's own job-started hook and `statvfs` on every poll,
     and holds the listener while free disk is below it, since job-started would refuse
     every job. On 2026-09-27, 68 of the fleet's 151 refusals in 24 h were this one,
     mostly seeds on cmuxs-mac-mini-6 at 134 of 150 GiB. A hold ends only 2 GiB above the
     floor, so a mini at the edge does not flap. `glaeda-disk --pressure` reads the highest
     floor among the user's runner hooks (`~/actions-runner*`, the default runner dirs) and,
     on HOME's volume, starts freeing at floor + 4 GiB, up to floor + 20 GiB (never past half
     the disk), whatever its `--low` and `--target`: otherwise a 460 GiB mini between the default
     low (about 115 GiB) and a 150 GiB floor would stay held with nothing freed. A hold past
     30 min logs once that space must be freed by hand. It no longer waits for CI to finish
     (`defer_during_ci`, default false in `~/.config/glaeda/disk-policy.json`): a busy mini frees
     space while jobs run, holding only what a job uses. cmux CI's hot tier (parked PR builds,
     seeds, kept builds) ages in host CI jobs since last use rather than hours: under pressure an
     item used in the last 30 minutes stays; older parked PR builds and other reproducible hot
     state are then ranked by reuse distance and idle time until the target is reached. Pressure
     keeps one current seed per root; an idle sweep keeps two and uses the activity thresholds.
     From 11,393 job starts
     on 13 minis (2026-09-26..29) a PR's next job on the same host came p50 6, p90 76, p95 189 jobs
     later (0.4, 5.2, 15.5 h), so an item idle only because the fleet is idle stays. cmux CI's
     SwiftPM scratch (`ci/spm-scratch/<fingerprint>`, `owned_spm_scratch.py`) is its own family:
     a directory a job holds by its `<fingerprint>.lock` flock stays, the rest ages like any cache
     and goes whatever its age below the emergency floor, deleted only under that lock (cmux9s,
     2026-09-30: 31 GiB of it unheld while glaeda-disk found 0.0 GiB to free and every job was
     refused at 21.9 GiB free).
   - **Stopping.** After two idle polls in a row, and one fresh look right before the
     signal, the gate sends `SIGINT` to the runner's `Runner.Listener`. Under the disk
     floor the first idle poll is enough, ahead of any other hold, and while a job runs
     under the floor the gate watches its `Runner.Worker` every 0.1 s and polls every 0.1 s
     for 5 s after it exits: the listener asks for its next job as soon as the Worker
     exits and got one 2 s later, inside one 2 s poll, so every job it took was refused
     (cmux9s, 2026-09-30: four in 34 s on one runner).
     - The listener's graceful exit ends its session, and GitHub shows the runner as
       offline.
     - With no listener running (`run-helper.sh` between listeners), the gate sends
       `SIGTERM` to the whole runner tree instead.
     - A runner with a job (a `Runner.Worker` under its tree) is never stopped, and
       its polls don't count toward the two.
     - Neither is a listener taking a job: from its `Acknowledging runner request`
       diag line until that request's Worker finishes or its acquisition fails. A
       SIGINT in that span loses the job, and GitHub fails it 10 min later with "lost
       communication". The gate freezes the listener (`SIGSTOP`) for the last look at
       its diag and Worker, so it cannot acknowledge a request between the look and
       the signal, then signals and thaws it. The runner handles SIGINT on another
       thread after the thaw, so a message it had already received in that instant
       can still be acknowledged: milliseconds, where the old gap was seconds. The
       acknowledge line exists only while GitHub sets the runner's acknowledge flag.
       An agent stop still cancels a running job but waits out an acquisition.
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
or later unless the release descends from the stamp's commit.

**Hook fixes roll out without `--apply`.** Every hour `glaeda-update` also runs the
installed release's `glaeda-cmux-runner --refresh-hooks --apply`. In each runner directory
a receipt owns, it writes `glaeda-hooks/glaeda-cmux-runner-hook` and
`glaeda_reservation.py` atomically and synced (0755 and 0644, the module first) and nothing
else, so no registration token or org-admin access is needed. It replaces only a file whose
bytes are a version the release descends from (the release's `hook-history.json`); a hook
installed from a newer main, or edited, is `kept`. A runner with a `Runner.Worker` (or
where pgrep cannot tell) is `deferred` to the next run, checked again right before the
swap. The new hook is staged in `glaeda-hooks/` and must `--parse-only` every hook call in
the installed wrappers (job-started, job-completed, listen, take-root, take-gui) under the
wrapper's own interpreter, or the runner is `blocked`, since a rejected job-started would
fail every job. `--apply`, `--uninstall` and `--refresh-hooks` share one lock
(`~/.local/state/glaeda/cmux-runner/runner.lock`); a refresh that finds it held waits for
the next hour. The listener
gate notices the new file and re-execs it, adopting its running `run.sh` ("the hook
changed; reloading the gate"). Wrapper, label or LaunchAgent changes still need
`--apply`. From an operator Mac, cmuxterm-hq's `fleet runner relabel HOST` stages glaeda's
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

### Build worker steps on the same ledger (cmuxterm-hq#794)

The build worker can run CI steps (cmuxterm-hq `ci-step` jobs) beside the runners' jobs, admitted by the
same `take_capacity`, so a mini claims a step only when it fits:

    glaeda-cmux-runner-hook fits    --capacity-units U
    glaeda-cmux-runner-hook admit   --class light --job-key KEY --watch-pid WORKER_PID --capacity-units U
    glaeda-cmux-runner-hook release --job-key KEY

Pass the `--capacity-units` (and `--compile-slots`, `--canonical-roots`) the runners' hooks bake, and the
worker's `--host-lock`, `--reservation` and `--capacity-dir`, so both scan one ledger (on the minis the worker
runs as `cmux` with root `/Users/Shared/cmux-build-fleet`, the runners' `FLEET_DIR`). `fits` prints the step
classes (`light`, `isolated`) the mini could admit now, from the listener gate's probe, and none while a fleet
build holds or waits for the host lock. `admit` takes the class's units without waiting and leaves them with a
holder that watches the worker's pid, or exits 1 with the capacity reason; a key admits once. The holder files
are named by a digest of the key, so one key's `release` never touches another's. Each prints one JSON line.
A dev build's exclusive host lock and a step's shared one exclude each other, as with runner jobs. Steps take no
root, persistent-dd, gui or simulator token yet: the worker is a system LaunchDaemon outside the console
session, and root classes move behind `admit` with compile placement.

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
id. It costs 2 units (isolated), or 1 unit when its id ends in `-light`, or 1 unit plus the one
simulator token when its id ends in `-sim` or `simulator`. A repository the hook cannot identify is a
guest. cmux jobs wait for room with no time limit (its rescue workflow moves one that waits too long); a
guest has no rescue, so it waits up to `--guest-wait` (600 s) for room before it is refused. Guests share
the runner user's `$HOME` and `/Users/Shared/cmux-build-fleet`, so the trust boundary is anyone who
can push a branch to any repository in the group (outside collaborators and bots such as Dependabot
included), and these hosts hold no secrets.

## 2f. Canonical roots

Compiles build in a canonical root (/private/tmp/cmux-ci; cmux#14338 adds /private/tmp/cmux-ci-2 and on),
and app-host test consumers restore a product there with `rm -rf <root>/src`, because `#filePath` is baked
in at the producer's root. So one root job per root per mini:

- Root jobs are compile (macos-compile-admission and any unknown job id), compile-gui (test-e2e's `build`:
  a producer that takes the gui token later, in its own step with take-gui, and no persistent-dd: it only
  clones its root's kept state, which the root token already guards), gui (tests-build-and-lag,
  app-host-test-rerun's `rerun`, test-e2e's `test`), gui-step (app-host-unit-tests: a gui job that takes
  the gui token with take-gui before its restore step takes the root, the order gui jobs take them in) and
  product (cli-product-tests, which also holds the gui token).
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
  (the seed prefetch's blobless mirror `ci/.prefetch/cmux.git`, trees only, 3 s for all roots), plus the kept
  pull request's own files from its stamp (`pr_app_swift_files`) and the job's own (the event's
  `changed_files`, an upper bound), neither of which counts when the kept build is the same pull request's; a
  build of the job's pull request parked on the root (`pr-builds/pr-<n>`, which admission swaps back in)
  counts as that root's kept build. All that is put
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
  gui token (an app-host shard in the step before its restore instead), and the producer's root in its
  restore step.
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
- A root's token comes free when its job ends, but its processes may not have. A step that ignores the
  runner's SIGINT and SIGTERM has its process tree killed, and what xcodebuild started outside that tree
  (its build service and compilers) keeps writing the root for seconds (cmux run 36312829569: the next
  holder's `rm -rf <root>/src` failed with "Directory not empty"). So whenever a job takes a root, at
  admission or with `take`, the hook first stops this user's leftovers still using it: a process with a
  working directory in the root (lsof) or an argument naming a path inside it (ps) that is under no live
  Runner.Worker. SIGTERM, then SIGKILL after 3 s, for up to 15 s. Processes of live jobs are spared (a
  job that switched roots may still name its old one), and so is the hook itself. The admission line or
  take-root's stderr says `stopped N leftover process(es) in <root>`.
- A producer's root this mini does not have (a product compiled at `/private/tmp/cmux-ci-2` on a two-root
  mini, restored on a one-root mini) is still a valid path to alias: nothing compiles there on this mini.
  Every taker holds that root's `root-N.token`, so two jobs never own its alias at once. A consumer that
  already holds every root this mini has (its admission on a one-root mini) takes it beside them, as an
  exception to one root per job; it cannot deadlock, since the root's other takers hold none of this mini's.
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
- `ios-simulator` and `screenshots` are `simulator` (1 unit, p75 0.15 cores per job,
  plus the per-mini `simulator` token: they
  reuse, erase and boot named devices in the user's one CoreSimulator service). A job refused only for
  that token waits for it at job start, as any capacity wait does.
- `validate` (ios-streamed-validate) stays on Blacksmith. It binds fixed ports, restarts a local Postgres
  under /tmp, changes the GUI session (open, launchctl setenv, system dark mode) and writes credentials
  to `$HOME`.

## 2h. Test keychain

The cmux user's login keychain is locked in the runner's launchd session, so tests that add keychain items
fail with errSecInteractionNotAllowed. On PR runners (not trusted ones) the job-started hook creates
`~/Library/Keychains/cmux-ci.keychain-db` with an empty password and no lock timeout, unlocks it, and makes
it first in the user search list and the user's default keychain. It holds test junk only, and every PR job
can read it.

Unlock state is per security session, and each runner's LaunchAgent has `SessionCreate`, so that unlock
never reaches the desktop. There the default keychain stayed locked, and Spotlight put "Spotlight wants to
use the cmux-ci keychain" over UI tests (cmux run 36309272077). The hook therefore also writes and kickstarts
`com.teamleaderleo.glaeda.test-keychain-unlock`, a LaunchAgent limited to the Aqua session that runs
`security unlock-keychain` on the test keychain. Its RunAtLoad unlocks it again at every login.

The unlock alone does not clear a prompt that is already up. At every login the keychain starts locked in the
new desktop session, and daemons ask for the default keychain within seconds, usually before the agent runs.
securityd queues one SecurityAgent prompt per request and keeps it after the keychain is unlocked. Killing
SecurityAgent cancels only the prompt on screen; securityd starts a new one for the next queued prompt. On
cmux14 and cmux8s (2026-09-27) prompts queued at the 09-26 reboot were still up the next morning, and the
assistantd one came back each time the cmux e2e action closed SecurityAgent (cmux run 36317492985). So after
the unlock the agent kills SecurityAgent until no queued prompt has started it again for 15 s (at most 30
kills): the next queued prompt takes 6 to 9 s to appear (cmux-mac-mini, 2026-09-27), so stopping at the first
quiet check a second later left it up. It does so at every job start and every login. No keychain prompt is wanted on a PR mini. When the hook changes the
agent, it boots the old one out before loading the new one, since a loaded agent keeps running its old
program.

`cmux-ci` stays the user's default keychain. `swift test` runs in a runner's own session, where the login
keychain is locked, and tests that add items without naming a keychain need an unlocked default. The
default is per user, not per session: macOS refuses `security list-keychains -d dynamic -s` and
`default-keychain -d dynamic -s` ("The specified preferences domain is not valid"). UI and app-host tests use
the desktop's session anyway: their xcodebuild goes through `launchctl asuser`, which joins it.

Crash and panic dialogs have the same shape. Diagnostics Reporter draws "Your computer was restarted because
of a problem" after a kernel panic and "cmux DEV cannot be opened because of a problem" after a crashed launch.
Its LaunchAgent has two QueueDirectories, `/var/db/PanicReporter` and `/var/db/DiagnosticsReporter`: launchd
starts it while either holds an entry, and the entry stays until someone answers. On cmux8s a panic queued on
2026-09-25 sat over UI tests for two days, and the cmux e2e action's kill only made launchd start it again with
the same dialog about two minutes later. So at every job start on macOS the hook empties both queues (they are
world-writable), removes the unanswered `.contents.*` summary a queued panic points at directly in
`/Library/Logs/DiagnosticReports`, and then closes Diagnostics Reporter. The full panic and crash reports stay.
A symlinked entry is removed, never followed.

**Never store credentials as the runner user on a PR mini** (`gh auth login`, `git credential-osxkeychain`,
`security import`, Keychain Access). Without an explicit keychain they land in `cmux-ci`, and any later PR job
can copy that file and read them. Credentials belong on trusted or signing hosts.

## 2h2. A fresh testmanagerd for each XCTest job

Every macOS XCTest run on a mini goes through the runner user's `/usr/libexec/testmanagerd`, a launchd agent
started on demand. On cmux7s (2026-09-25) it stopped half way through tearing down a control session. From
then on it accepted xcodebuild's control connections without creating the IDE session, and every XCTest
run there failed after about 7.5 minutes with `The test runner hung before establishing connection` (exit 65)
for ten hours. `launchctl kickstart` is refused under SIP, and the wedged daemon ignored SIGTERM.

- On PR runners (`--recycle-testmanagerd`, baked in beside `--test-keychain`), a job that takes the gui
  token stops the user's testmanagerd, and launchd starts a fresh one at the job's first test. It happens at
  job start for a job that holds the gui token from admission (gui and product jobs), and in `take-gui`
  for test-e2e's `build`, whose tests start after it. The admission line (or take-gui's stderr) ends with
  `testmanagerd: stopped pid N`, `killed pid N (it ignored SIGTERM)`, `not running` or `kept`.
- It is kept while any `xctest` or `xcodebuild test`/`test-without-building` runs on the mini. The gui
  token keeps the console-session XCTest jobs apart; this check covers the rest: `swift test` (xctest) in
  swift-package-tests and the light side lanes, the simulator jobs' xcodebuild, and guests.
- SIGTERM first; after 3 s, SIGKILL unless a test has started meanwhile (then it is kept for that run).
  Zombies count as gone. A daemon that outlives SIGKILL by 5 s refuses the job
  (`refused: testmanagerd: stuck: ...`) and frees its capacity, so the refusal rescue runs it elsewhere.
  `glaeda-fleet-status` reports such refusals as `jobs.testmanagerd_stuck`; a person decides on a logout
  or reboot.
- The simulators' own testmanagerd (under the iOS runtime root) is never touched.

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
- **Mini load.** The stream spools to a file at line rate (2.3 GB in ~25 s), so the seeder's slot frees at
  once; only the extraction is paced beside a job, to 64 MiB/s (about 140 s for a seed). Pacing the stream
  itself held the slot 140 to 180 s, so the 180 s serve limit cut streams off (ssh exit 255, "Truncated tar
  archive") and other minis got `busy` (cmuxterm-hq#658, 2026-09-28: 4 of 10 LAN fetches succeeded).
- **Retries.** A `busy` answer is asked again after 15 to 45 s, and a cut stream or one that fails
  `zstd -t` after 2 to 8 s, up to 4 attempts inside 900 s. The record's `lan.attempts` counts them.
- **R2 pacing.** The R2 fetch after the LAN step used a fixed 3 MB/s whenever a job existed, which on
  2026-09-28 was every fetch (p50 766 s). Its Governor now reads the mini's inbound bytes on `en*` each
  second, subtracts the download's own, and pauses the download's process group only while a job is
  running, that job's traffic was over 512 KiB/s in the last 15 s, and the download is ahead of 3 MiB/s.
  A compiling or testing job leaves the link to the download. The record's `paced` has `hot_seconds`,
  `paused_seconds` and `peak_job_bps`.
- **Remaining exposure.** A PR job on a mini can rewrite `~/.config/glaeda/seed-lan/config.json` and
  `known_hosts` (same user), pointing that mini's LAN step at another host. That host could only feed
  that mini a seed, which a PR job there can already write directly; the seeder and other minis are
  unaffected. A follow-up could have `glaeda-mini-fleet check` hash both files.
- **Integrity.** The spooled stream must end with ssh exit 0 and pass `zstd -t` (every frame carries an
  XXH64 checksum of its content). The mini then extracts into `seeds/.lan-<pid>/x`, requires exactly one top-level directory
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

## 2i2. The seed archive on cmux-lawrence

cmuxterm-hq#821. cmux15 keeps only its recent seeds, and it is the only writer of the minis' j14 seeds. So
before the archive, 189 of 195 LAN requests from cmux12s missed there, and the other minis fetched every
seed from R2. That took 108-158 s idle and up to 760 s paced beside a job, one download per mini per seed.

cmux-lawrence runs no jobs and has TBs of SSD. It keeps every seed cmux15 keeps, and the PR minis read from it:

- **Fill.** The `glaeda-seed-archive` LaunchAgent runs every 15 seconds with `/usr/bin/python3`, so Local
  Network Privacy lets its `/usr/bin/ssh` through. Lawrence is a seed-lan client of cmux15. Each run asks for
  `seed-list-v1` and streams every seed the archive lacks, newest first, so a seed is here about a minute
  after cmux15 keeps it. At every 5 minutes, PR minis that prefetched in between missed here and fell back
  to R2. Runs that fetch and prune nothing are not logged.
- **Storage.** Each seed is kept as `/Volumes/glaeda-seed-archive/seeds/<KEY>.tar.zst`: the zstd stream
  exactly as cmux15 sent it, about 2.1-2.4 GB against 9 GB unpacked, so serving it is a read. The volume is
  an APFS sparse bundle on the ExFAT X10 Pro (`glaeda-seed-archive.sparsebundle`, attached by each run).
  macOS privacy lets a launchd job write into an attached image but not onto the external disk itself. The filler checks every file before renaming it into place: all entries
  under `KEY/`, and the seed manifest present.
- **Prune.** Oldest use first, while the archive holds more than 800 GiB or the disk has less than 300 GiB
  free. Serving a seed touches it.
- **Serving.** `glaeda-seed-serve --role archive` refuses everything on a host that has a glaeda runner
  receipt.

```sh
# the archive as a client of the trusted seeder
scripts/glaeda-seed-lan install --seeder cmux15 --address 172.20.21.202 cmux-lawrence --apply
# the PR minis read from the archive (replaces their seeder config)
scripts/glaeda-seed-lan install --seeder cmux-lawrence --role archive --user cmux-lawrence \
  --address 172.20.21.158 HOST... --apply
# on cmux-lawrence, over SSH: copy glaeda-seed-archive and glaeda-seed-prefetch to ~/.local/libexec, then
# (creates and attaches the image, loads the LaunchAgent)
~/.local/libexec/glaeda-seed-archive install --apply
```

The hq command `fleet seed-archive` runs all three. The fill log is
`~/Library/Logs/glaeda-seed-archive.jsonl` on cmux-lawrence, and the serve log is
`~/.local/state/glaeda/seed-serve/serve.jsonl`. A miss there records the wanted key.

Roll back by pointing the minis at cmux15 again (`install --seeder cmux15 ...`). The archive can stay; it
is regenerable.

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
- **Lookup.** `glaeda-lan-fetch` asks every peer `product-has-v1` at once and transfers from the first
  that answers `has`, trying a second one if that transfer fails. A slow or stalled peer then delays only
  a miss (at most 10 s per lookup), never a hit, and the least-loaded holder tends to answer first.
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

- **When.** No Runner.Worker or xcodebuild, no job started or ended for 3 minutes (`IDLE_S`), 1-minute load under
  0.25 per core, thermal pressure nominal, at least 136 GiB free (the 100 GiB admission floor plus a cold
  compile), no reservation, no fleet build holding or waiting for the host lock. Never on cmux-mac-mini (hostname cmuxs-Mac-mini-5, the production
  iOS soak box) or Lawrence's machines, never next to a trusted-only runner (a seeder), and only on
  capacity-mode runners. `touch ~/.config/glaeda/idle-warm.disabled` stops it on one mini.
- **Which root.** The hook's own prediction (`warm_root_costs`) of each root's compile for main's head, from
  the seed prefetch's mirror and the job-written model: a rebuild root first, then far, then one it could not
  compare, then one with no kept build (a cold build), skipping any already built or tried for that head. When the hook can compare no root at all (every kept stamp predates the fields it reads, as on the light
  minis after a quiet spell), every root counts as one it could not compare, so one catch-up per root re-stamps it. Three failed builds in a row pause it for six hours.
- **How.** It loads the hook the runners run (from their `glaeda-hooks/`) and refuses if that hook predates
  the yield below, so a rollout in either order is safe. Under `capacity/admission.lock` it takes the root's
  token, a persistent-dd token and a compile's units, with the host lock shared, writes
  `capacity/idle-warm.json` (its pid and those names), and lets the admission lock go. Then cmux's
  `scripts/ci/owned_catch_up.sh`, run from a clean checkout of main's head kept under `ci/.catch-up/cmux` (a shallow, blobless clone),
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

**Idle UI fuzzing (a preemptible lane).** The idle-fuzz LaunchAgent (`glaeda-idle-warm --apply --fuzz`, every
minute, from glaeda-mini-setup, so it ships with glaeda OTA) runs cmux's UI fuzzer on every mini with glaeda
runners, except NEVER_HOSTS (cmux-mac-mini, Lawrence's machines) and a mini with
`~/.config/glaeda/idle-fuzz.disabled`. It is not part of the catch-up: it runs at nice 10 beside compiles and
catch-ups, and holds no capacity unit, root or token, so no admission waits for it or refuses because of it.

- **Starts** when this user owns an unlocked console, no job holds the gui token or asks for it (a take-gui
  step), no Xcode test and no other cmux DEV app runs, the host has no reservation (someone dogfooding there),
  and 90 GiB are free. It clones the newest main build a root keeps into `fuzz/builds/<sha>` (APFS `cp -c`, no
  root token: the copy counts only if the root's stamp is unchanged after it) and exports the fuzzer at that
  commit (`git archive` of `scripts/fuzz` and `dogfood/fuzz` from the catch-up checkout or the seed mirror) into
  `fuzz/engines/<sha>`, since a catch-up may check out a newer main meanwhile. It fuzzes for up to 10 minutes
  and minimizes up to two failures, 26 minutes at most; runs land in `/Users/Shared/cmux-build-fleet/fuzz/runs`.
- **Stops** the moment a job wants the console. It names its pid in `capacity/idle-fuzz.json` (the hook's
  FUZZ_HOLDER); a job whose admission takes the gui token, or a step running take-gui, stops it first with SIGTERM,
  which kills its process group, the app included (the app is the fuzzer's child), within the hook's 5 s yield.
  The lane also watches for itself every 0.25 s (gui token, reservation) and every second (process list: an
  Xcode test, a take-gui, another cmux DEV app), so a runner whose hook predates FUZZ_HOLDER loses it too.
  A compile's admission leaves it alone.

- **Replays for the collector, on trusted-only minis only.** cmuxterm-hq's `build-fleet/fuzz/collect.py` files a
  finding only once it reproduces on a replay it asked for, since the build user (and so any pull request job)
  can write a finding. Its `mini-serve.sh` leaves a request in `fuzz/replays/req.XXXX/`: the collector's own
  `replay.py`, its own copy of the fuzzer and the checked repro steps. On a PR mini that proves nothing (a job
  there can rewrite the request, the kept build and the answer), so only a mini whose runners are trusted-only
  (2e, main pushes only) replays, and it never fuzzes. There the lane runs the oldest
  request (up to 5 minutes) against the newest main build, with the same start rules, holder and preemption and
  no disk floor, then writes `.done`; a job that stops it leaves the request for the next tick. A PR mini
  removes `fuzz/replays`. Requests older than a day are removed.

`glaeda-idle-warm --fuzz` prints whether it would run now (fuzz, or which replay), or why not; `--apply --fuzz`
runs it. The collector files the findings as cmux issues.

## 2l. Mini health: heal what the runner user can, report the rest

`glaeda-mini-health` (the 2-minute mini-health LaunchAgent from glaeda-mini-setup, a no-op without glaeda
runners) looks for what stopped runner minis on 2026-09-25 and 26 without anything noticing:

| Finding | Heal |
|---|---|
| `console_locked`, `console_no_user` | none: root and a reboot (the hook already refuses GUI jobs there, #1286) |
| `autologin_locks` (loginwindow `autoLoginUserScreenLocked`), `display_sleep` | none: root |
| `testmanagerd_wedged`: the newest session line of this user's testmanagerd is `XCIDESession is responsible for cleaning up its socket`, 90 s old, nothing after it | the hook's `recycle_testmanagerd` (#1281) while no test runs; a hook without it gets the finding only |
| `tailscale_down`: a BackendState other than `Running`, two runs in a row | `tailscale up` when Stopped, else `scutil --nc stop/start` of the Tailscale VPN service; a standalone tailscaled or a logged-out node is reported only |
| `tailscale_probe_error`: the Tailscale CLI gave no BackendState (timeout, error, no JSON) | reported only: the check could not look, so the tunnel is not judged down |
| `runner_stopped:<agent>`: a runner LaunchAgent loaded but not running, two runs in a row | `launchctl kickstart` (nothing runs in it, so no job is cut) |
| `runner_unloaded:<agent>` (not one `cmux_mini_fix.sh` holds) | none: `glaeda-cmux-runner-fleet --apply` |

A heal runs at most every 10 minutes and 3 times a day per finding (a heal that worked but did not last counts),
then the finding reads `impossible`. The report,
`~/.local/state/glaeda/mini-health/health.json` (`glaeda-mini-health/v1`: each finding's id, severity, first
sighting, evidence, `auto_fix` pending or impossible, and the heals of the last day), is what ci-dash's probe
reads; ci-dash's Health section names the operator command for the rest. `glaeda-mini-health` alone prints
the report without healing; `touch ~/.config/glaeda/mini-health.disabled` keeps it reporting but stops heals.

## 2m. Build mesh: the fleet index and kept state between PR minis

Compiled products (2j) are one object per build. The bigger saving is kept compile-admission state: a
pull request re-pushed onto a different mini starts cold there (PR 13504 cold-started on three minis on
2026-09-25) while another mini holds its previous build. The same mesh key and forced command carry two
more verbs, so no new server, port or GitHub call is involved:

- **`inventory-v1`** answers `glaeda-seed-serve 1 inventory SIZE` and SIZE bytes of JSON
  (`glaeda-lan-inventory/v1`): per canonical root, the stamp of the kept state (`derived-data` +
  `stamp.json` in `/Users/Shared/cmux-build-fleet/ci`, `ci/cmux-ci-<k>`, and each parked pull-request build
  `<store>/pr-builds/pr-<n>`) with its sha256, `kept_at` and size (Logs and Index.noindex left out, sizes
  cached per stamp), the cached product digests and the kept seed keys.
- **`state-v1 K SLOT STAMP CODEC`** streams `tar` (zstd -1 -T0 when both ends have zstd) of that kept state.
  The server first takes an APFS clone (`cp -cR`) and serves it only if the stamp bytes and the
  `derived-data` inode were the same before and after, so a job's `keep` racing it yields `miss`, never a
  torn tree. The clone is removed afterwards, and clones a killed serve left are swept by pid.
- **Fleet index.** The `lan-fetch` broker polls every peer's inventory every 60 s into
  `~/.local/state/glaeda/lan-fetch/fleet-index.json` (`glaeda-fleet-index/v1`); `glaeda-lan-fetch index`
  prints it. Readers (glaeda-cmux-runner-hook) never touch the network.
- **Pull.** `glaeda-lan-fetch state PEER K SLOT STAMP` (through the broker, Local Network Privacy) extracts
  into `ci/.lan-state-*`, requires the stamp it asked for, then swaps `derived-data` and `stamp.json` into
  root K's store the way cmux `keep` does. A state is only usable in the same canonical root (its
  fingerprint covers the root path), so a pull always goes root K to root K.
- **Trust.** Kept state has no recorded digest: a PR mini trusts it exactly as it trusts its own kept state,
  which any in-repo PR job there may have written (forks never run on the minis). The mesh is PR minis only,
  and the broker refuses to pull into a mini whose runner receipt names a trusted ref, so PR state never
  reaches the trusted seed chain (cmux15), and mini-6 is never in the mesh.

Link speed (2026-09-26, every mini): the M4 Pro minis have the built-in 1 GbE port (Broadcom 57762,
"Maximum Link Speed: 1 Gb/s", negotiated 1000baseT), not the 10 GbE option, and the Thunderbolt bridge is
inactive. So a kept state (8-12 GB on disk) moves in about 35-50 s through zstd, or ~90 s as plain tar.

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
