#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-local-guard, on fake process tables."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import signal
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_local_guard", os.fspath(ROOT / "scripts" / "glaeda-local-guard"))
spec = importlib.util.spec_from_loader("glaeda_local_guard", loader)
lg = importlib.util.module_from_spec(spec)
sys.modules["glaeda_local_guard"] = lg
loader.exec_module(lg)

UID = 501
START = "Sun Sep 27 09:00:00 2026"
HOME = Path("/Users/leo")
PROJECT = "/Users/leo/Projects/cmux"
CLAUDE = "/Users/leo/Library/Application Support/Claude/claude-code/2.1.281/claude.app/Contents/MacOS/claude"
LAUNCHD = None


def proc(pid, ppid, command, path=None, elapsed=600, uid=UID, stat="S"):
    exe_path = path if path is not None else command.split(" ", 1)[0]
    return lg.Proc(pid, ppid, uid, elapsed, 1024, 0.0, stat, START, command, exe_path)


def launchd():
    return proc(1, 0, "/sbin/launchd")


def session(pid=10):
    """launchd -> Claude desktop -> claude CLI -> zsh tool shell (pid + 1)."""
    return [
        launchd(),
        proc(5, 1, "/Applications/Claude.app/Contents/MacOS/Claude"),
        proc(pid, 5, CLAUDE + " --session-id 0123abcd-0000-0000-0000-000000000000", path=CLAUDE),
        proc(pid + 1, pid, "/bin/zsh -c source snapshot && eval 'swift test'"),
    ]


def survey(procs, mapping=None):
    mapping = mapping or {}
    asked: list[int] = []

    def cwds(pids):
        asked.extend(pids)
        return {p: mapping[p] for p in pids if p in mapping}

    s = lg.survey(procs, UID, cwds, HOME)
    s["asked"] = asked
    return s


def reasons(s):
    return {e["pid"]: e["reason"] for e in s["kept"]}


class RuleTest(unittest.TestCase):
    def rule(self, command, path=None):
        return lg.rule(proc(2, 1, command, path=path))

    def test_runs_match(self) -> None:
        cases = {
            "/usr/bin/swift-build --package-path x": "swift-build",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/swift-test": "swift-test",
            "/usr/bin/swift test --filter Foo": "swift-test",
            "swift build -c release": "swift-build",
            "/usr/bin/xcodebuild -scheme cmux build": "xcodebuild",
            "xcodebuild -project a.xcodeproj build-for-testing": "xcodebuild",
            "/Users/leo/.cargo/bin/cargo +nightly build --release": "cargo-build",
            "cargo test -p foo": "cargo-test",
            "python3 tests/test_socket.py -k one": "tests",
            "/opt/homebrew/bin/python3 -u /Users/leo/Projects/cmux/tests/test_x.py": "tests",
            "bash ./tests/test_ci_self_hosted_guard.sh": "tests",
            "/bin/bash scripts/merge-main.sh": "merge-main",
            "python3 scripts/ci/merge_main.py --guards": "merge-main",
            "python3 scripts/verify-local.py": "verify-local",
        }
        for command, want in cases.items():
            self.assertEqual(self.rule(command), want, command)

    def test_framework_python_is_python(self) -> None:
        path = "/opt/homebrew/Frameworks/Python.framework/Versions/3.13/Resources/Python.app/Contents/MacOS/Python"
        self.assertEqual(self.rule(f"{path} tests/test_a.py", path=path), "tests")

    def test_look_alikes_do_not_match(self) -> None:
        for command in (
            "/bin/zsh -c swift test --filter X",  # a tool shell; its child is the run
            "bash -c bash tests/test_x.sh",
            "grep swift test file",
            "python3 -m py_compile tests/test_x.py",
            "python3 scripts/test-glaeda-procs.py",  # not in a tests directory
            "python3 tests/helper.py",
            "xcodebuild -showBuildSettings",
            "cargo fmt",
            "swift --version",
            "swift package resolve",
            "bash tests/lib.sh",
        ):
            self.assertIsNone(self.rule(command), command)


