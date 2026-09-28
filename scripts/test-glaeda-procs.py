#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-procs."""

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
loader = importlib.machinery.SourceFileLoader("glaeda_procs", os.fspath(ROOT / "scripts" / "glaeda-procs"))
spec = importlib.util.spec_from_loader("glaeda_procs", loader)
gp = importlib.util.module_from_spec(spec)
sys.modules["glaeda_procs"] = gp
loader.exec_module(gp)

UID = 501
PAUSE = "/opt/homebrew/bin/python3 -c import signal; signal.pause()"
START = "Sat Sep 26 04:21:22 2026"


def proc(pid, ppid=1, command=PAUSE, elapsed=7200, rss=4096, cpu=0.0, stat="S", uid=UID,
         path="", start=START):
    return gp.Proc(pid, ppid, uid, elapsed, rss, cpu, stat, start, command, path)


class ParseTest(unittest.TestCase):
    def test_etime(self) -> None:
        self.assertEqual(gp.parse_etime("05"), 5)
        self.assertEqual(gp.parse_etime("01:05"), 65)
        self.assertEqual(gp.parse_etime("02:01:05"), 7265)
        self.assertEqual(gp.parse_etime("3-02:01:05"), 3 * 86400 + 7265)

    def test_ps_row_keeps_command_with_spaces(self) -> None:
        row = f"  42     1   501  1-00:00:01  4704   0.0 SNs  {START} {PAUSE}\n"
        [p] = gp.parse_ps(row + "garbage\n")
        self.assertEqual((p.pid, p.ppid, p.uid, p.elapsed, p.rss_kib), (42, 1, 501, 86401, 4704))
        self.assertEqual(p.start, START)
        self.assertEqual(p.command, PAUSE)

    def test_comm_paths_with_spaces(self) -> None:
        paths = gp.parse_comm("  7 /Applications/Claude.app/Contents/MacOS/Claude Helper\n")
        self.assertEqual(paths, {7: "/Applications/Claude.app/Contents/MacOS/Claude Helper"})


class FixtureTest(unittest.TestCase):
    def kind(self, p, min_age=3600):
        return gp.fixture_kind(p, UID, min_age)

    def test_orphaned_idle_pause_is_a_fixture(self) -> None:
        self.assertEqual(self.kind(proc(10)), "python-signal-pause")
        self.assertEqual(self.kind(proc(11, command="sleep infinity")), "sleep-forever")

    def test_protected_shapes_are_not_fixtures(self) -> None:
        self.assertIsNone(self.kind(proc(10, ppid=500)), "still has its parent")
        self.assertIsNone(self.kind(proc(10, uid=0)), "another user")
        self.assertIsNone(self.kind(proc(10, elapsed=60)), "younger than min age")
        self.assertIsNone(self.kind(proc(10, cpu=5.0)), "doing work")
        self.assertIsNone(self.kind(proc(10, stat="Z")), "already dead")
        self.assertIsNone(self.kind(proc(10, command=PAUSE + "; import os")), "more than the fixture")
        self.assertIsNone(self.kind(proc(10, command="python3 ci_dash.py")), "a real program")
        self.assertIsNone(self.kind(proc(10, command="sleep 30")), "a bounded sleep")


CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
ROOTS = [Path("/private/tmp")]


def chrome(pid, profile="/private/tmp/cidash-sweep-1", extra="--headless=new --remote-debugging-port=0", **kw):
    return proc(pid, command=f"{CHROME} {extra} --user-data-dir={profile} about:blank", path=CHROME,
                cpu=kw.pop("cpu", 80.0), stat=kw.pop("stat", "R"), **kw)


