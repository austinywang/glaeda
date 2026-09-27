---
name: github-ci-wait
description: "Wait for GitHub CI, watch a workflow run, poll PR checks, wait for a reply comment (a bot's receipt), or answer 'is CI green' without burning the shared GitHub quota. Use glaeda-gh (a local daemon that is the only poller) instead of gh run watch, gh run view loops, sleep-and-read loops, or per-PR REST scans. Use whenever you would wait for CI, watch the run, poll checks, wait for a comment, check whether CI is green or a PR merged, or sweep many PRs."
---

# Wait on GitHub with glaeda-gh

Every session on this machine shares one GitHub account, so one REST quota of 5000 requests an
hour. Sessions polling on their own used it all up. `glaeda-gh` is a local daemon that is the
only process polling: one batched GraphQL query for every watched PR, ETag requests for runs
(an unchanged run costs nothing), and one batched query every 15 s for comment waits. Its
commands read the daemon's cache and make no API calls.
On the tailnet the daemon also follows the cmux build controller, which receives GitHub's run,
check and comment webhooks (cmuxterm-hq#675). A finished run, a PR check change or a bot's
reply then reaches `wait` within a few seconds at no quota cost. Public repos need no token;
private ones (cmuxterm-hq) need `~/.config/cmux/build-fleet/controller.token`. Without the
controller everything still works by polling, only slower; `glaeda-gh budget` says which.

## Wait and check

```bash
glaeda-gh wait pr OWNER/REPO#N --sha "$(git rev-parse HEAD)"   # until green; 1 at the first failed check
glaeda-gh wait pr OWNER/REPO#N --until done      # until every check finishes (0 green, 1 red, 4 conflict)
glaeda-gh wait pr OWNER/REPO#N --until merged    # ignores conflicts; 1 if closed without merging
glaeda-gh wait run OWNER/REPO/RUN_ID [--jobs]    # 0 success, 1 failure
glaeda-gh wait comment OWNER/REPO#N --author 'github-actions[bot]' --match 'REGEX' --since COMMENT_URL
glaeda-gh status pr OWNER/REPO#N                 # one look: state, mergeable, checks, review
glaeda-gh status run OWNER/REPO/RUN_ID --jobs
glaeda-gh status comment OWNER/REPO#N            # the latest comments
glaeda-gh budget                                 # REST and GraphQL left, the daemon's own use, its heartbeat
```

- `wait` exits 0 on success (or a matching comment), 1 on failure (including a PR closed without
  being merged), 2 on timeout (`--timeout S`, default 3600), 3 when the daemon is down, 4 when the
  PR conflicts with its base, 64 on bad arguments.
- Run one background `wait` per PR, run or comment (Bash `run_in_background`), not one wait
  looping over several: each then notifies you on its own when it exits, and you carry on meanwhile.
- The summary names what failed and the failed step, e.g.
  `failed: CI / build (step: Run selected tests)`, so you rarely need a REST call to see why.
  Only the checks GitHub marks required decide. The headline (`checks SUCCESS: ...`) is the
  verdict `wait` used; when GitHub's own rollup differs, the line says so. Non-required failures
  are marked `(not required)`, and attempts a newer attempt or run replaced are listed as
  `superseded` and not counted.
- `wait` only rules on data fetched after it started, so it answers within about a minute at the earliest.
- Add `--json` for machine-readable output. URLs work too: `glaeda-gh wait pr https://github.com/o/r/pull/12`.

## After a push: --sha

Pass the commit you pushed: `--sha "$(git rev-parse HEAD)"`. Checks of an older head then count
as pending until GitHub shows yours. Give the full 40-hex id. A shorter prefix is resolved in the
current checkout; one that does not resolve must match the head GitHub shows, or `wait` exits 64
at once rather than waiting for a commit that will never appear. For a run, a `--sha` that is not
the run's commit is also an error: a run's commit never changes.

## Merge conflicts: exit 4

A PR that conflicts with its base (mergeable `CONFLICTING`, merge state `DIRTY`) runs no
pull_request workflows until the next push. This happens to a stacked PR when the PR below it is
squash-merged and main moves under it. `wait pr --until green` and `--until done` stop with exit 4
as soon as the daemon sees it, printing `result: conflict` and `note: PR conflicts with its base`.
Rebase or merge the base, push, and wait again with the new `--sha`. The check uses the daemon's
cached PR data, so it costs no extra API calls. `UNKNOWN` mergeability (GitHub computes it
lazily) counts as pending, not as a conflict. With `--sha`, a conflict on an older head is pending
too. `--until merged` ignores conflicts and keeps waiting.

## Runner refusals and rescue attempts

A glaeda runner can refuse a job it was handed (its host is busy): the job fails within seconds at
the `Set up runner` step, and the repository's rescue (cmux: the owned-pool rescue sweeper)
re-runs it, usually on Blacksmith. `wait` treats such a failure as pending for `--rescue-grace`
seconds (default 360) and then decides on the next attempt; the summary says
`held: ... refused ... at setup`. With no new attempt within the grace, the failure stands and
the note says so. Any other failure in the same attempt decides at once.
`--rescue-grace 0` rules on each attempt as it finishes.

## Waiting for a comment

For a bot's reply, such as the callsign receipt on teamleaderleo/stensibly#454:

```bash
url=$(gh issue comment 454 --repo teamleaderleo/stensibly --body "/callsign reserve ...")
glaeda-gh wait comment teamleaderleo/stensibly#454 --author 'github-actions[bot]' \
  --match "${url##*#}\b" --since "$url" --timeout 300
```

- `--since` takes a comment URL or id (only later comments count), an ISO time, or a duration
  such as `10m`. Without it only comments posted after the wait began count, and a reply that came
  before you started waiting is missed. Pass your own comment's URL.
- `--author` accepts `github-actions[bot]` or `github-actions`. `--match` is a regular expression
  searched in the body.
- It prints the first matching comment (URL and body) and exits 0. A reply on a repo the
  controller receives webhooks for (teamleaderleo/stensibly among them) arrives within seconds;
  otherwise the daemon reads the latest 50 comments every 15 s while someone waits.

## When the daemon is down or hung

`glaeda-gh` exits 3 and prints how to start the daemon. Start it yourself, then wait again. Do not
fall back to polling GitHub yourself.

- macOS: `launchctl kickstart gui/$(id -u)/com.teamleaderleo.glaeda.gh-watch`. If launchctl
  cannot find the service, load it:
  `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.teamleaderleo.glaeda.gh-watch.plist`.
- Linux: `systemctl --user start glaeda-gh`.
- Not installed: `glaeda-mini-setup --hygiene-only --apply` from the glaeda checkout.

If it prints `looks hung` (it holds its lock but has not written a heartbeat for 10 minutes),
restart it: `launchctl kickstart -k gui/$(id -u)/com.teamleaderleo.glaeda.gh-watch` or
`systemctl --user restart glaeda-gh`.

If it prints `every cycle since ... has failed with ERROR at SITE`, the daemon runs but a bug makes
each cycle raise, so a restart will not help. Tell the user (the message names the code site), and
meanwhile use single `gh` reads, never a polling loop.

## Do not

- `gh run watch`, `gh pr checks --watch`, or `gh run view` / `gh pr view` / `gh api .../comments`
  in a loop.
- Per-PR REST scans: listing PRs, then `gh api repos/.../pulls/<n>/files` (or `/reviews`, `/commits`) for each.
- `gh run view --json jobs` just to learn which step failed: the `wait` summary already names it.

## Fine to do directly

- Single reads, merges, comments, reviews, labels, and dispatches with `gh`.
- Logs: `gh run view RUN_ID --log-failed` once a run has failed.
- One-off reads across many PRs: a single batched GraphQL query with aliases
  (`p0: repository(owner:, name:) { pullRequest(number:) { ... } } p1: ...`) through `gh api graphql`.
  GraphQL has its own 5000-point quota, and one aliased query costs about 1 point.
- Before a sweep that needs many REST calls, run `glaeda-gh budget` first and keep well above 500 left.