class SurveyTest(unittest.TestCase):
    def test_live_session_runs_are_never_touched(self) -> None:
        procs = session() + [
            proc(20, 11, "/usr/bin/xcodebuild -scheme cmux test", elapsed=7200),
            proc(21, 20, "/usr/bin/swift-frontend -c a.swift"),
            proc(22, 11, "/bin/bash scripts/merge-main.sh", elapsed=86400),
        ]
        s = survey(procs, {20: PROJECT, 22: PROJECT})
        self.assertEqual(s["orphans"], [])
        self.assertEqual(reasons(s), {20: "live-session", 22: "live-session"})
        self.assertEqual(s["asked"], [], "no cwd lookup for runs with a live session")

    def test_codex_counts_as_a_session(self) -> None:
        procs = [launchd(), proc(40, 1, "/Users/leo/.codex/bin/codex app-server"), proc(41, 40, "cargo build")]
        self.assertEqual(reasons(survey(procs, {41: PROJECT})), {41: "live-session"})

    def test_orphans_left_by_a_gone_session(self) -> None:
        procs = [launchd(),
                 proc(50, 1, "/bin/zsh -c eval 'bash scripts/merge-main.sh'"),  # the tool shell survived
                 proc(51, 50, "/bin/bash scripts/merge-main.sh"),
                 proc(52, 51, "python3 scripts/ci/merge_main.py"),
                 proc(53, 52, "swift test"),
                 proc(60, 1, "/usr/bin/xcodebuild -scheme cmux test"),  # reparented directly
                 proc(70, 1, "/usr/bin/swift build")]
        s = survey(procs, {51: PROJECT, 60: "/private/tmp/claude-501/x/scratch", 70: "/Users/leo/Downloads"})
        got = {e["pid"]: (e["rule"], e["processes"]) for e in s["orphans"]}
        self.assertEqual(got, {51: ("merge-main", 3), 60: ("xcodebuild", 1)})
        self.assertEqual({q.pid for q in s["orphans"][0]["_tree"]} | {q.pid for q in s["orphans"][1]["_tree"]},
                         {51, 52, 53, 60})
        self.assertEqual(reasons(s), {70: "outside-projects"})

    def test_orphan_without_cwd_evidence_is_kept(self) -> None:
        procs = [launchd(), proc(53, 1, "/usr/bin/swift build")]
        self.assertEqual(reasons(survey(procs)), {53: "cwd-unknown"})

    def test_leos_interactive_runs_are_kept(self) -> None:
        procs = [launchd(),
                 proc(60, 1, "/Applications/cmux.app/Contents/MacOS/cmux"),
                 proc(61, 60, "/usr/bin/login -flp leo"),
                 proc(62, 61, "-zsh", path="/bin/zsh"),
                 proc(63, 62, "swift test"),
                 proc(70, 1, "tmux new -s work", path="/opt/homebrew/bin/tmux"),
                 proc(71, 70, "-zsh", path="/bin/zsh"),
                 proc(72, 71, "cargo build"),
                 proc(80, 1, "/Applications/Xcode.app/Contents/MacOS/Xcode"),
                 proc(81, 80, "/usr/bin/xcodebuild -scheme cmux build")]
        s = survey(procs, {63: PROJECT, 72: PROJECT, 81: PROJECT})
        self.assertEqual(s["orphans"], [])
        self.assertEqual(reasons(s), {63: "not-agent", 72: "not-agent", 81: "not-agent"})

    def test_ci_runner_and_glaeda_apple_are_kept(self) -> None:
        runner = "/Users/leo/Projects/cmux/.local/runner"
        procs = [launchd(),
                 proc(90, 1, f"{runner}/bin/Runner.Listener run", path=f"{runner}/bin/Runner.Listener"),
                 proc(91, 90, f"{runner}/bin/Runner.Worker spawnclient", path=f"{runner}/bin/Runner.Worker"),
                 proc(92, 91, f"/bin/bash -e {runner}/_work/_temp/x.sh"),
                 proc(93, 92, "xcodebuild -scheme cmux test"),
                 proc(94, 1, "swift test"),  # a job's orphan: it runs inside the runner
                 proc(95, 1, "/bin/zsh -c glaeda-apple warm"),
                 proc(96, 95, "python3 scripts/apple_build.py warm"),
                 proc(97, 96, "xcodebuild -scheme cmux build")]
        s = survey(procs, {93: runner + "/_work/cmux", 94: runner + "/_work/cmux", 97: PROJECT})
        self.assertEqual(s["orphans"], [])
        self.assertEqual(reasons(s), {93: "ci-runner", 94: "ci-runner", 97: "glaeda-apple"})

    def test_other_users_and_zombies_are_ignored(self) -> None:
        procs = [launchd(), proc(20, 1, "swift test", uid=0), proc(21, 1, "swift build", stat="Z")]
        s = survey(procs, {20: PROJECT, 21: PROJECT})
        self.assertEqual((s["orphans"], s["kept"]), ([], []))