class ChromeTest(unittest.TestCase):
    def kind(self, p):
        return gp.fixture_kind(p, UID, 3600, ROOTS)

    def test_orphaned_spinning_headless_chrome_is_a_fixture(self) -> None:
        self.assertEqual(self.kind(chrome(10)), "headless-chrome")
        self.assertEqual(self.kind(chrome(10, extra="--headless --remote-debugging-port=9222")), "headless-chrome")

    def test_profile_paths_that_cannot_be_delimited_are_refused(self) -> None:
        self.assertIsNone(self.kind(chrome(10, profile="/private/tmp/my profile")), "space in the path")
        p = chrome(10, extra="--headless --remote-debugging-port=0 --user-data-dir=/private/tmp/a")
        self.assertIsNone(self.kind(p), "two profiles; Chrome uses the last")
        self.assertIsNone(self.kind(chrome(10, extra="--headless-shell --remote-debugging-port=0")), "not --headless")

    def test_temp_roots_ignore_tmpdir(self) -> None:
        saved = os.environ.get("TMPDIR")
        os.environ["TMPDIR"] = "/"
        try:
            roots = gp.temp_roots()
        finally:
            if saved is None:
                del os.environ["TMPDIR"]
            else:
                os.environ["TMPDIR"] = saved
        self.assertNotIn(Path("/"), roots)
        self.assertNotIn(Path.home(), roots)
        self.assertIsNone(gp.temp_profile(str(Path.home() / "Library" / "x"), roots))

    def test_protected_browsers(self) -> None:
        self.assertIsNone(self.kind(chrome(10, extra="--remote-debugging-port=0")), "not headless")
        self.assertIsNone(self.kind(chrome(10, extra="--headless=new")), "no debugging port")
        self.assertIsNone(self.kind(chrome(10, profile="/Users/u/Library/Chrome")), "real profile")
        self.assertIsNone(self.kind(chrome(10, profile="/private/tmp")), "the temp root itself")
        self.assertIsNone(self.kind(chrome(10, profile="/private/tmp/../etc")), "escapes the temp root")
        self.assertIsNone(self.kind(chrome(10, ppid=400)), "still has its parent")
        self.assertIsNone(self.kind(chrome(10, elapsed=60)), "younger than min age")

    def test_helpers_count_toward_the_leak(self) -> None:
        procs = [chrome(10, rss=100_000), proc(11, ppid=10, command="Google Chrome Helper", rss=50_000)]
        [f] = gp.survey(procs, UID, 1, ROOTS)["fixtures"]
        self.assertEqual((f["pid"], f["rss_bytes"]), (10, 150_000 * 1024))
        self.assertNotIn("cidash", json.dumps(gp.public(gp.survey(procs, UID, 1, ROOTS))))


class SurveyTest(unittest.TestCase):
    def test_attribution(self) -> None:
        procs = [
            proc(100, command="/Users/u/.local/bin/claude --session-id 655bc8de-fb1f-4296", path="/Users/u/.local/bin/claude", rss=300_000),
            proc(101, ppid=100, command="/bin/zsh -c swift test", path="/bin/zsh", rss=1_000),
            proc(102, ppid=101, command="swift-test --package-path P", path="/X/usr/bin/swift-test", rss=50_000),
            proc(103, ppid=102, command="swift-frontend -frontend", path="/X/usr/bin/swift-frontend", rss=200_000),
            proc(200, command="Runner.Listener run", path="/r/bin/Runner.Listener"),
            proc(201, ppid=200, command="Runner.Worker", path="/r/bin/Runner.Worker"),
            proc(202, ppid=201, command="xcodebuild test", path="/usr/bin/xcodebuild"),
            proc(300, command="python3 ci_dash.py", path="/usr/bin/python3"),
            proc(301, command="python3 ci_dash.py", path="/usr/bin/python3"),
            proc(400),
            proc(401, command="/Users/u/.local/bin/codex app-server --listen unix://", path="/Users/u/.local/bin/codex"),
        ]
        s = gp.survey(procs, UID, 1)
        [session] = s["sessions"]
        self.assertEqual((session["pid"], session["session_id"], session["processes"]), (100, "655bc8de-fb1f-4296", 4))
        self.assertEqual(session["rss_bytes"], 551_000 * 1024)
        builds = {b["pid"]: b for b in s["builds"]}
        self.assertEqual(set(builds), {102, 202}, "the frontend under swift-test is not its own build")
        self.assertTrue(builds[102]["direct"])
        self.assertEqual(builds[102]["session"]["pid"], 100)
        self.assertEqual(builds[102]["rss_bytes"], 250_000 * 1024)
        self.assertEqual(builds[202]["origin"], "ci-runner")
        self.assertFalse(builds[202]["direct"])
        self.assertEqual([f["pid"] for f in s["fixtures"]], [400])
        self.assertEqual([(d["count"], d["pids"]) for d in s["duplicate_orphans"]], [(2, [300, 301])])

    def test_duplicate_label_shows_only_script_names(self) -> None:
        token = proc(1, command="tool eyJhbGciOi.eyJzdWIi.sig", path="/usr/local/bin/tool")
        script = proc(2, command="python3 /Users/u/ci_dash.py", path="/usr/bin/python3")
        self.assertEqual(gp.label(token), "tool")
        self.assertEqual(gp.label(script), "python3 ci_dash.py")

    def test_json_omits_command_lines(self) -> None:
        s = gp.survey([proc(400), proc(300, command="python3 secret_tool.py --token abc")], UID, 1)
        text = json.dumps(gp.public(s))
        self.assertNotIn("signal.pause", text)
        self.assertNotIn("--token", text)
        self.assertNotIn("_proc", text)


class ApplyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.receipt = Path(self.tmp.name) / "r" / "receipts.jsonl"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def fixtures(self, *procs):
        return [{"pid": p.pid, "kind": "python-signal-pause", "elapsed_seconds": p.elapsed,
                 "rss_bytes": p.rss_kib * 1024, "_proc": p} for p in procs]

    def test_signals_only_unchanged_identity(self) -> None:
        live = {10: proc(10), 11: proc(11), 12: proc(12)}
        seen = {10: proc(10),
                11: proc(11, start="Sat Sep 26 09:00:00 2026"),  # PID reused
                12: proc(12, ppid=77)}  # re-parented
        sent = []

        def kill(pid, sig):
            sent.append((pid, sig))
            seen.pop(pid)

        out = gp.apply(self.fixtures(*live.values()), self.receipt, look=seen.get, kill=kill, wait_seconds=0.5)
        self.assertEqual(sent, [(10, signal.SIGTERM)])
        self.assertEqual({r["pid"]: r["outcome"] for r in out},
                         {10: "terminated", 11: "skipped-changed", 12: "skipped-changed"})
        records = [json.loads(line) for line in self.receipt.read_text().splitlines()]
        self.assertEqual([(r["pid"], r["outcome"]) for r in records],
                         [(10, "signalling"), (10, "terminated"), (11, "skipped-changed"), (12, "skipped-changed")])
        self.assertTrue(all("command" not in r for r in records))

    def test_gone_and_zombie_and_survivor(self) -> None:
        p20, p21, p22 = proc(20), proc(21), proc(22)
        seen = {21: p21, 22: p22}

        def kill(pid, sig):
            if pid == 21:
                seen[21] = proc(21, stat="Z")

        out = gp.apply(self.fixtures(p20, p21, p22), self.receipt, look=seen.get, kill=kill, wait_seconds=0.2)
        self.assertEqual([r["outcome"] for r in out], ["gone", "terminated", "survived"])

    def test_failed_observation_is_not_an_exit(self) -> None:
        def look(pid):
            raise gp.NoEvidence("ps timed out")

        sent = []
        out = gp.apply(self.fixtures(proc(30)), self.receipt, look=look,
                       kill=lambda pid, sig: sent.append(pid), wait_seconds=0.1)
        self.assertEqual((out[0]["outcome"], sent), ("no-evidence", []))

    def test_observation_failing_after_the_signal_is_not_an_exit(self) -> None:
        p = proc(31)
        calls = iter([p])

        def look(pid):
            try:
                return next(calls)
            except StopIteration:
                raise gp.NoEvidence("ps timed out") from None

        out = gp.apply(self.fixtures(p), self.receipt, look=look, kill=lambda pid, sig: None, wait_seconds=0.1)
        self.assertEqual(out[0]["outcome"], "no-evidence")


class ChromeApplyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(os.path.realpath(self.tmp.name))
        self.receipt = self.root / "receipts.jsonl"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_apply(self, profile: Path, others: list):
        p = chrome(10, profile=str(profile))
        f = [{"pid": 10, "kind": "headless-chrome", "elapsed_seconds": p.elapsed, "rss_bytes": 1,
              "_proc": p, "_profile": gp.temp_profile(str(profile), [self.root])}]
        seen = {10: p}
        return gp.apply(f, self.receipt, look=seen.get, kill=lambda pid, sig: seen.pop(pid),
                        wait_seconds=0.1, fresh=lambda: others, uid=os.getuid(),
                        connected=lambda pid: self.connected)

    connected = False

    def test_profile_removed_after_exit(self) -> None:
        profile = self.root / "frametime-1"
        (profile / "Default").mkdir(parents=True)
        (profile / "Local State").write_text("{}")
        [r] = self.run_apply(profile, [])
        self.assertEqual((r["outcome"], r["profile"]), ("terminated", "removed"))
        self.assertFalse(profile.exists())
        self.assertNotIn("frametime", self.receipt.read_text())

    def test_profile_kept_while_another_process_names_it(self) -> None:
        profile = self.root / "shared"
        profile.mkdir()
        (profile / "Local State").write_text("{}")
        [r] = self.run_apply(profile, [proc(99, ppid=5, command=f"node x.mjs {profile}")])
        self.assertEqual(r["profile"], "kept-in-use")
        self.assertTrue(profile.exists())

    def test_profile_named_through_an_alias_is_in_use(self) -> None:
        profile = self.root / "p1"
        profile.mkdir()
        (profile / "Local State").write_text("{}")
        (self.root / "alias").symlink_to(self.root)
        helper = proc(98, ppid=5, command=f"Google Chrome Helper --type=renderer --user-data-dir={self.root}/alias/p1")
        self.assertTrue(gp.profile_in_use(profile, [helper]))
        [r] = self.run_apply(profile, [helper])
        self.assertEqual(r["profile"], "kept-in-use")
        self.assertTrue(profile.exists())

    def test_connected_browser_is_skipped(self) -> None:
        profile = self.root / "driven"
        profile.mkdir()
        self.connected = True
        [r] = self.run_apply(profile, [])
        self.assertEqual((r["outcome"], r["profile"]), ("skipped-in-use", "kept"))
        self.assertTrue(profile.exists())

    def test_profile_kept_unless_this_run_saw_the_exit(self) -> None:
        profile = self.root / "vanished"
        profile.mkdir()
        p = chrome(10, profile=str(profile))
        f = [{"pid": 10, "kind": "headless-chrome", "elapsed_seconds": 1, "rss_bytes": 1,
              "_proc": p, "_profile": profile}]
        [r] = gp.apply(f, self.receipt, look=lambda pid: None, kill=lambda pid, sig: None,
                       fresh=lambda: [], uid=os.getuid(), connected=lambda pid: False)
        self.assertEqual((r["outcome"], r["profile"]), ("gone", "kept"))
        self.assertTrue(profile.exists())

    def test_directory_without_chrome_state_is_kept(self) -> None:
        scratch = self.root / "shared-scratch"
        scratch.mkdir()
        (scratch / "notes.txt").write_text("keep")
        [r] = self.run_apply(scratch, [])
        self.assertEqual((r["outcome"], r["profile"]), ("terminated", "kept-not-a-profile"))
        self.assertTrue((scratch / "notes.txt").exists())

    def test_symlinked_profile_is_not_followed(self) -> None:
        target = self.root / "real"
        target.mkdir()
        (self.root / "link").symlink_to(target)
        self.assertEqual(gp.remove_profile(self.root / "link", os.getuid(), lambda: []), "kept-not-owned")
        self.assertTrue(target.exists())


class RenderTest(unittest.TestCase):
    def test_report_names_the_pressure_and_the_fix(self) -> None:
        h = {"load": [298.4, 250.0, 200.0], "cpus": 10, "pressure": "normal", "memory_bytes": 24 * gp.GIB,
             "swap_total_bytes": 5 * gp.GIB, "swap_used_bytes": 4 * gp.GIB}
        self.assertEqual(gp.verdict(h), "elevated")
        s = gp.survey([proc(400)], UID, 1)
        text = gp.render(h, s, None, 10)
        self.assertIn("host elevated", text)
        self.assertIn("glaeda-procs --apply", text)
        self.assertNotIn("signal.pause", text)


if __name__ == "__main__":
    unittest.main()
