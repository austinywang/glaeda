---
name: glaeda-runner-change
description: "Change and roll out glaeda's cmux runner tooling safely: glaeda-cmux-runner, glaeda-cmux-runner-hook (admission, capacity ledger, lock holders, listener gate, job telemetry), glaeda-cmux-runner-fleet, glaeda-fleet-status and glaeda-mini-fleet. Use before editing any of those scripts or their docs, and when deploying a hook change to the minis."
---

# Change the glaeda runner tooling

The hook runs inside every CI job on every mini. A crash in `job-started` refuses the job; a lock bug strands a
mini. Keep changes small, observable and reversible.

## Before editing

- Read `AGENTS.md` and the contract doc for the surface: `docs/CMUX_MINI_RUNNER.md` (hook, runner, capacity,
  telemetry), `docs/FLEET_STATUS.md` (status sources and findings), `docs/CMUX_FLEET_ROLLOUT.md` (deploys).
- Work in your own worktree from `origin/main` (`git worktree add ~/Projects/glaeda-worktrees/NAME -b BRANCH
  origin/main`). The main checkout may be in use by fleet jobs.

## Rules the tests enforce

- No em dashes (U+2014) in the runner, hook, fleet, status tooling or their docs (`NoEmDashTest`).
- Output stays bounded and typed. No secrets, raw logs, environment dumps, process arguments or private paths
  (home paths become `~`). Job telemetry names processes by executable basename and user only.
- Subprocesses use absolute paths and argument vectors (`/bin/ps`, `/usr/sbin/sysctl`), with timeouts.
- Observation never becomes admission: telemetry and status findings must not refuse, delay or fail a job.
- Anything forked from the hook detaches twice, points stdio at /dev/null (the runner reads the hook's output
  until EOF), and must not keep lock fds it does not own. A pid file for a new helper must not match the
  `host-lock-holder*` glob, or the listener gate will count it as a lock holder.
- A new script goes into the `compile()` path list in `.github/workflows/ci.yml`; a new `scripts/test-*.py` goes
  into the test list there.

## Test

```bash
python3 scripts/test-glaeda-cmux-runner.py          # hook, gate, runner (about 6 min)
python3 scripts/test-glaeda-cmux-runner.py JobTelemetryTest -v
python3 scripts/test-glaeda-fleet-status.py
python3 scripts/test-glaeda-cmux-runner-fleet.py
python3 scripts/test-glaeda-mini-fleet.py
./scripts/verify fast                                # Rust, when src/ changes
```

The hook test harness sets `GLAEDA_RUNNER_TELEMETRY=0`; tests that need the sampler enable it explicitly.
CI runs on Linux: guard macOS-only commands (`sysctl vm.loadavg`, `pmset`) so they degrade to `None`.

## Review and merge

Concurrency, persistence and privilege changes (locks, holders, capacity, anything forked from the hook) need an
independent review of the exact head before merge; a read-only subagent review with the diff and the failure
modes above is the usual route. Routine low-risk changes may be self-reviewed after checks pass. Docs-only PRs
skip Verify; say so in the PR.

## Roll out

```bash
scripts/glaeda-cmux-runner-fleet                     # plan: eligible members and runner counts
scripts/glaeda-cmux-runner-fleet --apply --org manaflow-ai --group glaeda-minis --hosts ONE  # canary first
scripts/glaeda-cmux-runner-fleet --apply --org manaflow-ai --group glaeda-minis              # then the rest
```

The runners are org-scoped. An apply without `--org` refuses every instance ("already has a runner install")
and changes nothing. Run it from a worktree at `origin/main`, not the shared base checkout.

job-started.sh runs the hook file fresh for every job, so a staged hook takes effect for the next job without a
restart. After the canary, run a job there and check the new behavior on the host (for telemetry:
`ssh MEMBER 'tail -n 1 ~/Library/Logs/glaeda-cmux-jobs.jsonl'`), then `scripts/glaeda-fleet-status --host MEMBER`.
Kill switch for telemetry without a redeploy: `GLAEDA_RUNNER_TELEMETRY=0` in the runner LaunchAgent environment.

Diagnosing a job or host rather than changing tooling: use the `glaeda-fleet-diagnose` skill.