class GraceTest(unittest.TestCase):
    def orphans(self):
        procs = [launchd(), proc(51, 1, "swift test"), proc(52, 1, "swift build")]
        return survey(procs, {51: PROJECT, 52: PROJECT})["orphans"]

    def test_first_sighting_is_not_raced(self) -> None:
        due, seen = lg.settle(self.orphans(), {}, now=1000)
        self.assertEqual(due, [])
        self.assertEqual(set(seen.values()), {1000})

    def test_seen_a_run_ago_is_due(self) -> None:
        _, seen = lg.settle(self.orphans(), {}, now=1000)
        due, seen2 = lg.settle(self.orphans(), seen, now=1030)
        self.assertEqual(sorted(e["pid"] for e in due), [51, 52])
        self.assertEqual(due[0]["orphaned_seconds"], 30)
        self.assertEqual(seen2, seen)
        early, _ = lg.settle(self.orphans(), seen, now=1010)
        self.assertEqual(early, [])

    def test_a_reused_pid_starts_over_and_gone_ones_drop(self) -> None:
        stale = {"51 Sat Sep 26 01:00:00 2026": 1.0, "99 " + START: 1.0}
        due, seen = lg.settle(self.orphans(), stale, now=1000)
        self.assertEqual(due, [])
        self.assertEqual(set(seen), {"51 " + START, "52 " + START})

    def test_seen_file_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "state/seen.json"
            self.assertEqual(lg.read_seen(path), {})
            lg.write_seen({"1 x": 5.0}, path)
            self.assertEqual(lg.read_seen(path), {"1 x": 5.0})
            path.write_text("[1, 2]")
            self.assertEqual(lg.read_seen(path), {})


class HostTest(unittest.TestCase):
    def test_only_a_hygiene_only_mac(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config = Path(tmp) / "update.json"
            self.assertFalse(lg.laptop(config))
            config.write_text(json.dumps({"setupArgs": []}))
            self.assertFalse(lg.laptop(config))
            config.write_text(json.dumps({"setupArgs": ["--hygiene-only"]}))
            self.assertEqual(lg.laptop(config), sys.platform == "darwin")
            config.write_text("{not json")
            self.assertFalse(lg.laptop(config))

    def test_override_file_and_durations(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "allow"
            self.assertIsNone(lg.allowed_until(path))
            path.write_text("1790000000\n")
            self.assertEqual(lg.allowed_until(path), 1790000000.0)
            path.write_text("garbage")
            self.assertIsNone(lg.allowed_until(path))
        self.assertEqual([lg.parse_duration(t) for t in ("90s", "45m", "2h", "10", "0")], [90, 2700, 7200, 600, 0])
        with self.assertRaises(Exception):
            lg.parse_duration("soon")

    def test_parse_lsof(self) -> None:
        text = "p20\nfcwd\nn/Users/leo/Projects/cmux\np21\nfcwd\nn/private/tmp/claude-501/a b\n"
        self.assertEqual(lg.parse_lsof(text), {20: "/Users/leo/Projects/cmux", 21: "/private/tmp/claude-501/a b"})


class ApplyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.log = Path(self.tmp.name) / "guard.jsonl"
        self.procs = [launchd(), proc(20, 1, "swift test"), proc(21, 20, "swift-test"), proc(22, 21, "swift-frontend")]
        self.due = survey(self.procs, {20: PROJECT})["orphans"]
        self.sent: list[tuple[int, int]] = []

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def kill(self, pid, sig):
        self.sent.append((pid, sig))

    def records(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def run_apply(self, look, fresh):
        by_pid = {p.pid: p for p in self.procs}
        return lg.apply(self.due, self.log, look=look or (lambda pid: by_pid.get(pid)), kill=self.kill,
                        fresh=fresh, sleep=lambda s: None)

    def test_term_then_kill_survivors(self) -> None:
        survivor = next(p for p in self.procs if p.pid == 22)
        [r] = self.run_apply(None, lambda: [survivor])
        self.assertEqual(sorted(pid for pid, sig in self.sent if sig == signal.SIGTERM), [20, 21, 22])
        self.assertEqual(self.sent[-1], (22, signal.SIGKILL))
        self.assertEqual((r["outcome"], r["reason"]), ("killed", "orphan"))
        records = self.records()
        self.assertEqual([x["outcome"] for x in records], ["signalling", "killed"])
        self.assertEqual(records[1]["cwd"], PROJECT)
        self.assertNotIn("_proc", records[1])

    def test_clean_exit(self) -> None:
        [r] = self.run_apply(None, lambda: [])
        self.assertEqual(r["outcome"], "terminated")
        self.assertNotIn(signal.SIGKILL, {s for _, s in self.sent})

    def test_changed_or_gone_root_is_not_signalled(self) -> None:
        other = proc(20, 1, "swift test --other")
        [r] = self.run_apply(lambda pid: other, lambda: [])
        self.assertEqual((r["outcome"], self.sent), ("skipped-changed", []))
        [r] = self.run_apply(lambda pid: None, lambda: [])
        self.assertEqual((r["outcome"], self.sent), ("gone", []))

    def test_status_shows_outcomes_not_intents(self) -> None:
        self.run_apply(None, lambda: [])
        records = lg.recent(self.log, 10)
        self.assertEqual([r["outcome"] for r in records], ["terminated"])
        text = lg.render_status(["guard active"], records)
        self.assertIn("orphan swift-test", text)


if __name__ == "__main__":
    unittest.main()
