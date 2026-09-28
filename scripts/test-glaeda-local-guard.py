#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-local-guard, on fake process tables."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import dataclasses
import functools
import json
import os
import signal
import subprocess
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


def proc(pid, ppid, command, path=None, elapsed=600, uid=UID, stat="S", tty="??", cputime=0.0, start=START):
    exe_path = path if path is not None else command.split(" ", 1)[0]
    return lg.Proc(pid, ppid, uid, elapsed, 1024, 0.0, stat, start, command, exe_path, tty=tty, cputime=cputime)


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
        empty = {"runs": {}, "commands": {}, "roots": {}}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "state/seen.json"
            self.assertEqual(lg.read_seen(path), empty)
            seen = {"runs": {"1 x": 5.0}, "commands": {"2 y": {"cpu": 1.5, "since": 7.0}}, "roots": {"3 z": 8.0}}
            lg.write_seen(seen, path)
            self.assertEqual(lg.read_seen(path), seen)
            path.write_text("[1, 2]")
            self.assertEqual(lg.read_seen(path), empty)

    def test_first_format_reads_as_runs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "seen.json"
            path.write_text(json.dumps({"1 x": 5.0, "2 y": 6}))
            self.assertEqual(lg.read_seen(path), {"runs": {"1 x": 5.0, "2 y": 6.0}, "commands": {}, "roots": {}})


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


DAY = 86400
T0 = 1_790_000_000.0
ROOT_PIDS = [66464, 66466, 66467]
KEY = "66446 " + START
PIPE = ("PIPE", "0xaaaa", "0xbbbb")  # our end, and the reader's end


def incident(elapsed=4 * DAY, tty="??", grep_cpu=173.0, extra=()):
    """An SSH remote command left under launchd: zsh -c sudo -n sh -c grep, blocked for days."""
    return [launchd(),
            proc(66446, 1, "zsh -c sudo -n sh -c 'grep -rIl x /etc /Users'", path="/bin/zsh", elapsed=elapsed, tty=tty),
            proc(66464, 66446, "sudo -n sh -c grep -rIl x /etc /Users", path="/usr/bin/sudo", uid=0, elapsed=elapsed,
                 cputime=0.01),
            proc(66466, 66464, "sh -c grep -rIl x /etc /Users", path="/bin/sh", uid=0, elapsed=elapsed),
            proc(66467, 66466, "grep -rIl x /etc /Users", path="/usr/bin/grep", uid=0, elapsed=elapsed,
                 cputime=grep_cpu),
            *extra]


class Host:
    """Fake lsof answers for guard passes."""

    def __init__(self, stdout=PIPE, cwd="/Users/leo", held=None, scan_fails=False):
        self.stdout, self.cwd = stdout, cwd
        self.held = None if scan_fails else held or {}  # device -> pids
        self.fds_asked: list[int] = []
        self.cwds_asked: list[int] = []
        self.scans = 0

    def fds(self, pids):
        self.fds_asked.extend(pids)
        return {66446: {"cwd": self.cwd, "stdout": self.stdout}}

    def holders(self):
        self.scans += 1
        return self.held

    def cwds(self, pids):
        self.cwds_asked.extend(pids)
        return {20: PROJECT}

    def run(self, procs, seen, now, runs=False):
        return lg.guard_pass(procs, UID, runs, seen, now, cwds=self.cwds, fds=self.fds, holders=self.holders,
                             home=HOME)


EMPTY = {"runs": {}, "commands": {}, "roots": {}}
REFUSED = "sudo: a password is required\n"


class StaleCommandTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.log = Path(self.tmp.name) / "guard.jsonl"
        self.sent: list[tuple[int, int]] = []
        self.argvs: list[list[str]] = []

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def kill(self, pid, sig):
        self.sent.append((pid, sig))

    def sudo_run(self, *answers):
        """Answer successive sudo calls with (returncode, stderr); the last answer repeats."""
        answers = list(answers) or [(0, "")]

        def run(argv, **_):
            self.argvs.append(argv)
            code, err = answers.pop(0) if len(answers) > 1 else answers[0]
            return subprocess.CompletedProcess(argv, code, "", err)
        return run

    def settled(self, host, procs, first=T0, then=T0 + 3600):
        seen = host.run(procs, EMPTY, first)["seen"]
        return host.run(procs, seen, then)

    def apply(self, due, procs, run, survivors=(), look=None):
        by_pid = {p.pid: p for p in procs}
        snapshots = [procs, [by_pid[pid] for pid in survivors]]  # before the root TERM, then 5 s later
        return lg.apply(due, self.log, look=look or by_pid.get, kill=self.kill,
                        fresh=lambda: snapshots.pop(0) if len(snapshots) > 1 else snapshots[0],
                        sleep=lambda s: None, sudo=functools.partial(lg.sudo_kill, run=run))

    def test_incident_is_stopped_on_a_fleet_mini(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config = Path(tmp) / "update.json"
            config.write_text(json.dumps({"setupArgs": ["--runner"]}))
            active = lg.laptop(config)
        self.assertFalse(active)
        procs = incident(extra=[proc(20, 1, "swift test")])  # an orphaned run: laptops only
        host = Host(held={"0xaaaa": {66446, 66464, 66466, 66467}})  # only the tree holds its own end
        first = host.run(procs, EMPTY, T0, runs=active)
        self.assertEqual([(e["pid"], e["status"], e["idle_seconds"]) for e in first["commands"]],
                         [(66446, "active", 0)])
        self.assertEqual((host.fds_asked, host.scans), ([], 0), "no lsof before the age and idle gates")
        p = host.run(procs, first["seen"], T0 + 3600, runs=active)
        self.assertEqual(host.cwds_asked, [], "orphaned runs are not surveyed off a laptop")
        self.assertEqual((host.fds_asked, host.scans), ([66446], 1))
        [due] = p["due"]
        self.assertEqual((due["pid"], due["status"], due["idle_seconds"], due["processes"]), (66446, "due", 3600, 4))
        self.assertEqual(due["root_pids"], ROOT_PIDS)
        self.assertEqual(p["seen"]["commands"][KEY]["since"], T0)

        [r] = self.apply(p["due"], procs, self.sudo_run(), survivors=[66467])
        self.assertEqual(self.argvs, [["/usr/bin/sudo", "-n", "/bin/kill", "-s", "TERM", "66464", "66466", "66467"],
                                      ["/usr/bin/sudo", "-n", "/bin/kill", "-s", "KILL", "66467"]])
        self.assertEqual(self.sent, [(66446, signal.SIGTERM)])
        self.assertEqual((r["outcome"], r["rule"], r["root_pids"], r["idle_seconds"]),
                         ("killed", "stale-command", ROOT_PIDS, 3600))
        self.assertEqual([x["outcome"] for x in lg.recent(self.log, 10)], ["killed"])
        text = lg.render_status(["guard active"], lg.recent(self.log, 10))
        self.assertIn("stale commands:", text)
        self.assertIn("age 4d0h, idle 1h0m, root pids 66464 66466 66467", text)
        plan = lg.render_plan(["guard active"], p)
        self.assertIn("due        pid 66446 age 4d0h, idle 1h0m, 4 processes, root pids 66464 66466 66467", plan)

    def test_a_reparented_root_survivor_still_gets_its_kill(self) -> None:
        procs = incident()
        p = self.settled(Host(), procs)
        sudo = next(q for q in procs if q.pid == 66464)
        reparented = dataclasses.replace(sudo, ppid=1)  # its parent zsh took the TERM
        by_pid = {q.pid: q for q in procs}
        snapshots = [procs, [reparented]]
        lg.apply(p["due"], self.log, look=by_pid.get, kill=self.kill, fresh=lambda: snapshots.pop(0),
                 sleep=lambda s: None, sudo=functools.partial(lg.sudo_kill, run=self.sudo_run()))
        self.assertEqual(self.argvs[-1][-2:], ["KILL", "66464"])

    def test_a_live_reader_keeps_it(self) -> None:
        # a session waiting on a backgrounded subshell holds the pipe's other end
        host = Host(held={"0xbbbb": {4242}, "0xaaaa": {66446}})
        p = self.settled(host, incident())
        self.assertEqual((p["due"], p["commands"][0]["status"]), ([], "read"))
        self.assertEqual(self.settled(Host(held={"0xbbbb": {66467}}), incident())["commands"][0]["status"], "due",
                         "a reader inside the tree does not count")

    def test_a_failed_reader_scan_keeps_it(self) -> None:
        p = self.settled(Host(scan_fails=True), incident())
        self.assertEqual((p["due"], p["commands"][0]["status"]), ([], "reader-unknown"))

    def test_a_closed_reader_needs_no_scan(self) -> None:
        host = Host(stdout=("PIPE", "0xaaaa", ""))
        p = self.settled(host, incident())
        self.assertEqual(([e["pid"] for e in p["due"]], host.scans), ([66446], 0))

    def test_a_tree_with_a_tty_is_kept(self) -> None:
        host = Host()
        p = self.settled(host, incident(tty="ttys003"))
        self.assertEqual((p["due"], [(e["pid"], e["reason"]) for e in p["commands_kept"]]), ([], [(66446, "tty")]))
        self.assertEqual(host.fds_asked, [], "lsof only for candidates")

    def test_stdout_to_a_file_or_dev_null_is_kept(self) -> None:
        for stdout, status in ((("REG", "", "/Users/leo/scan.log"), "logged"), (("CHR", "", "/dev/null"), "detached")):
            host = Host(stdout=stdout)
            p = self.settled(host, incident())
            self.assertEqual((p["due"], p["commands"][0]["status"], host.scans), ([], status, 0))

    def test_a_cwd_under_a_runner_is_kept(self) -> None:
        for cwd in ("/Users/leo/actions-runner-glaeda-2/_work/cmux", "/Users/leo/Projects/cmux/.local/runner"):
            p = self.settled(Host(cwd=cwd), incident())
            self.assertEqual((p["due"], p["commands"][0]["status"]), ([], "ci-runner"), cwd)

    def test_a_tree_younger_than_six_hours_is_kept(self) -> None:
        host = Host()
        p = self.settled(host, incident(elapsed=5 * 3600), then=T0 + 2 * 3600)
        self.assertEqual((p["due"], p["commands"][0]["status"], host.fds_asked), ([], "young", []))

    def test_cpu_progress_resets_the_idle_clock(self) -> None:
        host = Host()
        seen = host.run(incident(grep_cpu=173.0), EMPTY, T0)["seen"]
        p = host.run(incident(grep_cpu=174.5), seen, T0 + 3000)
        self.assertEqual((p["commands"][0]["status"], p["commands"][0]["idle_seconds"]), ("active", 0))
        p = host.run(incident(grep_cpu=174.5), p["seen"], T0 + 3700)
        self.assertEqual((p["due"], p["commands"][0]["idle_seconds"]), ([], 700))
        self.assertEqual(p["seen"]["commands"][KEY]["since"], T0 + 3000)

    def test_membership_change_with_equal_cpu_is_activity(self) -> None:
        host = Host()
        poll = proc(70000, 66446, "gh run view 1", cputime=0.0)  # a poll loop's short child, no CPU yet
        seen = host.run(incident(), EMPTY, T0)["seen"]
        p = host.run(incident(extra=[poll]), seen, T0 + 3600)
        self.assertEqual((p["commands"][0]["status"], p["commands"][0]["idle_seconds"]), ("active", 0))
        self.assertEqual(p["commands"][0]["cpu_seconds"], 173.01)

    def test_refused_sudo_is_needs_root_and_waits(self) -> None:
        procs = incident()
        host = Host()
        p = self.settled(host, procs)
        [r] = self.apply(p["due"], procs, self.sudo_run((1, REFUSED)))
        self.assertEqual((r["outcome"], r["needs_root"]), ("needs-root", ROOT_PIDS))
        self.assertEqual(self.sent, [], "nothing in the tree is signalled without its root members")
        self.assertTrue(lg.remember_refusals([r], p["due"], p["seen"], T0 + 3600))
        later = host.run(procs, p["seen"], T0 + 3630)
        self.assertEqual((later["due"], later["commands"][0]["status"], later["commands"][0]["needs_root"]),
                         ([], "needs-root", ROOT_PIDS))
        self.assertIn("needs root for pids 66464 66466 66467", lg.render_plan(["guard active"], later))
        retry = host.run(procs, later["seen"], T0 + 7200)
        self.assertEqual([e["pid"] for e in retry["due"]], [66446])

    def test_kill_phase_refusal_is_retried_hourly(self) -> None:
        procs = incident()
        p = self.settled(Host(), procs)
        [r] = self.apply(p["due"], procs, self.sudo_run((0, ""), (1, REFUSED)), survivors=[66467])
        self.assertEqual((r["outcome"], r["stage"], r["needs_root"]), ("needs-root", "kill", [66467]))
        self.assertTrue(lg.remember_refusals([r], p["due"], p["seen"], T0 + 3600))
        grep_key = "66467 " + START
        self.assertEqual(p["seen"]["roots"], {grep_key: T0 + 3600})
        left = [q for q in procs if q.pid == 66467]
        self.argvs.clear()
        run = functools.partial(lg.sudo_kill, run=self.sudo_run((1, REFUSED)))
        self.assertEqual(lg.retry_roots(left, p["seen"]["roots"], T0 + 3700, self.log, sudo=run),
                         {grep_key: T0 + 3600})
        self.assertEqual(self.argvs, [], "not before the hour")
        self.assertEqual(lg.retry_roots(left, p["seen"]["roots"], T0 + 7200, self.log, sudo=run),
                         {grep_key: T0 + 7200})
        ok = functools.partial(lg.sudo_kill, run=self.sudo_run())
        self.assertEqual(lg.retry_roots(left, {grep_key: T0 + 7200}, T0 + 10800, self.log, sudo=ok), {})
        self.assertEqual(self.argvs[-1], ["/usr/bin/sudo", "-n", "/bin/kill", "-s", "KILL", "66467"])
        self.assertEqual(lg.retry_roots([], {grep_key: T0}, T0 + 10800, self.log, sudo=ok), {}, "gone ones drop")
        self.assertEqual([x["outcome"] for x in lg.recent(self.log, 10)], ["needs-root", "needs-root", "killed"])

    def test_changed_or_gone_top_is_not_signalled(self) -> None:
        procs = incident()
        p = self.settled(Host(), procs)
        other = proc(66446, 1, "zsh -c something else", path="/bin/zsh", elapsed=4 * DAY)
        [r] = self.apply(p["due"], procs, self.sudo_run(), look=lambda pid: other)
        self.assertEqual((r["outcome"], self.sent, self.argvs), ("skipped-changed", [], []))
        [r] = self.apply(p["due"], procs, self.sudo_run(), look=lambda pid: None)
        self.assertEqual((r["outcome"], self.sent, self.argvs), ("gone", [], []))

    def test_root_members_changed_before_the_term_are_dropped(self) -> None:
        procs = incident()
        p = self.settled(Host(), procs)
        by_pid = {q.pid: q for q in procs}
        reused = proc(66467, 66466, "other", uid=0, start="Mon Sep 28 01:00:00 2026")  # a reused pid
        now = [q for q in procs if q.pid != 66467] + [reused]
        snapshots = [now, []]
        lg.apply(p["due"], self.log, look=by_pid.get, kill=self.kill, fresh=lambda: snapshots.pop(0),
                 sleep=lambda s: None, sudo=functools.partial(lg.sudo_kill, run=self.sudo_run()))
        self.assertEqual(self.argvs, [["/usr/bin/sudo", "-n", "/bin/kill", "-s", "TERM", "66464", "66466"]])

    def test_sudo_refusal_is_detected_positively(self) -> None:
        def ok(code, err):
            return lg.sudo_kill("KILL", [9], run=self.sudo_run((code, err)))
        self.assertTrue(ok(0, ""))
        self.assertTrue(ok(1, "kill: 9: No such process\n"))
        self.assertFalse(ok(1, REFUSED))
        self.assertFalse(ok(1, "Sorry, user leo is not allowed to execute '/bin/kill -s KILL 9' as root "
                               "on this-host.\n"))
        self.assertFalse(ok(1, "kill: 9: No such process\nsomething else\n"))
        self.assertFalse(ok(1, ""))

        def missing(argv, **_):
            raise FileNotFoundError(argv[0])
        self.assertFalse(lg.sudo_kill("KILL", [9], run=missing))

    def test_runners_sessions_and_foreign_members(self) -> None:
        top = proc(80, 1, "/bin/bash -lc ./run.sh", elapsed=DAY)
        cases = {
            "ci-runner": [top, proc(81, 80, "/Users/leo/actions-runner-glaeda-2/bin/Runner.Worker",
                                    path="/Users/leo/actions-runner-glaeda-2/bin/Runner.Worker")],
            "glaeda-apple": [top, proc(81, 80, "python3 scripts/apple_build.py warm")],
            "live-session": [top, proc(81, 80, CLAUDE, path=CLAUDE)],
        }
        for reason, procs in cases.items():
            s = lg.commands([launchd(), *procs], UID)
            self.assertEqual([e["reason"] for e in s["kept"]], [reason], reason)
        # a root process not started through sudo in the tree is left alone
        procs = [launchd(), top, proc(81, 80, "/usr/bin/login -f x", uid=0), proc(82, 80, "sleep 99")]
        [e] = lg.commands(procs, UID)["commands"]
        self.assertEqual((e["root_pids"], sorted(q.pid for q in e["_tree"])), ([], [80, 82]))

    def test_shells_without_c_and_other_users_are_not_candidates(self) -> None:
        procs = [launchd(), proc(90, 1, "/bin/zsh -l", elapsed=DAY), proc(91, 1, "/bin/sh -c x", uid=0, elapsed=DAY),
                 proc(92, 1, "/bin/bash tests/test_x.sh -c", elapsed=DAY)]
        self.assertEqual(lg.commands(procs, UID), {"commands": [], "kept": []})
        self.assertTrue(lg.runs_string(["-lc", "x"]))
        self.assertFalse(lg.runs_string(["--login", "script.sh", "-c"]))

    def test_laptop_runs_are_unchanged(self) -> None:
        procs = incident(extra=[proc(20, 1, "swift test")])
        host = Host()
        seen = host.run(procs, EMPTY, T0, runs=True)["seen"]
        p = host.run(procs, seen, T0 + 30, runs=True)
        self.assertEqual([(e["pid"], e["rule"]) for e in p["due"]], [(20, "swift-test")])
        self.assertEqual(p["seen"]["runs"], {"20 " + START: T0})

    def test_a_malformed_commands_section_keeps_the_runs_clocks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "seen.json"
            path.write_text(json.dumps({"runs": {"20 x": 5.0}, "commands": {"a": 3, "b": {"cpu": 1.0}}, "roots": 7}))
            self.assertEqual(lg.read_seen(path), {"runs": {"20 x": 5.0}, "commands": {"b": {"cpu": 1.0}}, "roots": {}})
            path.write_text(json.dumps({"runs": {"20 x": 5.0}, "commands": [1]}))
            self.assertEqual(lg.read_seen(path)["runs"], {"20 x": 5.0})

    def test_parse_fds_and_holders(self) -> None:
        text = ("p80\nfcwd\ntDIR\nn/Users/leo\nf1\ntPIPE\nd0xaaaa\nn->0xbbbb\n"
                "p81\nfcwd\ntDIR\nn/\nf1\ntPIPE\nd0xcccc\nn\n"
                "p82\nf1\ntREG\nn/tmp/log\n")
        self.assertEqual(lg.parse_fds(text), {80: {"cwd": "/Users/leo", "stdout": ("PIPE", "0xaaaa", "0xbbbb")},
                                              81: {"cwd": "/", "stdout": ("PIPE", "0xcccc", "")},
                                              82: {"stdout": ("REG", "", "/tmp/log")}})
        self.assertEqual(lg.parse_holders("p80\nf1\nd0xaaaa\np90\nf0\nd0xbbbb\nf3\ndflowsw\n"),
                         {"0xaaaa": {80}, "0xbbbb": {90}})


if __name__ == "__main__":
    unittest.main()
