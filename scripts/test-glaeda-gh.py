#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-gh, with a fake GitHub transport."""

from __future__ import annotations

import contextlib
import fcntl
import importlib.machinery
import importlib.util
import io
import json
import os
import re
import sys
import tempfile
import threading
import time
import socket
import unittest
from unittest import mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_gh", os.fspath(ROOT / "scripts" / "glaeda-gh"))
spec = importlib.util.spec_from_loader("glaeda_gh", loader)
gg = importlib.util.module_from_spec(spec)
sys.modules["glaeda_gh"] = gg
loader.exec_module(gg)

TOKEN = "ghp_faketoken_must_never_touch_disk"
NOW = float(int(time.time()))  # watch files carry real mtimes


def pr_node(number: int, state: str = "OPEN", rollup: str | None = "PENDING", checks: list | None = None) -> dict:
    return {
        "number": number, "url": f"https://github.com/o/r/pull/{number}", "title": "t", "state": state,
        "isDraft": False, "merged": state == "MERGED", "mergedAt": None, "mergeable": "MERGEABLE",
        "mergeStateStatus": "BLOCKED", "headRefOid": "a" * 40, "reviewDecision": None, "updatedAt": "x",
        "commits": {"nodes": [{"commit": {"statusCheckRollup": None if rollup is None else {
            "state": rollup,
            "contexts": {"totalCount": len(checks or []), "pageInfo": {"hasNextPage": False}, "nodes": checks or []},
        }}}]},
    }


def check_run(name: str, status: str = "COMPLETED", conclusion: str | None = "SUCCESS") -> dict:
    return {"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion,
            "detailsUrl": f"https://ci/{name}"}


def iso(epoch: float) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(epoch))


def job(name: str, conclusion: str = "failure", run_attempt: int = 1, runner: str = "cmuxs-mac-mini-3-glaeda-1",
        labels: tuple = ("glaeda-root-std-xcode-26.6",), failed_step: str = "Run selected tests",
        seconds: int = 300, end: float | None = None) -> dict:
    """A REST job; a failed one fails at `failed_step`, `seconds` after it started."""
    end = time.time() if end is None else end
    steps = [{"name": "Set up job", "conclusion": "success"}]
    if conclusion == "failure":
        steps.append({"name": failed_step, "conclusion": "failure"})
    return {"id": abs(hash(name)) % 1000, "name": name, "status": "completed", "conclusion": conclusion,
            "html_url": f"https://ci/job/{name}", "run_attempt": run_attempt, "runner_name": runner,
            "labels": list(labels), "started_at": iso(end - seconds), "completed_at": iso(end), "steps": steps}


def cached(*jobs: dict) -> dict:
    """Jobs as the daemon caches them."""
    return {"jobs": gg.slim_jobs({"jobs": list(jobs)})}


def refused(name: str = "build", **kw) -> dict:
    """What glaeda's runner hook leaves when it refuses a job: Set up runner failed within seconds."""
    return job(name, failed_step="Set up runner", seconds=7, **kw)


class FakeGitHub:
    """Answers GraphQL and REST like api.github.com; records every request."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, dict, bytes | None]] = []
        self.prs: dict[tuple[str, str, int], dict] = {}
        self.runs: dict[int, dict] = {}
        self.jobs: dict[int, list] = {}
        self.issues: dict[tuple[str, str, int], dict] = {}
        self.steps: dict[str, list] = {}  # check run node id -> steps
        self.step_queries: list[list[str]] = []
        self.rest_remaining = 4000
        self.graphql_remaining = 4000

    def rate(self, resource: str, remaining: int) -> dict:
        return {"x-ratelimit-limit": "5000", "x-ratelimit-remaining": str(remaining),
                "x-ratelimit-reset": str(int(NOW + 1800)), "x-ratelimit-resource": resource}

    def __call__(self, method: str, url: str, headers: dict, body: bytes | None) -> tuple[int, dict, bytes]:
        self.calls.append((method, url, dict(headers), body))
        assert headers["Authorization"] == f"Bearer {TOKEN}"
        if url.endswith("/graphql"):
            doc = json.loads(body)
            v, data, errors = doc["variables"], {}, []
            self.graphql_remaining -= 1
            data["rateLimit"] = {"limit": 5000, "remaining": self.graphql_remaining, "resetAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(NOW + 1800)), "cost": 1}
            if "nodes(ids:" in doc["query"]:
                self.step_queries.append(list(v["ids"]))
                data["nodes"] = [{"id": i, "steps": {"nodes": self.steps[i]}} if i in self.steps else None
                                 for i in v["ids"]]
            comments = "issueOrPullRequest" in doc["query"]
            alias, field, table = ("c", "issueOrPullRequest", self.issues) if comments else ("p", "pullRequest", self.prs)
            i = 0
            while f"o{i}" in v:
                node = table.get((v[f"o{i}"], v[f"r{i}"], v[f"n{i}"]))
                data[f"{alias}{i}"] = {field: node}
                if node is None:
                    errors.append({"type": "NOT_FOUND", "path": [f"{alias}{i}", field], "message": "Could not resolve"})
                i += 1
            out = {"data": data, **({"errors": errors} if errors else {})}
            return 200, self.rate("graphql", self.graphql_remaining), json.dumps(out).encode()
        path = url.split("api.github.com", 1)[1]
        parts = path.split("?")[0].split("/")
        run_id = int(parts[6])
        payload = {"jobs": self.jobs.get(run_id, [])} if parts[-1] == "jobs" else self.runs[run_id]
        etag = '"' + str(abs(hash(json.dumps(payload, sort_keys=True)))) + '"'
        if headers.get("If-None-Match") == etag:
            return 304, self.rate("core", self.rest_remaining), b""
        self.rest_remaining -= 1
        return 200, {**self.rate("core", self.rest_remaining), "etag": etag}, json.dumps(payload).encode()

    def rest_calls(self) -> list:
        return [c for c in self.calls if not c[1].endswith("/graphql")]

    def gql_calls(self) -> list:
        return [c for c in self.calls if c[1].endswith("/graphql")]

    def run_calls(self) -> list:
        return [c for c in self.rest_calls() if "/jobs" not in c[1]]

    def job_calls(self) -> list:
        return [c for c in self.rest_calls() if "/jobs" in c[1]]


class Base(unittest.TestCase):
    def setUp(self) -> None:
        # watch files carry real mtimes, so the fake clock starts at each test, not at import:
        # a test reached 20 s into the suite saw its registrations as newer than NOW + 10 ticks
        global NOW
        NOW = float(int(time.time()))
        self.tmp = tempfile.TemporaryDirectory()
        self.saved = (gg.BASE, gg.WAIT_POLL, gg.DOWN_GRACE)
        gg.BASE = Path(self.tmp.name) / "gh"
        gg.WAIT_POLL = 0.05
        gg.DOWN_GRACE = 0.3
        self.gh = FakeGitHub()
        self.daemon = gg.Daemon(transport=self.gh, token=lambda: TOKEN, controller=None)

    def tearDown(self) -> None:
        gg.BASE, gg.WAIT_POLL, gg.DOWN_GRACE = self.saved
        self.tmp.cleanup()

    def age_watch(self, key: str, seconds: float) -> None:
        path = gg.watch_dir() / gg.file_name(key)
        os.utime(path, (NOW - seconds, NOW - seconds))


class FakeController:
    """Stands in for the cmux build controller: /v1/github/runs/<id> and the change feed. A change
    to `runs` shows up in the feed, as a webhook delivery would; suites and comments are queued."""

    def __init__(self) -> None:
        self.runs: dict[int, dict] = {}
        self.suites: list[dict] = []
        self.comments: list[dict] = []
        self.calls: list[int] = []  # run() reads
        self.feeds: list[int | None] = []  # events() reads, by cursor
        self.down = False
        self.token = "t"
        self.seq = 0
        self.sent: dict[int, dict] = {}

    def fail(self) -> None:
        if self.down:
            raise self.down if isinstance(self.down, gg.WatchError) else gg.WatchError("controller: URLError")

    def run(self, run_id: int) -> dict | None:
        self.calls.append(run_id)
        self.fail()
        return self.runs.get(run_id)

    def events(self, since: int | None) -> dict:
        self.feeds.append(since)
        self.fail()
        changed = [dict(r) for i, r in self.runs.items() if self.sent.get(i) != r]
        self.sent = {i: dict(r) for i, r in self.runs.items()}
        page = {"cursor": self.seq, "more": False, "runs": [], "suites": [], "comments": [],
                "receiver_last_delivery": "2026-09-25T10:00:00Z"}
        if since is None:  # the start: only the cursor
            self.suites, self.comments = [], []
            return page
        self.seq += len(changed) + len(self.suites) + len(self.comments)
        page.update(cursor=self.seq, runs=changed, suites=self.suites, comments=self.comments)
        self.suites, self.comments = [], []
        return page


def ctl_run(run_id: int, status: str, conclusion: str = "", updated: str = "2026-09-25T10:00:00Z", repo: str = "o/r") -> dict:
    return {"id": run_id, "repo": repo, "name": "seed", "event": "workflow_dispatch", "head_sha": "a" * 40,
            "attempt": 1, "status": status, "conclusion": conclusion, "url": f"https://x/{run_id}", "updated_at": updated}


class ControllerTest(Base):
    def setUp(self) -> None:
        super().setUp()
        self.ctl = FakeController()
        self.daemon = gg.Daemon(transport=self.gh, token=lambda: TOKEN, controller=self.ctl)

    def test_tracked_run_costs_no_quota(self) -> None:
        self.ctl.runs[21] = ctl_run(21, "in_progress")
        key = gg.parse_key("run", "o/r/21")
        gg.register(key)
        for i in range(5):
            self.daemon.tick(NOW + i * gg.CYCLE)
        self.assertEqual(self.gh.rest_calls(), [])
        self.ctl.runs[21] = ctl_run(21, "completed", "failure", "2026-09-25T10:05:00Z")
        self.gh.runs[21] = {"id": 21, "status": "completed", "conclusion": "failure", "run_attempt": 1,
                            "updated_at": "2026-09-25T10:05:00Z"}
        self.gh.jobs[21] = [job("build", end=NOW)]
        self.daemon.tick(NOW + 5 * gg.CYCLE + gg.TICK)
        entry = gg.read_cache(key)
        self.assertEqual((entry["source"], entry["data"]["conclusion"]), ("controller", "failure"))
        self.assertEqual(gg.verdict("run", entry["data"], jobs=entry["jobs"]), gg.EXIT_FAIL)
        # the failed attempt's job list is read once, for its failed steps; nothing after that
        self.assertEqual((len(self.gh.run_calls()), len(self.gh.job_calls())), (1, 1))
        self.assertEqual(entry["jobs"][0]["failedSteps"], ["Run selected tests"])
        for i in range(1, 4):
            self.daemon.tick(NOW + 5 * gg.CYCLE + gg.TICK + i * gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 2)

    def test_refusal_reported_by_the_controller_is_held_until_the_rescue_attempt(self) -> None:
        now = time.time()
        self.ctl.runs[31] = ctl_run(31, "completed", "failure", iso(now))
        self.gh.runs[31] = {"id": 31, "status": "completed", "conclusion": "failure", "run_attempt": 1,
                            "updated_at": iso(now)}
        self.gh.jobs[31] = [refused(end=now)]
        key = "run:o/r/31"
        gg.register(key)
        self.age_watch(key, 1)  # the reader came before the first fetch; a later one would get a REST confirmation
        self.daemon.tick(NOW)
        entry = gg.read_cache(key)
        self.assertIsNone(gg.verdict("run", entry["data"], jobs=entry["jobs"]))  # held for the rescue
        self.ctl.runs[31] = {**ctl_run(31, "in_progress", "", iso(now + 50)), "attempt": 2}
        self.daemon.tick(NOW + gg.TICK)
        entry = gg.read_cache(key)
        self.assertEqual((entry["data"]["run_attempt"], entry["data"]["status"]), (2, "in_progress"))
        self.ctl.runs[31] = {**ctl_run(31, "completed", "success", iso(now + 600)), "attempt": 2}
        self.daemon.tick(NOW + 2 * gg.TICK)
        entry = gg.read_cache(key)
        self.assertEqual(gg.verdict("run", entry["data"], jobs=entry.get("jobs")), gg.EXIT_OK)
        self.assertEqual(len(self.gh.rest_calls()), 2)  # only attempt 1's run and job list

    def test_safety_net_reads_rest_every_15_minutes(self) -> None:
        self.ctl.runs[22] = ctl_run(22, "in_progress")
        self.gh.runs[22] = {"id": 22, "status": "in_progress", "updated_at": "2026-09-25T10:00:00Z"}
        gg.register("run:o/r/22")
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.REST_SAFETY - gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 0)
        # the waiting reader keeps touching its watch; without that it expires at IDLE_EXPIRY,
        # which equals REST_SAFETY (the test passed only while NOW trailed the registration)
        self.age_watch(gg.parse_key("run", "o/r/22"), -(gg.REST_SAFETY - gg.CYCLE))
        self.daemon.tick(NOW + gg.REST_SAFETY + 1)
        self.assertEqual(len(self.gh.rest_calls()), 1)

    def test_lost_delivery_is_not_undone(self) -> None:
        self.ctl.runs[23] = ctl_run(23, "in_progress")
        self.gh.runs[23] = {"id": 23, "status": "completed", "conclusion": "success", "updated_at": "2026-09-25T10:09:00Z"}
        key = gg.parse_key("run", "o/r/23")
        gg.register(key)
        self.daemon.tick(NOW)
        self.age_watch(key, -(gg.REST_SAFETY - gg.CYCLE))  # a reader is still waiting (see above)
        self.daemon.tick(NOW + gg.REST_SAFETY + 1)
        self.daemon.tick(NOW + gg.REST_SAFETY + 1 + gg.TICK)
        self.assertEqual(gg.read_cache(key)["data"]["conclusion"], "success")

    def test_unknown_run_uses_rest(self) -> None:
        self.gh.runs[24] = {"id": 24, "status": "completed", "conclusion": "success"}
        gg.register("run:o/r/24")
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.assertEqual(gg.read_cache("run:o/r/24")["source"], "rest")

    def test_controller_down_backs_off(self) -> None:
        self.ctl.down = True
        self.gh.runs[25] = {"id": 25, "status": "in_progress"}
        gg.register("run:o/r/25")
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.TICK)
        self.assertEqual(len(self.ctl.feeds), 1)
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.assertEqual(self.daemon.controller_status["state"], "down")
        self.daemon.tick(NOW + gg.CONTROLLER_RETRY + 1)  # still down: the wait doubles
        self.assertEqual(len(self.ctl.feeds), 2)
        self.daemon.tick(NOW + 2 * gg.CONTROLLER_RETRY + 2)
        self.assertEqual(len(self.ctl.feeds), 2)
        self.ctl.down = False
        self.daemon.tick(NOW + 3 * gg.CONTROLLER_RETRY + 2)
        self.assertEqual(len(self.ctl.feeds), 3)
        self.assertEqual(self.daemon.controller_status["state"], "ok")
        self.assertEqual(self.daemon.controller_failures, 0)

    def test_job_lists_and_other_repos_stay_on_rest(self) -> None:
        self.ctl.runs[26] = ctl_run(26, "in_progress", repo="other/repo")
        self.gh.runs[26] = {"id": 26, "status": "in_progress"}
        gg.register("run:o/r/26")
        self.gh.runs[27] = {"id": 27, "status": "in_progress"}
        self.ctl.runs[27] = ctl_run(27, "in_progress")
        gg.register("run:o/r/27", jobs=True)
        self.daemon.tick(NOW)
        self.assertEqual(self.ctl.calls, [26])
        self.assertEqual(gg.read_cache("run:o/r/26")["source"], "rest")
        self.assertEqual(gg.read_cache("run:o/r/27")["source"], "rest")

    def test_new_reader_of_a_finished_run_gets_one_rest_confirmation(self) -> None:
        self.ctl.runs[28] = ctl_run(28, "completed", "success")
        self.gh.runs[28] = {"id": 28, "status": "completed", "conclusion": "success", "updated_at": "2026-09-25T10:00:00Z"}
        key = "run:o/r/28"
        gg.register(key)
        self.age_watch(key, 200)
        self.daemon.tick(NOW - 100)
        self.assertEqual(self.gh.rest_calls(), [])
        stale = gg.read_cache(key)["dataAt"]
        self.daemon.tick(NOW - 98)
        self.assertEqual(gg.read_cache(key)["dataAt"], stale, "unchanged controller data keeps its time")
        self.age_watch(key, 0)  # a new reader
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.assertEqual(gg.read_cache(key)["dataAt"], NOW)
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 1, "confirmed once, not every cycle")

    def test_rerun_before_its_webhook_is_not_answered_with_the_old_attempt(self) -> None:
        self.ctl.runs[29] = ctl_run(29, "completed", "failure")
        self.gh.runs[29] = {"id": 29, "status": "queued", "conclusion": None, "run_attempt": 2,
                            "updated_at": "2026-09-25T10:30:00Z"}
        key = "run:o/r/29"
        gg.register(key)
        self.age_watch(key, 200)
        self.daemon.tick(NOW - 100)
        self.age_watch(key, 0)
        self.daemon.tick(NOW)
        entry = gg.read_cache(key)
        self.assertEqual((entry["source"], entry["data"]["status"]), ("rest", "queued"))
        self.daemon.tick(NOW + gg.TICK)  # the controller has not heard of the rerun yet
        self.assertEqual(gg.read_cache(key)["data"]["status"], "queued")
        self.ctl.runs[29] = {**ctl_run(29, "completed", "success", "2026-09-25T10:40:00Z"), "attempt": 2}
        self.daemon.tick(NOW + 2 * gg.TICK)
        entry = gg.read_cache(key)
        self.assertEqual((entry["source"], entry["data"]["conclusion"], entry["data"]["run_attempt"]), ("controller", "success", 2))

    def test_controller_down_after_tracking_resumes_rest(self) -> None:
        self.ctl.runs[30] = ctl_run(30, "in_progress")
        self.gh.runs[30] = {"id": 30, "status": "in_progress"}
        gg.register("run:o/r/30")
        self.daemon.tick(NOW)
        self.assertEqual(self.gh.rest_calls(), [])
        self.ctl.down = True
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 1, "no 15-minute blackout while the controller is down")

    def test_controller_without_the_receiver_is_quiet(self) -> None:
        self.ctl.down = gg.NoReceiver("controller HTTP 404")
        self.gh.runs[32] = {"id": 32, "status": "in_progress"}
        gg.register("run:o/r/32")
        status = self.daemon.tick(NOW)
        self.assertEqual(status["errors"], [])
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.daemon.tick(NOW + gg.CONTROLLER_RETRY + 1)
        self.assertEqual(len(self.ctl.feeds), 1)  # not asked again for 15 minutes
        self.daemon.tick(NOW + gg.CONTROLLER_ABSENT + 1)
        self.assertEqual(len(self.ctl.feeds), 2)

    def test_one_feed_read_a_tick_after_the_baseline(self) -> None:
        for i in (40, 41, 42):
            self.ctl.runs[i] = ctl_run(i, "in_progress")
            gg.register(f"run:o/r/{i}")
        for t in range(10):
            self.daemon.tick(NOW + t * gg.TICK)
        self.assertEqual(sorted(self.ctl.calls), [40, 41, 42])  # one baseline each, then only the feed
        self.assertEqual(len(self.ctl.feeds), 10)
        self.ctl.runs[41] = ctl_run(41, "completed", "success", "2026-09-25T10:03:00Z")
        self.daemon.tick(NOW + 10 * gg.TICK)
        self.assertEqual(gg.read_cache("run:o/r/41")["data"]["conclusion"], "success")
        self.assertEqual(len(self.ctl.calls), 3)
        self.assertEqual(self.gh.rest_calls(), [])
        self.assertEqual(self.daemon.tick(NOW + 11 * gg.TICK)["controller"]["runs"], 3)

    def test_feed_reset_takes_a_new_baseline(self) -> None:
        self.ctl.runs[43] = ctl_run(43, "in_progress")
        gg.register("run:o/r/43")
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.TICK)
        self.assertEqual(self.ctl.calls, [43])
        real = self.ctl.events
        self.ctl.events = lambda since: {**real(since), "reset": True, "cursor": 0}
        self.daemon.tick(NOW + 2 * gg.TICK)  # the controller lost its state
        self.ctl.events = real
        self.daemon.tick(NOW + 3 * gg.TICK)
        self.assertEqual(self.ctl.calls, [43, 43])

    def test_check_events_nudge_a_watched_pr(self) -> None:
        node = pr_node(1)
        node["headRefName"] = "feat/x"
        self.gh.prs[("o", "r", 1)] = node
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.gql_calls()), 1)
        self.daemon.tick(NOW + gg.NUDGE_MIN)  # nothing happened: no read before the cycle
        self.assertEqual(len(self.gh.gql_calls()), 1)
        self.ctl.suites.append({"id": 1, "repo": "o/r", "head_sha": "a" * 40, "status": "completed"})
        self.daemon.tick(NOW + gg.NUDGE_MIN + gg.TICK)
        self.assertEqual(len(self.gh.gql_calls()), 2)  # re-read at once
        # a push: runs for a new head on the PR's branch nudge it too, but not more than every NUDGE_MIN
        self.ctl.runs[50] = {**ctl_run(50, "queued"), "head_sha": "c" * 40, "head_branch": "feat/x"}
        self.daemon.tick(NOW + gg.NUDGE_MIN + 2 * gg.TICK)
        self.assertEqual(len(self.gh.gql_calls()), 2)
        self.daemon.tick(NOW + 2 * gg.NUDGE_MIN + gg.TICK)
        self.assertEqual(len(self.gh.gql_calls()), 3)
        self.ctl.suites.append({"id": 2, "repo": "o/other", "head_sha": "a" * 40, "status": "completed"})
        self.daemon.tick(NOW + 2 * gg.NUDGE_MIN + 3 * gg.TICK)  # still inside the 60 s cycle
        self.assertEqual(len(self.gh.gql_calls()), 3)  # another repository's commit

    def test_comments_from_the_feed_reach_a_comment_wait(self) -> None:
        self.gh.issues[("t", "s", 454)] = {"__typename": "Issue", "url": "u", "state": "OPEN",
                                           "comments": {"totalCount": 0, "nodes": []}}
        gg.register("comment:t/s#454")
        self.daemon.tick(NOW)
        self.ctl.comments.append({"id": 77, "repo": "t/s", "number": 454, "author": "github-actions[bot]",
                                  "body": "callsign-receipt/v0\nstatus: accepted", "url": "u77",
                                  "created_at": iso(NOW + 1)})
        self.ctl.comments.append({"id": 78, "repo": "t/other", "number": 454, "author": "x", "body": "y"})
        self.daemon.tick(NOW + gg.TICK)  # before the next GraphQL read
        self.assertEqual(len(self.gh.gql_calls()), 1)
        comments = gg.read_cache("comment:t/s#454")["data"]["comments"]
        self.assertEqual([c["id"] for c in comments], [77])
        self.assertTrue(gg.comment_matches(comments[0], "github-actions", re.compile("accepted"), None, NOW))
        self.ctl.comments.append({"id": 77, "repo": "t/s", "number": 454, "deleted": True})
        self.daemon.tick(NOW + 2 * gg.TICK)
        self.assertEqual(gg.read_cache("comment:t/s#454")["data"]["comments"], [])

    def test_budget_names_the_controller_state(self) -> None:
        self.assertIn("webhook events ok, 2 watched runs", gg.controller_line(
            {"state": "ok", "runs": 2, "lastDelivery": iso(time.time() - 5), "token": True}))
        self.assertIn("public repos only", gg.controller_line({"state": "ok", "token": False}))
        self.assertIn("unreachable (controller: URLError); polling GitHub", gg.controller_line(
            {"state": "down", "error": "controller: URLError", "retryAt": time.time() + 60}))
        self.assertIn("no run-events receiver", gg.controller_line({"state": "absent", "retryAt": 0}))
        self.assertIn("none configured", gg.controller_line({"state": "off"}))

    def test_controller_works_without_a_token(self) -> None:
        saved = {k: os.environ.get(k) for k in ("CMUX_CI_TOKEN_FILE", "CMUX_CI_CONTROLLER")}
        os.environ["CMUX_CI_TOKEN_FILE"] = "/nonexistent/token"
        os.environ["CMUX_CI_CONTROLLER"] = "http://controller.test:18765/"
        try:
            ctl = gg.Controller.from_env()
            self.assertEqual((ctl.base, ctl.token), ("http://controller.test:18765", None))  # public repos only
            os.environ["CMUX_CI_CONTROLLER"] = "ftp://nope"
            self.assertIsNone(gg.Controller.from_env())
        finally:
            for k, v in saved.items():
                os.environ.pop(k, None) if v is None else os.environ.__setitem__(k, v)


class TransportErrorTest(unittest.TestCase):
    """glaeda-gh's http() helper shadows the `http` package. An except clause naming
    http.client raised AttributeError on the first connection failure and aborted every tick."""

    @staticmethod
    def closed_port() -> int:
        with socket.socket() as server:
            server.bind(("127.0.0.1", 0))
            return server.getsockname()[1]

    def test_unreachable_controller_is_a_watch_error(self) -> None:
        ctl = gg.Controller(f"http://127.0.0.1:{self.closed_port()}", None)
        with self.assertRaises(gg.WatchError):
            ctl.events(None)

    def test_unreachable_github_is_a_watch_error(self) -> None:
        api = f"http://127.0.0.1:{self.closed_port()}"
        with mock.patch.object(gg, "API", api), self.assertRaises(gg.WatchError):
            gg.http("GET", api + "/rate_limit", {})


class DaemonTest(Base):
    def test_run_etag_304_reuses_cached_body(self) -> None:
        self.gh.runs[7] = {"id": 7, "name": "CI", "status": "in_progress", "conclusion": None, "html_url": "u"}
        key = gg.parse_key("run", "O/R/7")
        gg.register(key)
        self.daemon.tick(NOW)
        first = gg.read_cache(key)
        self.assertEqual(first["data"]["status"], "in_progress")
        self.assertTrue(first["etag"])
        self.daemon.tick(NOW + gg.CYCLE)
        calls = self.gh.rest_calls()
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1][2]["If-None-Match"], first["etag"])
        second = gg.read_cache(key)
        self.assertEqual(second["data"], first["data"])
        self.assertEqual(second["etag"], first["etag"])
        self.assertEqual(self.gh.rest_remaining, 3999)  # the 304 cost nothing
        self.assertEqual(self.daemon.budget["rest"]["remaining"], 3999)
        # a change comes through as fresh data
        self.gh.runs[7] = {**self.gh.runs[7], "status": "completed", "conclusion": "success"}
        self.daemon.tick(NOW + 2 * gg.CYCLE)
        self.assertEqual(gg.read_cache(key)["data"]["conclusion"], "success")

    def test_jobs_only_when_asked(self) -> None:
        self.gh.runs[8] = {"id": 8, "status": "in_progress"}
        self.gh.jobs[8] = [{"id": 1, "name": "build", "status": "completed", "conclusion": "failure", "html_url": "j"}]
        key = gg.parse_key("run", "https://github.com/o/r/actions/runs/8/job/99")
        gg.register(key)
        self.daemon.tick(NOW)
        self.assertFalse(any("/jobs" in c[1] for c in self.gh.calls))
        gg.register(key, jobs=True)
        gg.register(key)  # another reader without --jobs does not turn it off
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(gg.read_cache(key)["jobs"][0]["name"], "build")

    def test_batched_graphql_query_and_parse(self) -> None:
        keys = [gg.parse_key("pr", "o/r#1"), gg.parse_key("pr", "https://github.com/O/R/pull/2"), gg.parse_key("pr", "o/r#3")]
        query, variables = gg.pr_query(keys)
        self.assertIn("p0: repository(owner: $o0, name: $r0)", query)
        self.assertIn("p2: repository(owner: $o2", query)
        self.assertIn("rateLimit", query)
        self.assertEqual(variables, {"o0": "o", "r0": "r", "n0": 1, "o1": "o", "r1": "r", "n1": 2,
                                     "o2": "o", "r2": "r", "n2": 3})
        status_ctx = {"__typename": "StatusContext", "context": "legacy", "state": "ERROR", "targetUrl": "s"}
        self.gh.prs[("o", "r", 1)] = pr_node(1, rollup="FAILURE", checks=[check_run("a"), check_run("b", conclusion="FAILURE"), status_ctx])
        self.gh.prs[("o", "r", 2)] = pr_node(2, state="MERGED", rollup="SUCCESS", checks=[check_run("a")])
        for key in keys:
            gg.register(key)
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.gql_calls()), 1)  # one query for all three
        one = gg.read_cache(keys[0])["data"]
        self.assertEqual([c["name"] for c in gg.failing(one["checks"])], ["b", "legacy"])
        query_vars = json.loads(self.gh.gql_calls()[0][3])["query"]
        self.assertIn("isRequired(pullRequestNumber: $n2)", query_vars)
        self.assertEqual(one["checksTotal"], 3)
        self.assertEqual(gg.read_cache(keys[1])["data"]["state"], "MERGED")
        missing = gg.read_cache(keys[2])
        self.assertIsNone(missing["data"])
        self.assertIn("Could not resolve", missing["error"])
        self.assertTrue(missing["missing"])
        self.assertEqual(self.daemon.budget["graphql"]["remaining"], 3999)

    def test_new_entry_fills_at_once_then_waits_for_the_cycle(self) -> None:
        self.gh.prs[("o", "r", 1)] = pr_node(1)
        self.gh.prs[("o", "r", 2)] = pr_node(2)
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        gg.register("pr:o/r#2")
        self.daemon.tick(NOW + gg.TICK)  # only the new PR is fetched
        self.assertEqual(len(self.gh.gql_calls()), 2)
        self.assertEqual(json.loads(self.gh.gql_calls()[1][3])["variables"], {"o0": "o", "r0": "r", "n0": 2})
        self.daemon.tick(NOW + 2 * gg.TICK)
        self.assertEqual(len(self.gh.gql_calls()), 2)
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(len(self.gh.gql_calls()), 3)

    def test_interest_expiry(self) -> None:
        self.gh.prs[("o", "r", 1)] = pr_node(1)
        self.gh.prs[("o", "r", 2)] = pr_node(2, state="MERGED", rollup="SUCCESS")
        self.gh.prs[("o", "r", 3)] = pr_node(3)
        for n in (1, 2, 3):
            gg.register(f"pr:o/r#{n}")
        self.daemon.tick(NOW)
        self.age_watch("pr:o/r#1", gg.IDLE_EXPIRY + 1)  # nobody read it for 15 minutes
        self.age_watch("pr:o/r#2", gg.TERMINAL_GRACE + 1)  # merged, last reader gone
        self.age_watch("pr:o/r#3", gg.TERMINAL_GRACE + 1)  # open: still within the idle window
        status = self.daemon.tick(NOW + 1)
        self.assertEqual(status["watching"], 1)
        left = [gg.read_json(p)["key"] for p in gg.watch_dir().glob("*.json")]
        self.assertEqual(left, ["pr:o/r#3"])

    def test_terminal_entries_are_not_repolled_every_cycle(self) -> None:
        self.gh.prs[("o", "r", 2)] = pr_node(2, state="MERGED", rollup="SUCCESS")
        gg.register("pr:o/r#2")
        self.daemon.tick(NOW)
        for i in range(1, 5):
            gg.register("pr:o/r#2")
            self.age_watch("pr:o/r#2", 1)  # the reader was already there before the last fetch
            self.daemon.tick(NOW + i * gg.CYCLE)
        self.assertEqual(len(self.gh.gql_calls()), 1)

    def test_rerun_of_a_completed_run_is_seen_on_the_next_cycle(self) -> None:
        self.gh.runs[3] = {"id": 3, "status": "completed", "conclusion": "failure", "run_attempt": 1}
        self.gh.jobs[3] = [job("build", run_attempt=1)]
        gg.register("run:o/r/3")
        self.age_watch("run:o/r/3", 1)  # registered before the first fetch
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.CYCLE)  # nobody new is reading: a completed run is not re-read
        self.assertEqual(len(self.gh.run_calls()), 1)
        self.assertEqual(len(self.gh.job_calls()), 1)  # the failed attempt's steps, read once
        self.gh.runs[3] = {"id": 3, "status": "queued", "conclusion": None, "run_attempt": 2}  # gh run rerun 3
        path = gg.watch_dir() / gg.file_name("run:o/r/3")
        gg.register("run:o/r/3")  # a new wait
        os.utime(path, (NOW + gg.CYCLE + 1,) * 2)
        self.daemon.tick(NOW + 2 * gg.CYCLE)
        self.assertEqual(len(self.gh.run_calls()), 2)
        self.assertEqual(gg.read_cache("run:o/r/3")["data"]["status"], "queued")

    def test_budget_floor_goes_lean_then_stops(self) -> None:
        self.gh.runs[9] = {"id": 9, "status": "in_progress"}
        key = "run:o/r/9"
        gg.register(key, jobs=True)
        self.gh.rest_remaining = gg.FLOORS["rest"][0]  # the run fetch leaves it just under the floor
        self.daemon.tick(NOW)  # first fill still happens, but without the job list
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.assertEqual(self.daemon.mode("rest"), "lean")
        self.assertNotIn("jobs", gg.read_cache(key))
        self.gh.runs[9] = {"id": 9, "status": "queued"}
        self.daemon.tick(NOW + gg.CYCLE)  # lean: one normal cycle is not enough
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.daemon.tick(NOW + gg.CYCLE * gg.LEAN_FACTOR)
        self.assertEqual(len(self.gh.rest_calls()), 2)
        self.daemon.budget["rest"]["remaining"] = gg.FLOORS["rest"][1] - 1  # below the reserve
        gg.register("run:o/r/10")
        self.daemon.tick(NOW + 8 * gg.CYCLE)  # before the reset
        self.assertEqual(len(self.gh.rest_calls()), 2)
        self.assertIsNone(gg.read_cache("run:o/r/10"))
        self.assertEqual(self.daemon.mode("rest"), "stop")
        self.daemon.budget["rest"]["reset"] = NOW  # the quota reset: back to normal
        self.daemon.now = NOW + 1
        self.assertEqual(self.daemon.mode("rest"), "normal")

    def test_graphql_reserve_and_rate_limit_backoff(self) -> None:
        self.daemon.budget["graphql"] = {"limit": 5000, "remaining": 10, "reset": NOW + 600}
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        self.assertEqual(self.gh.calls, [])

        def limited(method, url, headers, body):
            return 403, {"retry-after": "120", "x-ratelimit-remaining": "0", "x-ratelimit-resource": "core",
                         "x-ratelimit-limit": "5000", "x-ratelimit-reset": str(int(NOW + 900))}, b'{"message":"rate limited"}'

        self.daemon.transport = limited
        gg.register("run:o/r/5")
        self.daemon.tick(NOW)
        self.assertEqual(self.daemon.mode("rest"), "stop")
        self.assertIn("HTTP 403", gg.read_cache("run:o/r/5")["error"])

    def test_token_never_written(self) -> None:
        self.gh.runs[7] = {"id": 7, "status": "queued"}
        self.gh.prs[("o", "r", 1)] = pr_node(1)
        gg.register("run:o/r/7")
        gg.register("pr:o/r#1")
        gg.register("pr:o/r#404")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.daemon.tick(NOW)
        for path in gg.BASE.rglob("*"):
            if path.is_file():
                self.assertNotIn(TOKEN, path.read_text(), path)
        self.assertNotIn(TOKEN, out.getvalue())

    def test_token_failure_is_reported_without_output(self) -> None:
        self.daemon.token_source = lambda: (_ for _ in ()).throw(gg.WatchError("gh auth token exited 1; is gh signed in?"))
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        self.assertIn("gh signed in", gg.read_cache("pr:o/r#1")["error"])

    def test_failures_are_retried_once_per_cycle(self) -> None:
        def broken(method, url, headers, body):
            raise gg.WatchError("request failed: RemoteDisconnected")

        self.daemon.transport = broken
        gg.register("pr:o/r#1")
        gg.register("run:o/r/2")
        attempts = []
        self.daemon.transport = lambda *a: (attempts.append(a[1]), broken(*a))[1]
        for i in range(20):  # 40 s of ticks
            self.daemon.tick(NOW + i * gg.TICK)
        self.assertEqual(len(attempts), 2)
        self.assertIn("RemoteDisconnected", gg.read_cache("run:o/r/2")["error"])

        def explodes(method, url, headers, body):
            raise KeyError("boom")  # an unexpected bug still costs one attempt per cycle

        self.daemon.transport = lambda *a: (attempts.append(a[1]), explodes(*a))[1]
        for i in range(20):
            self.daemon.tick(NOW + gg.CYCLE + i * gg.TICK)
        self.assertEqual(len(attempts), 4)

    def test_redirects_and_foreign_hosts_are_refused(self) -> None:
        with self.assertRaises(gg.WatchError):
            gg.http("GET", "https://example.com/x", {"Authorization": "Bearer x"})
        handler = gg.NoRedirect()
        self.assertIsNone(handler.redirect_request(None, None, 301, "Moved", {}, "https://evil.example/"))

    def test_token_format_is_checked(self) -> None:
        self.assertTrue(gg.TOKEN_RE.fullmatch("gho_" + "a" * 36))
        self.assertTrue(gg.TOKEN_RE.fullmatch("github_pat_" + "A1_" * 20))
        self.assertFalse(gg.TOKEN_RE.fullmatch("To get started with GitHub CLI, please run: gh auth login"))

    def test_bad_keys_are_refused(self) -> None:
        for kind, text in (("pr", "o/r"), ("pr", "o/r#x"), ("run", "o/r"), ("pr", "o/r#1) { x }")):
            with self.assertRaises(gg.WatchError):
                gg.parse_key(kind, text)
        self.assertEqual(gg.parse_key("pr", "Manaflow-AI/cmux#14542"), "pr:manaflow-ai/cmux#14542")
        self.assertEqual(gg.parse_key("run", "o/r/actions/runs/12/attempts/2"), "run:o/r/12")
        self.assertEqual(gg.parse_key("comment", "https://github.com/T/S/issues/454#issuecomment-5"), "comment:t/s#454")
        self.assertEqual(gg.parse_key("comment", "t/s#454"), "comment:t/s#454")
        with self.assertRaises(gg.WatchError):
            gg.parse_key("pr", "t/s/issues/454")  # an issue is not a pull request

    def test_failed_run_reads_its_jobs_once_per_attempt(self) -> None:
        key = "run:o/r/4"
        self.gh.runs[4] = {"id": 4, "status": "completed", "conclusion": "failure", "run_attempt": 1,
                           "updated_at": iso(NOW)}
        self.gh.jobs[4] = [job("build", end=NOW), job("lint", "success", end=NOW)]
        gg.register(key)  # no --jobs
        self.daemon.tick(NOW)
        entry = gg.read_cache(key)
        self.assertEqual([j["failedSteps"] for j in entry["jobs"]], [["Run selected tests"], []])
        self.assertIn("failed: build (step: Run selected tests)", gg.summary(key, entry))
        path = gg.watch_dir() / gg.file_name(key)
        os.utime(path, (NOW + gg.CYCLE - 1,) * 2)  # a waiter is still reading
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual((len(self.gh.run_calls()), len(self.gh.job_calls())), (2, 1))  # the run answered 304
        self.assertEqual(self.gh.rest_remaining, 3998)
        # attempt 2 fails too: its own job list is read
        self.gh.runs[4] = {**self.gh.runs[4], "run_attempt": 2}
        self.gh.jobs[4] = [job("build", run_attempt=2, end=NOW), job("lint", "success", end=NOW)]
        os.utime(path, (NOW + 2 * gg.CYCLE - 1,) * 2)
        self.daemon.tick(NOW + 2 * gg.CYCLE)
        self.assertEqual(len(self.gh.job_calls()), 2)
        self.assertEqual(gg.jobs_attempt(gg.read_cache(key)["jobs"]), 2)

    def test_failed_check_steps_are_read_once(self) -> None:
        failed = {**check_run("build", conclusion="FAILURE"), "id": "CR_1"}
        self.gh.prs[("o", "r", 1)] = pr_node(1, rollup="FAILURE", checks=[failed, {**check_run("lint"), "id": "CR_2"}])
        self.gh.steps["CR_1"] = [{"name": "Set up job", "conclusion": "SUCCESS"},
                                 {"name": "Run selected tests", "conclusion": "FAILURE"},
                                 {"name": "Upload logs", "conclusion": "CANCELLED"}]
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        self.assertEqual(self.gh.step_queries, [["CR_1"]])  # only the failed check
        data = gg.read_cache("pr:o/r#1")["data"]
        self.assertEqual(data["checks"][0]["failedSteps"], ["Run selected tests"])
        self.assertIn("failed: build (step: Run selected tests)", gg.summary("pr:o/r#1", {"data": data}))
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(len(self.gh.gql_calls()), 3)  # PR, steps, PR: the steps were kept
        self.assertEqual(gg.read_cache("pr:o/r#1")["data"]["checks"][0]["failedSteps"], ["Run selected tests"])

    def test_comment_watch_polls_fast_and_ends_with_its_waiter(self) -> None:
        self.gh.issues[("t", "s", 454)] = {"__typename": "Issue", "url": "https://github.com/t/s/issues/454",
                                           "state": "OPEN", "comments": {"totalCount": 1, "nodes": [
                                               {"databaseId": 9, "url": "u9", "createdAt": iso(NOW),
                                                "author": {"login": "github-actions"}, "body": "x" * 9000}]}}
        gg.register("comment:t/s#454")
        gg.register("comment:t/s#455")
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.gql_calls()), 1)  # both issues in one query
        data = gg.read_cache("comment:t/s#454")["data"]
        self.assertEqual(data["comments"][0]["author"], "github-actions")
        self.assertEqual(len(data["comments"][0]["body"]), gg.COMMENT_BODY_MAX)
        self.assertTrue(gg.read_cache("comment:t/s#455")["missing"])
        self.daemon.tick(NOW + gg.TICK)
        self.assertEqual(len(self.gh.gql_calls()), 1)
        self.daemon.tick(NOW + gg.COMMENT_CYCLE)
        self.assertEqual(len(self.gh.gql_calls()), 2)
        self.age_watch("comment:t/s#454", gg.TERMINAL_GRACE + 1)  # the wait ended
        self.age_watch("comment:t/s#455", 1)
        self.assertEqual(self.daemon.tick(NOW + gg.COMMENT_CYCLE + 1)["watching"], 1)

    def test_an_entry_filled_between_cycles_is_refreshed_on_the_next_one(self) -> None:
        self.gh.prs[("o", "r", 1)] = pr_node(1)
        self.daemon.tick(NOW)  # a cycle with nothing watched still starts the cycle clock
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW + 40)  # filled at once
        self.daemon.tick(NOW + gg.CYCLE)  # the next cycle, 20 s later, refreshes it
        self.assertEqual(len(self.gh.gql_calls()), 2)
        self.assertEqual(gg.read_cache("pr:o/r#1")["fetchedAt"], NOW + gg.CYCLE)

    def test_heartbeat_and_request_counts(self) -> None:
        self.daemon.tick(NOW)  # nothing watched: the heartbeat is still written
        self.assertEqual(gg.read_json(gg.BASE / "daemon.json")["at"], NOW)
        self.daemon.tick(NOW + gg.TICK)
        self.assertEqual(gg.read_json(gg.BASE / "daemon.json")["at"], NOW)  # not every tick
        self.daemon.tick(NOW + gg.HEARTBEAT_EVERY)
        self.assertEqual(gg.read_json(gg.BASE / "daemon.json")["at"], NOW + gg.HEARTBEAT_EVERY)
        self.gh.runs[7] = {"id": 7, "status": "in_progress"}
        gg.register("run:o/r/7")
        self.daemon.tick(NOW + 100)
        self.daemon.tick(NOW + 100 + gg.CYCLE)
        hour = gg.read_json(gg.BASE / "daemon.json")["lastHour"]
        self.assertEqual(hour, {"rest": 1, "rest304": 1, "graphql": 0})
        self.gh.runs[7] = {"id": 7, "status": "queued"}
        self.daemon.tick(NOW + 100 + 3700)
        self.assertEqual(gg.read_json(gg.BASE / "daemon.json")["lastHour"]["rest304"], 0)  # an hour later


class ClientTest(Base):
    def setUp(self) -> None:
        super().setUp()
        gg.BASE.mkdir(parents=True)
        self.lock = open(gg.BASE / "daemon.lock", "a+")
        fcntl.flock(self.lock, fcntl.LOCK_EX)  # stands in for a running daemon

    def tearDown(self) -> None:
        self.lock.close()
        super().tearDown()

    def put(self, key: str, data: dict | None, at: float | None = None, **extra) -> None:
        at = time.time() if at is None else at
        gg.write_json(gg.cache_dir() / gg.file_name(key), {"key": key, "fetchedAt": at,
                                                          "dataAt": at if data else None, "data": data, "error": None, **extra})

    def wait(self, kind: str, target: str, until: str = "green", timeout: float = 0.5, fill: tuple | None = None,
             sha: str | None = None, grace: float = gg.RESCUE_GRACE) -> tuple[int, str]:
        """Wait while a stand-in daemon keeps writing `fill` (data, extra), as the real one refreshes.

        One write at a fixed delay raced the wait's own start: resolving a short --sha runs git,
        which took 0.15 s on a loaded host, so the write landed before the wait began and was
        (rightly) ignored as older data."""
        key = gg.parse_key(kind, target)
        done = threading.Event()

        def daemon() -> None:
            while not done.wait(0.05):
                self.put(key, fill[0], **fill[1])
        writer = threading.Thread(target=daemon) if fill else None
        if writer:
            writer.start()
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
                code = gg.cmd_wait(kind, target, until, timeout, False, False, sha, grace)
        finally:
            done.set()
            if writer:
                writer.join()
        return code, out.getvalue()

    def test_wait_exit_codes(self) -> None:
        green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))
        self.assertEqual(self.wait("pr", "o/r#1", fill=(green, {}))[0], 0)

        red = gg.parse_pr(pr_node(2, rollup="PENDING", checks=[check_run("a", conclusion="FAILURE"),
                                                                check_run("b", "IN_PROGRESS", None)]))
        code, text = self.wait("pr", "o/r#2", fill=(red, {}))
        self.assertEqual(code, 1)  # green fails fast
        self.assertIn("failed: a  https://ci/a", text)
        self.assertEqual(self.wait("pr", "o/r#2", until="done", fill=(red, {}))[0], 2)  # done waits for b

        closed = gg.parse_pr(pr_node(3, state="CLOSED", rollup="SUCCESS"))
        self.assertEqual(self.wait("pr", "o/r#3", until="merged", fill=(closed, {}))[0], 1)
        merged = gg.parse_pr(pr_node(4, state="MERGED", rollup=None))
        self.assertEqual(self.wait("pr", "o/r#4", until="merged", fill=(merged, {}))[0], 0)

        self.assertEqual(self.wait("run", "o/r/5", fill=({"status": "completed", "conclusion": "failure"}, {}))[0], 1)
        self.assertEqual(self.wait("run", "o/r/6", fill=({"status": "completed", "conclusion": "success"}, {}))[0], 0)
        code, text = self.wait("run", "o/r/7", timeout=0.1)  # nothing cached yet
        self.assertEqual(code, 2)
        self.assertIn("result: timeout", text)
        # waiting registered interest for the daemon to pick up
        self.assertIsNotNone(gg.read_json(gg.watch_dir() / gg.file_name("run:o/r/7")))

    def test_conflicting_pr_stops_green_and_done_but_not_merged(self) -> None:
        pending = [check_run("a"), check_run("b", "IN_PROGRESS", None)]
        node = {**pr_node(1, rollup="PENDING", checks=pending), "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}
        data = gg.parse_pr(node)
        for until in ("green", "done"):
            code, text = self.wait("pr", "o/r#1", until=until, fill=(data, {}))
            self.assertEqual(code, gg.EXIT_CONFLICT)
            self.assertEqual(code, 4)
            self.assertIn("result: conflict", text)
            self.assertIn("note: PR conflicts with its base", text)
        self.assertEqual(self.wait("pr", "o/r#1", until="merged", fill=(data, {}))[0], 2)  # keeps waiting
        # A conflict decides before any check: green checks do not hide it either.
        green = {**pr_node(2, rollup="SUCCESS", checks=[check_run("a")]), "mergeable": "CONFLICTING",
                 "mergeStateStatus": "DIRTY"}
        self.assertEqual(gg.verdict("pr", gg.parse_pr(green), "green"), gg.EXIT_CONFLICT)
        # GitHub computes mergeability lazily: UNKNOWN is pending, not a conflict.
        unknown = gg.parse_pr({**node, "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN"})
        self.assertIsNone(gg.verdict("pr", unknown, "done"))
        self.assertEqual(self.wait("pr", "o/r#3", until="done", fill=(unknown, {}))[0], 2)
        # With --sha, an older head's conflict is pending: the push that resolves it has not landed yet.
        self.assertIsNone(gg.verdict("pr", data, "green", sha="b" * 40))
        self.assertEqual(gg.verdict("pr", data, "green", sha="a" * 40), gg.EXIT_CONFLICT)
        # A merged PR is never a conflict.
        merged = gg.parse_pr({**node, "state": "MERGED"})
        self.assertEqual(gg.verdict("pr", merged, "merged"), 0)

    def test_closed_unmerged_pr_fails_and_says_so(self) -> None:
        closed = gg.parse_pr(pr_node(1, state="CLOSED", rollup="PENDING", checks=[check_run("a", "IN_PROGRESS", None)]))
        for until in ("green", "done", "merged"):
            code, text = self.wait("pr", "o/r#1", until=until, fill=(closed, {}))
            self.assertEqual(code, 1)
            self.assertIn("note: PR was closed without being merged", text)

    def test_wait_ignores_cache_from_before_it_began(self) -> None:
        old_green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))
        self.put("pr:o/r#1", old_green, at=time.time() - 30)
        self.assertEqual(self.wait("pr", "o/r#1", timeout=0.3)[0], 2)

    def test_wait_sha_mismatch_is_pending(self) -> None:
        green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))  # head is aaaa...
        code, text = self.wait("pr", "o/r#1", fill=(green, {}), sha="b" * 40, timeout=0.4)
        self.assertEqual(code, 2)
        self.assertIn("GitHub still shows head aaaaaaaaaa", text)
        self.assertEqual(self.wait("pr", "o/r#1", fill=(green, {}), sha="aaaaaaa")[0], 0)

    def test_missing_pr_fails(self) -> None:
        code, text = self.wait("pr", "o/r#404", fill=(None, {"error": "Could not resolve", "missing": True}))
        self.assertEqual(code, 1)
        self.assertIn("Could not resolve", text)

    def test_rerun_after_cancel_counts_the_latest_attempt(self) -> None:
        old = {**check_run("Web complexity", conclusion="CANCELLED"), "databaseId": 1, "startedAt": "2026-09-25T10:00:00Z"}
        new = {**check_run("Web complexity"), "databaseId": 2, "startedAt": "2026-09-25T11:00:00Z"}
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[new, old]))
        self.assertEqual([c["conclusion"] for c in data["checks"]], ["SUCCESS"])
        self.assertEqual(gg.verdict("pr", data), 0)

    def test_queued_rerun_outranks_the_cancelled_attempt(self) -> None:
        old = {**check_run("e2e", conclusion="CANCELLED"), "databaseId": 1, "startedAt": "2026-09-25T10:00:00Z"}
        queued = {**check_run("e2e", "QUEUED", None), "databaseId": 2, "startedAt": None}
        data = gg.parse_pr(pr_node(1, rollup="PENDING", checks=[old, queued]))
        self.assertEqual([c["status"] for c in data["checks"]], ["QUEUED"])
        self.assertIsNone(gg.verdict("pr", data))

    def test_a_required_check_that_has_not_reported_is_pending(self) -> None:
        node = pr_node(1, rollup="SUCCESS", checks=[{**check_run("Web complexity"), "isRequired": True},
                                                     {**check_run("lint"), "isRequired": False}])
        node["baseRef"] = {"branchProtectionRule": None, "rules": {"nodes": [
            {"type": "REQUIRED_STATUS_CHECKS", "parameters": {"requiredStatusChecks": [
                {"context": "Web complexity"}, {"context": "ci-status"}]}},
            {"type": "DELETION", "parameters": None}]}}
        data = gg.parse_pr(node)
        self.assertEqual(data["requiredMissing"], ["ci-status"])
        self.assertIsNone(gg.verdict("pr", data, "green"))
        self.assertIsNone(gg.verdict("pr", data, "done"))
        self.assertIn("required, not reported yet: ci-status", gg.summary("pr:o/r#1", {"data": data}))
        # GitHub's merge state already counts it: CLEAN means nothing required is outstanding.
        self.assertEqual(gg.verdict("pr", {**data, "mergeStateStatus": "CLEAN"}, "green"), 0)
        # A conflicted PR runs no workflows until the next push, and a merged one never will: no wait.
        self.assertEqual(gg.check_verdict({**data, "mergeStateStatus": "DIRTY"}, "done"), 1)
        self.assertEqual(gg.verdict("pr", {**data, "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}, "done"),
                         gg.EXIT_CONFLICT)
        # A lagging DIRTY without CONFLICTING is not called a conflict.
        self.assertNotEqual(gg.verdict("pr", {**data, "mergeable": "MERGEABLE", "mergeStateStatus": "DIRTY"}, "done"),
                            gg.EXIT_CONFLICT)
        self.assertIn("merge conflicts", gg.summary("pr:o/r#1", {"data": {**data, "mergeStateStatus": "DIRTY"}}))
        self.assertEqual(gg.verdict("pr", {**data, "state": "MERGED", "mergeStateStatus": "UNKNOWN"}, "green"), 0)
        # Classic branch protection lists its contexts too.
        node["baseRef"] = {"branchProtectionRule": {"requiredStatusCheckContexts": ["build"]}, "rules": None}
        self.assertEqual(gg.parse_pr(node)["requiredMissing"], ["build"])

    def test_a_run_cancelled_for_a_newer_run_of_its_workflow_is_dropped(self) -> None:
        def in_run(check: dict, run: int, conclusion: str | None) -> dict:
            return {**check, "databaseId": run * 10, "isRequired": True, "checkSuite": {
                "conclusion": conclusion, "workflowRun": {"databaseId": run, "workflow": {"name": "CI"}}}}
        old = [in_run(check_run("ci-status", conclusion="FAILURE"), 1, "CANCELLED"),
               in_run(check_run("changes", conclusion="CANCELLED"), 1, "CANCELLED")]
        new = [in_run(check_run("changes"), 2, None), in_run(check_run("macos", "QUEUED", None), 2, None)]
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=old + new))
        self.assertEqual(sorted(c["name"] for c in data["checks"]), ["changes", "macos"])
        self.assertIsNone(gg.verdict("pr", data, "green"))
        # With no newer run, a cancelled run's failure still counts.
        alone = gg.parse_pr(pr_node(2, rollup="FAILURE", checks=old))
        self.assertEqual(gg.verdict("pr", alone, "green"), 1)

    def test_same_name_in_two_workflows_stays_separate(self) -> None:
        def in_workflow(check: dict, workflow: str, db: int) -> dict:
            return {**check, "databaseId": db, "checkSuite": {"workflowRun": {"workflow": {"name": workflow}}}}
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[
            in_workflow(check_run("build", conclusion="FAILURE"), "CI", 1),
            in_workflow(check_run("build"), "Release", 2)]))
        self.assertEqual(len(data["checks"]), 2)
        self.assertEqual(gg.verdict("pr", data), 1)
        self.assertIn("failed: CI / build", gg.summary("pr:o/r#1", {"data": data}))

    def test_finished_things_answer_at_once_from_an_older_fetch(self) -> None:
        self.put("pr:o/r#1", gg.parse_pr(pr_node(1, state="MERGED", rollup="SUCCESS")), at=time.time() - 300)
        self.assertEqual(self.wait("pr", "o/r#1", until="merged", timeout=0.3)[0], 0)
        # a completed run may have been rerun since: only data fetched after the wait began decides
        self.put("run:o/r/2", {"status": "completed", "conclusion": "failure"}, at=time.time() - 300)
        self.assertEqual(self.wait("run", "o/r/2", timeout=0.3)[0], 2)
        rerun = ({"status": "queued", "conclusion": None}, {})
        self.assertEqual(self.wait("run", "o/r/2", timeout=0.4, fill=rerun)[0], 2)
        passed = ({"status": "completed", "conclusion": "success"}, {})
        self.assertEqual(self.wait("run", "o/r/2", timeout=0.5, fill=passed)[0], 0)
        self.put("pr:o/r#4", gg.parse_pr(pr_node(4, state="CLOSED", rollup="SUCCESS")), at=time.time() - 300)
        self.assertEqual(self.wait("pr", "o/r#4", until="merged", timeout=0.3)[0], 2)  # may be reopened
        self.put("run:o/r/3", {"status": "in_progress"}, at=time.time() - 300)
        self.assertEqual(self.wait("run", "o/r/3", timeout=0.3)[0], 2)  # live and stale: not trusted

    def test_only_not_found_fails_a_missing_pr(self) -> None:
        code, _ = self.wait("pr", "o/r#5", fill=(None, {"error": "graphql: HTTP 502", "missing": False}), timeout=0.4)
        self.assertEqual(code, 2)  # a transport error or 5xx stays pending
        code, _ = self.wait("pr", "o/r#6", fill=(None, {"error": "gh auth token exited 1; is gh signed in?"}), timeout=0.4)
        self.assertEqual(code, 2)

    def test_only_required_checks_decide_when_marked(self) -> None:
        req = {**check_run("build"), "isRequired": True}
        optional = {**check_run("lint", conclusion="FAILURE"), "isRequired": False}
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[req, optional]))
        self.assertEqual(gg.verdict("pr", data, "green"), 0)
        self.assertEqual(gg.verdict("pr", data, "done"), 0)
        self.assertIn("lint (not required)", gg.summary("pr:o/r#1", {"data": data}))
        many = {**data, "checksTruncated": True, "checkState": "PENDING"}
        self.assertIsNone(gg.verdict("pr", many))  # over 100 checks: the rollup decides
        self.assertIn("none reported yet", gg.summary("pr:o/r#2", {"data": gg.parse_pr(pr_node(2, rollup=None))}))

    def test_daemon_down_waits_out_a_reexec(self) -> None:
        self.lock.close()
        holder = open(gg.BASE / "daemon.lock", "a+")
        relock = threading.Timer(0.1, lambda: fcntl.flock(holder, fcntl.LOCK_EX))
        relock.start()
        self.assertTrue(gg.daemon_running(grace=0.5))
        relock.join()
        holder.close()

    def test_done_uses_every_check(self) -> None:
        both = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[check_run("a"), check_run("b", conclusion="CANCELLED")]))
        self.assertEqual(gg.verdict("pr", both, "done"), 1)
        ok = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a"), check_run("b", conclusion="SKIPPED")]))
        self.assertEqual(gg.verdict("pr", ok, "done"), 0)
        self.assertIsNone(gg.verdict("pr", gg.parse_pr(pr_node(1, rollup=None)), "done"))

    def test_status_prints_cached_entry(self) -> None:
        self.put("pr:o/r#1", gg.parse_pr(pr_node(1, rollup="PENDING", checks=[check_run("a")])))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(gg.cmd_status("pr", "o/r#1", False, True, first_wait=0), 0)
        self.assertEqual(json.loads(out.getvalue())["data"]["checkState"], "PENDING")

    def test_daemon_down(self) -> None:
        self.lock.close()
        err = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(gg.cmd_wait("pr", "o/r#1", "green", 5, False, False), 3)
            self.assertEqual(gg.cmd_status("pr", "o/r#1", False, False), 3)
            self.assertEqual(gg.cmd_budget(False), 3)
        self.assertIn("daemon is not running", err.getvalue())
        self.assertIn("Start it: ", err.getvalue())
        self.assertFalse(gg.watch_dir().exists())  # no interest registered, nothing polled

    def test_hung_daemon_is_reported(self) -> None:
        gg.write_json(gg.BASE / "daemon.json", {"at": time.time() - gg.STALE_HEARTBEAT - 60, "heartbeat": 30})
        code, text = self.wait("run", "o/r/1", timeout=0.1)
        self.assertEqual(code, 2)
        self.assertIn("looks hung", text)
        self.assertEqual(text.count("looks hung"), 1)  # said once, not every poll
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(gg.cmd_budget(False), 3)
        gg.write_json(gg.BASE / "daemon.json", {"at": time.time(), "heartbeat": 30})
        self.assertNotIn("looks hung", self.wait("run", "o/r/1", timeout=0.1)[1])
        # an older daemon, idle, wrote daemon.json only when it last fetched something
        gg.write_json(gg.BASE / "daemon.json", {"at": time.time() - 3 * gg.STALE_HEARTBEAT})
        self.assertNotIn("looks hung", self.wait("run", "o/r/1", timeout=0.1)[1])

    # -- a glaeda runner refusing a job at setup, then the rescue re-running it

    def run_doc(self, attempt: int, conclusion: str | None, finished: float | None = None) -> dict:
        finished = time.time() if finished is None else finished
        return {"status": "completed" if conclusion else "in_progress", "conclusion": conclusion,
                "run_attempt": attempt, "head_sha": "c" * 40, "updated_at": iso(finished), "html_url": "u"}

    def test_refused_attempt_is_held_then_the_rescue_attempt_decides(self) -> None:
        refusal = (self.run_doc(1, "failure"), cached(refused(), job("lint", "cancelled", seconds=5)))
        code, text = self.wait("run", "o/r/1", fill=refusal, timeout=0.4)
        self.assertEqual(code, 2)  # held, not failed
        self.assertIn("held: attempt 1 failed only because a glaeda runner refused build at setup", text)
        self.assertIn("failed: build (step: Set up runner) (refused by cmuxs-mac-mini-3-glaeda-1)", text)
        # the rescue's attempt 2 lands on Blacksmith and passes
        self.assertEqual(self.wait("run", "o/r/1", fill=(self.run_doc(2, "success"), {}))[0], 0)
        # no rescue came within the grace: the failure stands, and says why it waited
        late = (self.run_doc(1, "failure", time.time() - gg.RESCUE_GRACE - 1), cached(refused()))
        code, text = self.wait("run", "o/r/1", fill=late)
        self.assertEqual(code, 1)
        self.assertIn("no rescue attempt followed within 360s", text)
        # --rescue-grace 0 rules on each attempt
        self.assertEqual(self.wait("run", "o/r/1", fill=refusal, grace=0)[0], 1)

    def test_only_a_refusal_on_a_glaeda_runner_is_held(self) -> None:
        cases = {
            "a real failure": [job("build")],
            "a refusal and a real failure": [refused(), job("test")],
            "setup failed on another runner": [refused(runner="GitHub Actions 12", labels=("ubuntu-latest",))],
            "setup failed after running a while": [job("build", failed_step="Set up runner", seconds=600)],
            "an older attempt's jobs": [refused(run_attempt=1)],
        }
        for why, jobs in cases.items():
            attempt = 2 if why == "an older attempt's jobs" else 1
            code, _ = self.wait("run", "o/r/1", fill=(self.run_doc(attempt, "failure"), cached(*jobs)))
            self.assertEqual(code, 1, why)
        self.assertEqual(self.wait("run", "o/r/1", fill=(self.run_doc(1, "failure"), {}))[0], 1)  # jobs unknown
        # a run the rescue cancelled after the refusal is held as well
        cancelled = (self.run_doc(1, "cancelled"), cached(refused(), job("test", "cancelled")))
        self.assertEqual(self.wait("run", "o/r/1", fill=cancelled, timeout=0.3)[0], 2)

    def test_refused_pr_check_holds_its_run(self) -> None:
        now = time.time()

        def in_run(check: dict, run: int, seconds: int, required: bool = True) -> dict:
            return {**check, "id": f"CR_{check['name']}", "databaseId": run * 10, "isRequired": required,
                    "startedAt": iso(now - seconds), "completedAt": iso(now),
                    "checkSuite": {"conclusion": "FAILURE", "workflowRun": {"databaseId": run, "workflow": {"name": "CI"}}}}
        build = in_run(check_run("build", conclusion="FAILURE"), 5, 7)
        status = in_run(check_run("ci-status", conclusion="FAILURE"), 5, 3)
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[build, status]))
        data["checks"][0]["failedSteps"], data["checks"][1]["failedSteps"] = ["Set up runner"], ["Check results"]
        self.assertIsNone(gg.verdict("pr", data, "green"))
        text = gg.summary("pr:o/r#1", {"data": data}, "green")
        self.assertIn("checks PENDING: 0 passed, 0 failed, 0 pending, 2 held", text)
        self.assertIn("held: CI / build (step: Set up runner)", text)
        self.assertEqual(gg.verdict("pr", data, "green", grace=0), 1)
        self.assertEqual(gg.verdict("pr", data, "green", now=now + gg.RESCUE_GRACE + 1), 1)
        # a real failure in the same run is not held
        data["checks"][0]["failedSteps"] = ["Run selected tests"]
        self.assertEqual(gg.verdict("pr", data, "green"), 1)

    def test_headline_agrees_with_the_verdict(self) -> None:
        """manaflow-ai/cmux#14667: GitHub's rollup was FAILURE for a Web complexity run cancelled
        between two passing ones, and the headline printed that next to result: success."""
        def web(run: int, conclusion: str) -> dict:
            return {**check_run("Web complexity", conclusion=conclusion), "databaseId": run, "isRequired": True,
                    "checkSuite": {"conclusion": conclusion, "workflowRun": {"databaseId": run, "workflow": {
                        "name": "Web complexity", "databaseId": 77}}}}
        lint = {**check_run("lint", conclusion="CANCELLED"), "isRequired": False}
        node = pr_node(1, rollup="FAILURE", checks=[web(1, "SUCCESS"), web(2, "CANCELLED"), web(3, "SUCCESS"),
                                                     {**check_run("build"), "isRequired": True}, lint])
        node["mergeStateStatus"] = "CLEAN"
        data = gg.parse_pr(node)
        self.assertEqual(gg.verdict("pr", data, "done"), 0)
        text = gg.summary("pr:o/r#1", {"data": data}, "done")
        self.assertIn("checks SUCCESS: 2 passed, 1 failed, 0 pending (of 3, 2 required); GitHub's rollup says FAILURE", text)
        self.assertIn("cancelled: lint (not required)", text)
        self.assertIn("superseded: Web complexity / Web complexity cancelled, replaced by a newer attempt or run", text)
        self.assertNotIn("checks FAILURE", text)
        # the headline follows the mode the wait used: green ignores optional checks still running
        running = gg.parse_pr(pr_node(2, rollup="PENDING", checks=[{**check_run("build"), "isRequired": True},
                                                                    {**check_run("slow", "IN_PROGRESS", None)}]))
        self.assertIn("checks SUCCESS", gg.summary("pr:o/r#2", {"data": running}, "green"))
        self.assertIn("checks PENDING", gg.summary("pr:o/r#2", {"data": running}, "done"))

    # -- comments

    def comments(self, *items: tuple[int, str, str, float]) -> dict:
        return {"url": "https://github.com/t/s/issues/454", "state": "OPEN", "type": "Issue", "total": len(items),
                "comments": [{"id": i, "author": a, "body": b, "createdAt": iso(t), "url": f"u{i}"}
                             for i, a, b, t in items]}

    def wait_comment(self, fill: dict | None = None, timeout: float = 0.5, **kw) -> tuple[int, str]:
        key = "comment:t/s#454"
        timer = threading.Timer(0.1, lambda: self.put(key, fill)) if fill else None
        if timer:
            timer.start()
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            code = gg.cmd_wait_comment("t/s#454", timeout, False, **kw)
        if timer:
            timer.join()
        return code, out.getvalue()

    def test_wait_comment(self) -> None:
        now = time.time()
        receipt = "callsign-receipt/v0\nstatus: accepted\nrequest-comment: https://github.com/t/s/issues/454#issuecomment-100"
        old = (100, "someone", "/callsign reserve Quill", now - 600)
        old_receipt = (101, "github-actions", receipt, now - 590)
        # by default only comments after the wait began count
        self.put("comment:t/s#454", self.comments(old, old_receipt))
        code, text = self.wait_comment(timeout=0.3, author="github-actions[bot]")
        self.assertEqual(code, 2)
        self.assertIn("0 comments since the wait began, none matched; to include a reply posted before", text)
        # --since the request comment's URL finds the receipt that came before the wait
        code, text = self.wait_comment(timeout=0.3, author="github-actions[bot]", match=r"issuecomment-100\b",
                                       since="https://github.com/t/s/issues/454#issuecomment-100")
        self.assertEqual(code, 0)
        self.assertIn("comment 101 by github-actions", text)
        self.assertIn("  status: accepted", text)
        self.assertEqual(self.wait_comment(timeout=0.3, since="10m", author="github-actions")[0], 0)
        self.assertEqual(self.wait_comment(timeout=0.3, since="101")[0], 2)
        self.assertEqual(self.wait_comment(timeout=0.3, since="5m", author="github-actions")[0], 2)
        # a reply that arrives while waiting
        reply = (102, "github-actions", receipt.replace("100", "102"), time.time() + 1)
        code, _ = self.wait_comment(self.comments(old, old_receipt, reply), author="github-actions[bot]",
                                    match="issuecomment-102")
        self.assertEqual(code, 0)
        self.assertEqual(self.wait_comment(self.comments(reply), timeout=0.3, author="someone")[0], 2)
        with self.assertRaises(gg.WatchError):
            self.wait_comment(match="(")
        with self.assertRaises(gg.WatchError):
            gg.parse_since("yesterday", now)
        self.assertEqual(gg.parse_since("2026-09-25T18:00:00Z", now), (None, gg.iso_epoch("2026-09-25T18:00:00Z")))
        self.assertEqual(gg.parse_since(None, now), (None, now))

    def test_wait_comment_on_a_missing_issue_fails(self) -> None:
        key = "comment:t/s#454"
        timer = threading.Timer(0.1, lambda: self.put(key, None, error="Could not resolve", missing=True))
        timer.start()
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(gg.cmd_wait_comment("t/s#454", 0.5, False), 1)
        timer.join()

    # -- --sha

    def test_sha_is_validated_and_resolved(self) -> None:
        green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))  # head is aaaa...
        for bad in ("xyz1234", "abc", "a" * 41):
            with self.assertRaises(gg.WatchError):
                gg.resolve_sha(bad)
        saved = gg.git_rev_parse
        try:
            gg.git_rev_parse = lambda prefix: None  # not a commit in this checkout
            with self.assertRaisesRegex(gg.WatchError, "not the head GitHub shows \\(aaaaaaaaaaaa\\)"):
                self.wait("pr", "o/r#1", fill=(green, {}), sha="abcdef1")
            self.assertEqual(self.wait("pr", "o/r#1", fill=(green, {}), sha="AAAAAAA")[0], 0)
            gg.git_rev_parse = lambda prefix: prefix + "0" * (40 - len(prefix))  # a local commit not pushed yet
            self.assertEqual(self.wait("pr", "o/r#1", fill=(green, {}), sha="bbbbbbb", timeout=0.3)[0], 2)
        finally:
            gg.git_rev_parse = saved
        self.assertEqual(self.wait("pr", "o/r#1", fill=(green, {}), sha="b" * 40, timeout=0.3)[0], 2)  # pending
        with self.assertRaisesRegex(gg.WatchError, "a run's commit never changes"):
            self.wait("run", "o/r/2", fill=(self.run_doc(1, "success"), {}), sha="d" * 40)
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(gg.main(["wait", "pr", "o/r#1", "--sha", "HEAD"]), 64)
            self.assertEqual(gg.main(["wait", "comment", "o/r#1", "--sha", "a" * 40]), 64)
            self.assertEqual(gg.main(["wait", "pr", "o/r#1", "--author", "x"]), 64)
            self.assertEqual(gg.main(["wait", "run", "o/r/1", "--rescue-grace", "-1"]), 64)
        self.assertIn("--sha must be a commit id", err.getvalue())


if __name__ == "__main__":
    unittest.main()
