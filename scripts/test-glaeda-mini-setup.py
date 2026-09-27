#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-mini-setup. Runs on Linux CI: host probes are stubbed."""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import plistlib
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader(
    "glaeda_mini_setup", os.fspath(ROOT / "scripts" / "glaeda-mini-setup")
)
spec = importlib.util.spec_from_loader("glaeda_mini_setup", loader)
ms = importlib.util.module_from_spec(spec)
sys.modules["glaeda_mini_setup"] = ms
loader.exec_module(ms)
ms.DARWIN_REQUIRED = False
ms.HOST_PLATFORM = "macos"  # CI runs these on Linux; the Linux profile has its own tests below
REAL_ENROLL_PYTHON = ms.enroll_python

PMSET_SLEEPY = """Battery Power:
 sleep                1
AC Power:
 womp                 0
 sleep                10
 autorestart          0
"""
PMSET_READY = """AC Power:
 womp                 1
 sleep                0
 autorestart          1
"""


def fake_preflight(power: dict | None = None, blocking: list[str] | None = None):
    def probe(ctx):
        checks = {name: ms.check(True, "stub") for name in (
            "macos", "appleSilicon", "macMini", "hardware", "buildLargeClass", "commandLineTools",
            "xcode", "xcodeSelected", "xcodeUsable", "cmuxXcodePin", "diskFree", "tailscale",
            "homebrew", "python", "workloadToolPath", "power")}
        return {
            "checks": checks,
            "hardware": {},
            "tailscale": {"installed": True, "running": True},
            "power": power or {"readable": True, "acSleepDisabled": True, "wakeOnNetwork": True, "autoRestart": True},
            "xcode": {"apps": [], "selected": ms.CMUX_XCODE_APP + "/Contents/Developer", "licenceNeeded": False},
            "blocking": blocking or [],
        }
    return probe


class MiniSetupTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(os.path.realpath(self.tmp.name)) / "operator"
        self.home.mkdir()
        self.reclaim = Path(self.tmp.name) / "glaeda-worktree-reclaim"
        self.reclaim.write_bytes(b"\x7fELF fake reclaim binary\n")
        self.calls: list[list[str]] = []
        real_run = ms.run

        def recording_run(argv, *args, **kwargs):
            self.calls.append(list(argv))
            if argv[0] == "/bin/launchctl" or os.path.basename(argv[0]) == "systemctl":
                raise AssertionError("launchctl and systemctl must never run with a sandbox HOME")
            return real_run(argv, *args, **kwargs)

        self.patches = [
            mock.patch.object(ms, "preflight", fake_preflight()),
            mock.patch.object(ms, "run", recording_run),
            mock.patch.dict(os.environ, {"HOME": os.fspath(self.home)}),
            mock.patch.object(ms, "enroll_python", lambda ctx: "/opt/homebrew/bin/python3.13"),
            mock.patch.object(ms, "brew_install", mock.Mock(side_effect=AssertionError("brew must not run"))),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self) -> None:
        for p in reversed(self.patches):
            p.stop()
        self.tmp.cleanup()

    def invoke(self, *args: str) -> dict:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ms.main(["--output", "json", "--python", "/usr/bin/python3",
                            "--reclaim-binary", os.fspath(self.reclaim), *args])
        self.assertEqual(code, 0, out.getvalue()[-2000:])
        return json.loads(out.getvalue())

    def tree(self) -> dict[str, bytes | None]:
        return {
            os.fspath(p.relative_to(self.home)): (p.read_bytes() if p.is_file() else None)
            for p in sorted(self.home.rglob("*"))
        }

    def states(self, receipt: dict) -> set[str]:
        return {a["state"] for a in receipt["actions"]}

    def test_command_line_tools_block_only_without_a_usable_selected_xcode(self) -> None:
        self.assertEqual(ms.command_line_tools_check(True, "26.6", False)["level"], "ok")
        # The selected, licensed Xcode backs git and xcrun: nothing to install.
        self.assertEqual(ms.command_line_tools_check(False, None, True)["level"], "info")
        self.assertEqual(ms.command_line_tools_check(False, None, False)["level"], "block")
        for level, wanted in (("info", False), ("block", True)):
            pre = fake_preflight()(None)
            pre["checks"]["commandLineTools"] = {"level": level, "value": "missing", "note": ""}
            steps = ms.operator_steps(mock.Mock(xcode_app=ms.CMUX_XCODE_APP), pre, None)
            self.assertEqual(any(s["command"] == "xcode-select --install" for s in steps), wanted, level)

    def test_pick_python_trusts_the_shim_beside_a_versioned_xcode(self) -> None:
        def fake_glob(self, pattern):
            return iter([Path("/Applications/Xcode_26.6.app")]) if pattern == "Xcode*.app" else iter([])
        with mock.patch.object(ms.os, "access", return_value=False), \
             mock.patch.object(ms.Path, "exists", return_value=False), \
             mock.patch.object(ms.Path, "glob", fake_glob):
            self.assertEqual(ms.pick_python(), "/usr/bin/python3")
        with mock.patch.object(ms.os, "access", return_value=False), \
             mock.patch.object(ms.Path, "exists", return_value=False), \
             mock.patch.object(ms.Path, "glob", lambda self, pattern: iter([])):
            self.assertIsNone(ms.pick_python())

    def test_seed_prefetch_runs_apples_python_for_local_network_privacy(self) -> None:
        agents = self.home / "Library/LaunchAgents"
        for apple, wanted in (("/usr/bin/python3", "/usr/bin/python3"), (None, "/opt/x/python3")):
            with self.subTest(apple=apple), mock.patch.object(ms, "apple_python", return_value=apple):
                self.invoke("--apply", "--python", "/opt/x/python3")
                seed = plistlib.loads((agents / "com.teamleaderleo.glaeda.seed-prefetch.plist").read_bytes())
                disk = plistlib.loads((agents / "com.teamleaderleo.glaeda.disk-pressure.plist").read_bytes())
                self.assertEqual(seed["ProgramArguments"][0], wanted)
                self.assertEqual(disk["ProgramArguments"][0], "/opt/x/python3")
        self.assertIsNone(ms.apple_python("linux"))

    def test_the_lan_fetch_broker_runs_apples_python_and_asks_for_a_root_owned_copy(self) -> None:
        with mock.patch.object(ms, "apple_python", return_value="/usr/bin/python3"):
            self.invoke("--apply")
        agent = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.lan-fetch.plist").read_bytes())
        self.assertEqual(agent["ProgramArguments"],
                         ["/usr/bin/python3", "-I", os.fspath(self.home / ".local/bin/glaeda-lan-fetch"), "serve-local"])
        self.assertTrue(agent["KeepAlive"])
        self.assertEqual((self.home / ".local/bin/glaeda-lan-fetch").read_bytes(),
                         (ROOT / "scripts/glaeda-lan-fetch").read_bytes())
        missing = os.fspath(self.home / "no-such-dir/glaeda-lan-fetch")
        with mock.patch.object(ms, "LAN_FETCH_ROOT_COPY", missing):
            steps = ms.operator_steps(self.ctx_for_steps(), fake_preflight()(None), None)
        step = next(s for s in steps if "helper-install" in s["command"])
        self.assertIn("non-cmux admin account", step["needs"])
        self.assertRegex(step["command"], r"^scripts/glaeda-seed-lan helper-install \S+ --apply$")
        self.assertEqual(ms.LAN_FETCH_ROOT_COPY, "/Library/Application Support/glaeda/bin/glaeda-lan-fetch")
        # A copy in a directory the fleet user owns (here, the test's) still gets the step.
        own = self.home / "own-bin/glaeda-lan-fetch"
        own.parent.mkdir()
        own.write_bytes((ROOT / "scripts/glaeda-lan-fetch").read_bytes())
        self.assertFalse(ms.root_chain(own))
        with mock.patch.object(ms, "LAN_FETCH_ROOT_COPY", os.fspath(own)):
            steps = ms.operator_steps(self.ctx_for_steps(), fake_preflight()(None), None)
        self.assertTrue(any("helper-install" in s["command"] for s in steps))

    def ctx_for_steps(self):
        return ms.Context(self.home, False, "/usr/bin/python3", True, None, None, None, ms.CMUX_XCODE_APP, 50)

    def test_apple_python_falls_back_when_it_does_not_run(self) -> None:
        def fake_glob(self, pattern):
            return iter([Path("/Applications/Xcode_26.6.app")]) if pattern == "Xcode*.app" else iter([])
        runs = []

        def fake_run(argv, **kwargs):
            runs.append(argv)
            return subprocess.CompletedProcess(argv, fake_run.code, b"", b"")

        with mock.patch.object(ms.Path, "glob", fake_glob), mock.patch.object(ms.subprocess, "run", fake_run):
            fake_run.code = 0
            ms.apple_python.cache_clear()
            self.assertEqual(ms.apple_python("macos"), "/usr/bin/python3")
            fake_run.code = 69  # xcrun: Xcode licence not accepted
            ms.apple_python.cache_clear()
            self.assertIsNone(ms.apple_python("macos"))
        with mock.patch.object(ms.Path, "glob", fake_glob), \
             mock.patch.object(ms.subprocess, "run", side_effect=subprocess.TimeoutExpired("python3", 10)):
            ms.apple_python.cache_clear()
            self.assertIsNone(ms.apple_python("macos"))
        ms.apple_python.cache_clear()
        self.assertEqual(runs[0][:3], ["/usr/bin/python3", "-I", "-c"])

    def test_default_xcode_pin_matches_the_cmux_pull_request_lane(self) -> None:
        self.assertEqual(ms.CMUX_XCODE_APP, "/Applications/Xcode_26.6.app")

    def test_plan_is_default_and_writes_nothing(self) -> None:
        receipt = self.invoke()
        self.assertFalse(receipt["applied"])
        self.assertEqual(self.tree(), {})
        self.assertIn("create", self.states(receipt))
        self.assertTrue(receipt["sandboxHome"])

    def test_apply_installs_templated_for_user_then_is_idempotent(self) -> None:
        first = self.invoke("--apply")
        self.assertTrue(first["applied"])
        bin_dir = self.home / ".local/bin"
        for name in ("glaeda-disk", "glaeda-worktree-reclaim", "glaeda-worktree-reclaim-all"):
            self.assertTrue(os.access(bin_dir / name, os.X_OK), name)
        self.assertEqual((bin_dir / "glaeda-disk").read_bytes(), (ROOT / "scripts/glaeda-disk").read_bytes())
        for label in ("disk-pressure", "disk-dedupe", "worktree-reclaim", "fleet-cas-prune", "seed-prefetch"):
            path = self.home / f"Library/LaunchAgents/com.teamleaderleo.glaeda.{label}.plist"
            raw = path.read_bytes()
            self.assertNotIn(b"/Users/leoli", raw)
            doc = plistlib.loads(raw)
            self.assertEqual(doc["Label"], f"com.teamleaderleo.glaeda.{label}")
            for arg in doc["ProgramArguments"][1:2]:
                self.assertTrue(arg.startswith(os.fspath(bin_dir)), arg)
            self.assertTrue(doc["StandardOutPath"].startswith(os.fspath(self.home / "Library/Logs")))
            # dedupe is never urgent; pressure must still free space while a build fills the disk
            self.assertEqual(doc.get("ProcessType"),
                             "Background" if label in ("disk-dedupe", "fleet-cas-prune", "seed-prefetch") else None)
        warm = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.idle-warm.plist").read_bytes())
        self.assertEqual(warm["ProgramArguments"][1:], [os.fspath(bin_dir / "glaeda-idle-warm"), "--apply"])
        self.assertIsNone(warm.get("ProcessType"), "its build runs only while the mini is idle and should be fast")
        self.assertEqual((bin_dir / "glaeda-idle-warm").read_bytes(), (ROOT / "scripts/glaeda-idle-warm").read_bytes())
        health = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.mini-health.plist").read_bytes())
        self.assertEqual(health["ProgramArguments"][1:], [os.fspath(bin_dir / "glaeda-mini-health"), "--apply"])
        self.assertEqual(health["StartInterval"], 120)
        self.assertEqual((bin_dir / "glaeda-mini-health").read_bytes(), (ROOT / "scripts/glaeda-mini-health").read_bytes())
        gh = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.gh-watch.plist").read_bytes())
        self.assertEqual(gh["ProgramArguments"][1:], [os.fspath(bin_dir / "glaeda-gh"), "serve"])
        self.assertTrue(gh["KeepAlive"])
        self.assertIn("/opt/homebrew/bin", gh["EnvironmentVariables"]["PATH"])  # gh supplies the token
        self.assertTrue(os.access(bin_dir / "glaeda-gh", os.X_OK))
        skill = self.home / ".claude/skills/github-ci-wait/SKILL.md"
        self.assertEqual(skill.read_bytes(), (ROOT / "skills/github-ci-wait/SKILL.md").read_bytes())
        self.assertEqual(skill.stat().st_mode & 0o777, 0o644)
        dedupe = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.disk-dedupe.plist").read_bytes())
        self.assertEqual(dedupe["WatchPaths"], [os.fspath(self.home / "Library/Developer/Xcode/DerivedData")])
        self.assertTrue((self.home / "Library/Developer/Xcode/DerivedData").is_dir())
        self.assertEqual((self.home / ".cache/glaeda/cmux-native-cache").stat().st_mode & 0o777, 0o700)
        receipt_path = self.home / ".local/state/glaeda/mini-setup/receipt.json"
        self.assertEqual(receipt_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(json.loads(receipt_path.read_text())["schema"], "glaeda-mini-setup/v1")

        before = self.tree()
        second = self.invoke("--apply")
        self.assertEqual(self.states(second), {"unchanged"})
        after = self.tree()
        after.pop(".local/state/glaeda/mini-setup/receipt.json")
        before.pop(".local/state/glaeda/mini-setup/receipt.json")
        self.assertEqual(before, after)

    def test_hand_written_plist_with_same_content_is_unchanged(self) -> None:
        self.invoke("--apply")
        path = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.disk-pressure.plist"
        path.write_bytes(plistlib.dumps(plistlib.loads(path.read_bytes()), fmt=plistlib.FMT_XML, sort_keys=False))
        path.write_text(path.read_text().replace("<dict>", "<dict>\n  <!-- hand written -->", 1))
        receipt = self.invoke()
        state = {a.get("label"): a["state"] for a in receipt["actions"] if a["kind"] == "agent"}
        self.assertEqual(state["com.teamleaderleo.glaeda.disk-pressure"], "unchanged")

    def test_update_keeps_a_backup(self) -> None:
        (self.home / ".local/bin").mkdir(parents=True)
        old = self.home / ".local/bin/glaeda-disk"
        old.write_text("#!/bin/sh\necho old\n")
        receipt = self.invoke("--apply")
        act = next(a for a in receipt["actions"] if a.get("path") == os.fspath(old))
        self.assertEqual(act["state"], "update")
        self.assertEqual(Path(act["backup"]).read_text(), "#!/bin/sh\necho old\n")

    def test_git_defaults_only_fill_unset_keys_and_uninstall_only_ours(self) -> None:
        (self.home / ".gitconfig").write_text("[fetch]\n\tparallel = 4\n")
        receipt = self.invoke("--apply")
        git = {a["key"]: a for a in receipt["actions"] if a["kind"] == "git"}
        self.assertEqual(git["fetch.parallel"]["state"], "kept")
        self.assertEqual(git["core.untrackedCache"]["state"], "create")
        self.invoke("--apply")  # the second run must not forget what the first one set
        self.invoke("--uninstall", "--apply")
        text = (self.home / ".gitconfig").read_text()
        self.assertIn("parallel = 4", text)
        self.assertNotIn("untrackedCache", text)
        self.assertNotIn("fetchJobs", text)

    def test_uninstall_plan_then_apply_removes_only_unmodified_files(self) -> None:
        self.invoke("--apply")
        edited = self.home / ".local/bin/glaeda-worktree-reclaim-all"
        edited.write_text(edited.read_text() + "# operator edit\n")
        before = self.tree()
        plan = self.invoke("--uninstall")
        self.assertEqual(before, self.tree())
        by_path = {a.get("path"): a["state"] for a in plan["actions"]}
        self.assertEqual(by_path[os.fspath(edited)], "kept")
        self.assertEqual(by_path[os.fspath(self.home / ".local/bin/glaeda-disk")], "remove")
        self.invoke("--uninstall", "--apply")
        self.assertTrue(edited.exists())
        self.assertFalse((self.home / ".local/bin/glaeda-disk").exists())
        self.assertEqual(list((self.home / "Library/LaunchAgents").iterdir()), [])
        self.assertTrue((self.home / ".local/state/glaeda/mini-setup/uninstall-receipt.json").is_file())

    def test_nothing_runs_sudo_and_privileged_steps_are_operator_steps(self) -> None:
        with mock.patch.object(ms, "preflight", fake_preflight(
                power={"readable": True, "acSleepDisabled": False, "wakeOnNetwork": False, "autoRestart": False})):
            receipt = self.invoke("--apply")
        self.assertFalse(any("sudo" in argv for argv in self.calls))
        commands = [s["command"] for s in receipt["operatorSteps"]]
        self.assertIn("sudo pmset -c sleep 0", commands)
        self.assertIn("sudo pmset -c womp 1", commands)
        self.assertIn("sudo pmset -a autorestart 1", commands)
        self.assertTrue(any("glaeda-cmux-runner --apply" in c for c in commands))
        self.assertFalse(any("persistent-compile" in c for c in commands))
        source = (ROOT / "scripts/glaeda-mini-setup").read_text()
        self.assertIsNone(re.search(r'\[\s*"sudo"', source))

    def test_reserved_slots_register_nothing(self) -> None:
        receipt = self.invoke("--apply")
        self.assertEqual(receipt["reserved"]["compilationCacheNode"]["state"], "reserved")
        self.assertEqual(receipt["reserved"]["runnerRegistration"]["state"], "not_performed")
        self.assertEqual(receipt["reserved"]["fleetEnrollment"]["state"], "not_run")

    def test_missing_reclaim_source_blocks_that_action_only(self) -> None:
        self.reclaim.unlink()
        receipt = self.invoke()
        act = next(a for a in receipt["actions"] if a.get("path", "").endswith("/glaeda-worktree-reclaim"))
        self.assertEqual(act["state"], "blocked")
        self.assertFalse(receipt["ready"])

    def test_uninstall_without_receipt_removes_nothing(self) -> None:
        self.invoke("--apply")
        (self.home / ".local/state/glaeda/mini-setup/receipt.json").unlink()
        before = self.tree()
        plan = self.invoke("--uninstall", "--apply")
        self.assertEqual({a["state"] for a in plan["actions"] if a["kind"] in {"tool", "agent", "git"}} - {"absent"}, {"kept"})
        self.assertTrue(any("nothing is removed" in a.get("note", "") for a in plan["actions"]))
        after = self.tree()
        after.pop(".local/state/glaeda/mini-setup/uninstall-receipt.json")
        self.assertEqual(before, after)

    def test_preexisting_identical_file_is_unchanged_and_never_removed(self) -> None:
        (self.home / ".local/bin").mkdir(parents=True)
        disk = self.home / ".local/bin/glaeda-disk"
        disk.write_bytes((ROOT / "scripts/glaeda-disk").read_bytes())
        disk.chmod(0o755)
        first = self.invoke("--apply")
        act = next(a for a in first["actions"] if a.get("path") == os.fspath(disk))
        self.assertEqual((act["state"], act["owned"]), ("unchanged", False))
        self.invoke("--apply")
        plan = self.invoke("--uninstall", "--apply")
        by_path = {a.get("path"): a["state"] for a in plan["actions"]}
        self.assertEqual(by_path[os.fspath(disk)], "kept")
        self.assertTrue(disk.exists())
        self.assertFalse((self.home / ".local/bin/glaeda-worktree-reclaim-all").exists())

    def test_blocked_rerun_keeps_ownership_of_earlier_install(self) -> None:
        self.invoke("--apply")
        self.reclaim.unlink()  # the re-run cannot find --reclaim-binary
        rerun = self.invoke("--apply")
        by_path = {a.get("path"): a for a in rerun["actions"]}
        binary = self.home / ".local/bin/glaeda-worktree-reclaim"
        agent = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.worktree-reclaim.plist"
        for path in (binary, agent):
            self.assertEqual(by_path[os.fspath(path)]["state"], "blocked")
            self.assertTrue(by_path[os.fspath(path)]["owned"], path)
        self.invoke("--apply")  # a second blocked run must not lose it either
        plan = self.invoke("--uninstall", "--apply")
        states = {a.get("path"): a["state"] for a in plan["actions"]}
        self.assertEqual(states[os.fspath(binary)], "remove")
        self.assertEqual(states[os.fspath(agent)], "remove")
        self.assertFalse(binary.exists())
        self.assertFalse(agent.exists())

    def test_reclaim_agent_blocked_without_binary(self) -> None:
        self.reclaim.unlink()
        receipt = self.invoke("--apply")
        agent = next(a for a in receipt["actions"] if a.get("label") == "com.teamleaderleo.glaeda.worktree-reclaim")
        self.assertEqual(agent["state"], "blocked")
        self.assertFalse((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.worktree-reclaim.plist").exists())

    def test_cargo_build_skipped_when_binary_unchanged(self) -> None:
        target = Path(self.tmp.name) / "target"
        (target / "release").mkdir(parents=True)
        (target / "release/glaeda-worktree-reclaim").write_bytes(self.reclaim.read_bytes())
        (self.home / ".local/bin").mkdir(parents=True)
        installed = self.home / ".local/bin/glaeda-worktree-reclaim"
        installed.write_bytes(self.reclaim.read_bytes())
        installed.chmod(0o755)
        builds: list[int] = []
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": os.fspath(target)}), \
                mock.patch.object(ms, "find_cargo", lambda ctx: "/bin/false"), \
                mock.patch.object(ms, "build_reclaim", lambda ctx: builds.append(1) or (1, "")):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(ms.main(["--output", "json", "--python", "/usr/bin/python3", "--apply"]), 0)
        self.assertEqual(builds, [])
        act = next(a for a in json.loads(out.getvalue())["actions"] if a.get("path") == os.fspath(installed))
        self.assertEqual(act["state"], "unchanged")

    def test_fleet_bootstrap_non_json_is_recorded(self) -> None:
        glaeda = Path(self.tmp.name) / "glaeda"
        glaeda.write_text("")
        (self.home / ".cache/glaeda/cmux-native-cache").mkdir(parents=True)
        done = mock.Mock(returncode=0, stdout="warning: not json\n", stderr="")
        with mock.patch.object(ms.subprocess, "run", return_value=done):
            receipt = self.invoke("--cmux-root", self.tmp.name, "--glaeda", os.fspath(glaeda))
        slot = receipt["reserved"]["fleetEnrollment"]
        self.assertEqual(slot["state"], "unparsed")
        self.assertIn("not json", slot["raw"])

    def test_bootout_waits_and_bootstrap_retries_eio(self) -> None:
        calls: list[tuple[str, ...]] = []
        answers = {"print": [(0, ""), (0, ""), (113, "")], "bootstrap": [(5, "Bootstrap failed: 5: Input/output error"), (0, "")]}

        def fake(ctx, *args):
            calls.append(args)
            queue = answers.get(args[0])
            return queue.pop(0) if queue else (0, "")

        ctx = mock.Mock(uid=501)
        with mock.patch.object(ms, "launchctl", fake), mock.patch.object(ms.time, "sleep", lambda s: None):
            self.assertTrue(ms.bootout(ctx, "x"))
            self.assertEqual(ms.bootstrap(ctx, "/p.plist")[0], 0)
        self.assertEqual([c[0] for c in calls], ["bootout", "print", "print", "print", "bootstrap", "bootstrap"])

    def test_power_parser_reuses_fleet_rule(self) -> None:
        with mock.patch.object(ms, "run", lambda argv, **kw: (0, PMSET_SLEEPY)):
            self.assertEqual(ms.power(), {"readable": True, "acSleepDisabled": False,
                                          "wakeOnNetwork": False, "autoRestart": False})
        with mock.patch.object(ms, "run", lambda argv, **kw: (0, PMSET_READY)):
            self.assertEqual(ms.power(), {"readable": True, "acSleepDisabled": True,
                                          "wakeOnNetwork": True, "autoRestart": True})

    def test_hygiene_only_installs_tools_and_agents_on_any_mac(self) -> None:
        self.patches[0].stop()
        pre = fake_preflight()(None)
        pre["checks"]["diskFree"] = ms.check(False, "40 GiB", "need 120 GiB", True)
        pre["blocking"] = ["diskFree"]
        self.patches[0] = mock.patch.object(ms, "preflight", lambda ctx: pre)
        self.patches[0].start()
        receipt = self.invoke("--hygiene-only", "--apply")
        self.assertEqual(receipt["profile"], "hygiene")
        self.assertTrue(receipt["ready"], receipt["blocking"])
        self.assertEqual(receipt["preflight"]["checks"]["diskFree"]["level"], "warn")
        self.assertEqual(receipt["operatorSteps"], [])
        self.assertEqual(receipt["reserved"], {})
        kinds = {a["kind"] for a in receipt["actions"]}
        self.assertNotIn("git", kinds)
        self.assertNotIn("python", kinds)
        self.assertFalse((self.home / ".cache/glaeda/cmux-native-cache").exists())
        for name in ("glaeda-disk", "glaeda-worktree-reclaim", "glaeda-worktree-reclaim-all"):
            self.assertTrue(os.access(self.home / ".local/bin" / name, os.X_OK), name)
        for label in ("disk-pressure", "disk-dedupe", "worktree-reclaim"):
            self.assertTrue((self.home / f"Library/LaunchAgents/com.teamleaderleo.glaeda.{label}.plist").is_file())
        # the build-host profile still blocks on the same machine
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            ms.main(["--output", "json", "--python", "/usr/bin/python3",
                     "--reclaim-binary", os.fspath(self.reclaim)])
        self.assertIn("diskFree", json.loads(out.getvalue())["blocking"])

    def test_hygiene_only_still_blocks_without_python(self) -> None:
        self.patches[0].stop()
        pre = fake_preflight()(None)
        pre["checks"]["python"] = ms.check(False, "missing", "", True)
        pre["blocking"] = ["python"]
        self.patches[0] = mock.patch.object(ms, "preflight", lambda ctx: pre)
        self.patches[0].start()
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            ms.main(["--output", "json", "--python", "/usr/bin/python3",
                     "--reclaim-binary", os.fspath(self.reclaim), "--hygiene-only"])
        self.assertEqual(json.loads(out.getvalue())["blocking"], ["python"])

    def python_action(self, receipt: dict) -> dict:
        return next(a for a in receipt["actions"] if a["kind"] == "python")

    def test_enroll_python_present_is_unchanged(self) -> None:
        act = self.python_action(self.invoke())
        self.assertEqual((act["state"], act["value"]), ("unchanged", "/opt/homebrew/bin/python3.13"))

    def test_missing_enroll_python_blocks_with_the_fix_when_brew_is_not_ours(self) -> None:
        with mock.patch.object(ms, "enroll_python", lambda ctx: None), \
                mock.patch.object(ms, "owned_brew", lambda: None):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(ms.main(["--output", "json", "--python", "/usr/bin/python3",
                                          "--reclaim-binary", os.fspath(self.reclaim), "--apply"]), 0)
        receipt = json.loads(out.getvalue())
        act = self.python_action(receipt)
        self.assertEqual(act["state"], "blocked")
        self.assertIn("~/.local/bin/python3", act["note"])
        self.assertFalse(receipt["ready"])
        self.assertIn("action:enroll-python", receipt["blocking"])
        self.assertTrue(any("~/.local/bin/python3" in s["command"] for s in receipt["operatorSteps"]))
        ms.brew_install.assert_not_called()

    def test_missing_enroll_python_is_installed_with_our_brew(self) -> None:
        found = iter([None, "/opt/homebrew/bin/python3.13"])
        ms.brew_install.side_effect = None
        ms.brew_install.return_value = (0, "installed")
        with mock.patch.object(ms, "enroll_python", lambda ctx: next(found)), \
                mock.patch.object(ms, "owned_brew", lambda: "/opt/homebrew/bin/brew"), \
                mock.patch.object(ms, "brew_allowed", lambda ctx: True):
            receipt = self.invoke("--apply")
        ms.brew_install.assert_called_once_with("/opt/homebrew/bin/brew", "python@3.13")
        act = self.python_action(receipt)
        self.assertEqual((act["state"], act["applied"], act["value"]), ("create", True, "/opt/homebrew/bin/python3.13"))
        self.assertTrue(receipt["ready"], receipt["blocking"])

    def test_plan_only_names_the_brew_install(self) -> None:
        with mock.patch.object(ms, "enroll_python", lambda ctx: None), \
                mock.patch.object(ms, "owned_brew", lambda: "/opt/homebrew/bin/brew"), \
                mock.patch.object(ms, "brew_allowed", lambda ctx: True):
            receipt = self.invoke()
        self.assertEqual(self.python_action(receipt)["state"], "create")
        ms.brew_install.assert_not_called()

    def test_failed_brew_install_fails_the_run(self) -> None:
        ms.brew_install.side_effect = None
        ms.brew_install.return_value = (1, "Error: permission denied")
        with mock.patch.object(ms, "enroll_python", lambda ctx: None), \
                mock.patch.object(ms, "owned_brew", lambda: "/opt/homebrew/bin/brew"), \
                mock.patch.object(ms, "brew_allowed", lambda ctx: True):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = ms.main(["--output", "json", "--python", "/usr/bin/python3",
                                "--reclaim-binary", os.fspath(self.reclaim), "--apply"])
        self.assertEqual(code, 1)
        act = self.python_action(json.loads(out.getvalue()))
        self.assertEqual(act["state"], "failed")
        self.assertIn("permission denied", act["note"])

    def test_sandbox_home_never_runs_brew(self) -> None:
        with mock.patch.object(ms, "enroll_python", lambda ctx: None), \
                mock.patch.object(ms, "owned_brew", lambda: "/opt/homebrew/bin/brew"):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                ms.main(["--output", "json", "--python", "/usr/bin/python3",
                         "--reclaim-binary", os.fspath(self.reclaim), "--apply"])
        act = self.python_action(json.loads(out.getvalue()))
        self.assertEqual(act["state"], "blocked")
        self.assertIn("sandbox HOME", act["note"])
        ms.brew_install.assert_not_called()

    def test_owned_brew_needs_ownership_not_write_access(self) -> None:
        stat = mock.Mock(st_uid=os.getuid() + 1)
        with mock.patch.object(ms, "which", return_value="/opt/homebrew/bin/brew"), \
                mock.patch.object(ms.os, "access", return_value=True), \
                mock.patch.object(ms.Path, "stat", return_value=stat):
            self.assertIsNone(ms.owned_brew())
            stat.st_uid = os.getuid()
            self.assertEqual(ms.owned_brew(), "/opt/homebrew/bin/brew")

    def test_setup_and_enroll_share_one_python_rule(self) -> None:
        enroll = ms.enroll_module()
        with mock.patch.object(enroll, "pick_python", return_value="/x/python3.13") as pick:
            self.assertEqual(REAL_ENROLL_PYTHON(mock.Mock(home=self.home)), "/x/python3.13")
        pick.assert_called_once_with(None, self.home)

    def linux(self) -> None:
        """Run the rest of the test as a Linux host with a stub preflight."""
        platform_patch = mock.patch.object(ms, "HOST_PLATFORM", "linux")
        platform_patch.start()
        self.patches.append(platform_patch)

    def test_linux_installs_the_ops_systemd_units_verbatim_then_is_idempotent(self) -> None:
        self.linux()
        receipt = self.invoke("--apply")  # Linux gets the hygiene profile without the flag
        self.assertEqual((receipt["platform"], receipt["profile"]), ("linux", "hygiene"))
        self.assertTrue(receipt["ready"], receipt["blocking"])
        units = self.home / ".config/systemd/user"
        self.assertEqual(sorted(p.name for p in units.iterdir()), sorted(ms.LINUX_UNITS))
        for name in ms.LINUX_UNITS:
            self.assertEqual((units / name).read_bytes(), (ROOT / "ops/systemd" / name).read_bytes(), name)
        for name in ("glaeda-disk", "glaeda-worktree-reclaim", "glaeda-worktree-reclaim-all"):
            self.assertTrue(os.access(self.home / ".local/bin" / name, os.X_OK), name)
        self.assertTrue((self.home / ".local/state").is_dir())
        self.assertFalse((self.home / "Library").exists())
        kinds = {a["kind"] for a in receipt["actions"]}
        self.assertNotIn("git", kinds)
        self.assertNotIn("python", kinds)
        self.assertEqual(receipt["operatorSteps"], [])
        again = self.invoke("--apply")
        self.assertEqual(self.states(again), {"unchanged"})
        self.assertFalse(any("systemctl" in os.path.basename(c[0]) for c in self.calls))

    def test_linux_uninstall_removes_only_our_units(self) -> None:
        self.linux()
        self.invoke("--apply")
        units = self.home / ".config/systemd/user"
        (units / "glaeda-disk-pressure.timer").write_text("[Timer]\nOnCalendar=daily\n")
        receipt = self.invoke("--uninstall", "--apply")
        by_path = {Path(a["path"]).name: a["state"] for a in receipt["actions"] if a["kind"] == "agent"}
        self.assertEqual(by_path["glaeda-disk-pressure.timer"], "kept")
        self.assertEqual(by_path["glaeda-worktree-reclaim.timer"], "remove")
        self.assertEqual([p.name for p in units.iterdir()], ["glaeda-disk-pressure.timer"])

    def test_linux_reclaim_units_block_without_a_reclaim_binary(self) -> None:
        self.linux()
        out = io.StringIO()
        with mock.patch.object(ms, "find_cargo", lambda ctx: None), contextlib.redirect_stdout(out):
            ms.main(["--output", "json", "--python", "/usr/bin/python3", "--apply"])
        receipt = json.loads(out.getvalue())
        states = {a.get("label"): a["state"] for a in receipt["actions"] if a["kind"] == "agent"}
        self.assertEqual(states["glaeda-worktree-reclaim.timer"], "blocked")
        self.assertEqual(states["glaeda-disk-pressure.timer"], "create")

    def test_linux_activation_reloads_then_enables_or_restarts_timers(self) -> None:
        self.linux()
        ctx = ms.Context(self.home, True, "/usr/bin/python3", False, self.reclaim, None, None,
                         ms.CMUX_XCODE_APP, 1, True, "linux")
        ctx.skip_launchctl = False  # the real path, with systemctl stubbed below
        commands: list[list[str]] = []

        def systemctl_run(argv, *args, **kwargs):
            commands.append(argv[1:])
            return 0, ""

        def agent(label, state, applied, loaded):
            return {"kind": "agent", "label": label, "state": state, "applied": applied, "loaded": loaded}

        actions = [
            agent("glaeda-disk-pressure.service", "unchanged", False, None),
            agent("glaeda-disk-pressure.timer", "unchanged", False, True),  # running, untouched: left alone
            agent("glaeda-worktree-reclaim.service", "update", True, None),
            agent("glaeda-worktree-reclaim.timer", "unchanged", False, True),  # its service changed
        ]
        with mock.patch.object(ms, "run", systemctl_run), mock.patch.object(ms, "systemctl", lambda: "systemctl"):
            ms.activate_systemd(ctx, actions)
        self.assertEqual(commands, [
            ["--user", "daemon-reload"],
            ["--user", "restart", "glaeda-worktree-reclaim.timer"],
            ["--user", "enable", "glaeda-worktree-reclaim.timer"],
        ])
        commands.clear()
        fresh = [agent("glaeda-disk-pressure.timer", "create", True, False),
                 agent("glaeda-worktree-reclaim.timer", "blocked", False, False)]
        with mock.patch.object(ms, "run", systemctl_run), mock.patch.object(ms, "systemctl", lambda: "systemctl"):
            ms.activate_systemd(ctx, fresh)
        self.assertEqual(commands, [["--user", "daemon-reload"],
                                    ["--user", "enable", "--now", "glaeda-disk-pressure.timer"]])
        commands.clear()
        # the long-running glaeda-gh service has no timer: it is enabled and started itself
        with mock.patch.object(ms, "run", systemctl_run), mock.patch.object(ms, "systemctl", lambda: "systemctl"):
            ms.activate_systemd(ctx, [agent("glaeda-gh.service", "create", True, False)])
        self.assertEqual(commands, [["--user", "daemon-reload"], ["--user", "enable", "--now", "glaeda-gh.service"]])

    def test_linux_activation_failure_fails_the_run(self) -> None:
        self.linux()
        ctx = ms.Context(self.home, True, "/usr/bin/python3", False, self.reclaim, None, None,
                         ms.CMUX_XCODE_APP, 1, True, "linux")
        ctx.skip_launchctl = False

        def refusing_run(argv, *args, **kwargs):
            return (1, "Failed to enable unit") if "enable" in argv else (0, "")

        blocked = {"kind": "agent", "label": "glaeda-worktree-reclaim.timer", "state": "blocked",
                   "applied": False, "loaded": False, "note": "glaeda-worktree-reclaim could not be installed"}
        timer = {"kind": "agent", "label": "glaeda-disk-pressure.timer", "state": "create",
                 "applied": True, "loaded": False}
        with mock.patch.object(ms, "run", refusing_run):
            ms.activate_systemd(ctx, [timer, blocked])
        self.assertEqual(timer["state"], "failed")
        self.assertIn("enable --now failed", timer["note"])
        self.assertEqual(blocked["note"], "glaeda-worktree-reclaim could not be installed")

    def test_linux_uninstall_disables_timers_whatever_their_state(self) -> None:
        self.linux()
        self.invoke("--apply")
        ctx = ms.Context(self.home, True, "/usr/bin/python3", False, self.reclaim, None, None,
                         ms.CMUX_XCODE_APP, 1, True, "linux")
        ctx.skip_launchctl = False
        commands: list[list[str]] = []

        def systemctl_run(argv, *args, **kwargs):
            commands.append(argv[1:])
            return 1, ""  # is-enabled/is-active: neither; removal must still disable and stop

        with mock.patch.object(ms, "run", systemctl_run), mock.patch.object(ms, "systemctl", lambda: "systemctl"):
            actions = ms.plan_uninstall(ctx)
            commands.clear()
            ms.apply_uninstall(ctx, actions)
        self.assertIn(["--user", "disable", "--now", "glaeda-worktree-reclaim.timer"], commands)
        self.assertIn(["--user", "stop", "glaeda-worktree-reclaim.service"], commands)
        self.assertIn(["--user", "disable", "--now", "glaeda-gh.service"], commands)
        self.assertEqual(commands[-1], ["--user", "daemon-reload"])

    def test_ota_updater_installed_and_ring_written_once(self) -> None:
        receipt = self.invoke("--hygiene-only", "--apply")
        self.assertTrue(os.access(self.home / ".local/bin/glaeda-update", os.X_OK))
        plist = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.update.plist").read_bytes())
        self.assertEqual(plist["ProgramArguments"][1:], [os.fspath(self.home / ".local/bin/glaeda-update"), "--apply"])
        self.assertEqual(plist["ProcessType"], "Background")
        path = self.home / ".config/glaeda/update.json"
        config = json.loads(path.read_text())
        self.assertEqual((config["ring"], config["setupArgs"], config["reportStatus"]),
                         ("stable", ["--hygiene-only"], False))
        self.assertEqual(next(a for a in receipt["actions"] if a["kind"] == "config")["state"], "create")
        # a later run leaves a hand-edited config alone, unless --ota-ring names another ring
        path.write_text(json.dumps({**config, "host": "renamed"}))
        self.assertEqual(self.states(self.invoke("--hygiene-only", "--apply")), {"unchanged"})
        self.invoke("--hygiene-only", "--apply", "--ota-ring", "canary")
        config = json.loads(path.read_text())
        self.assertEqual((config["ring"], config["host"], config["reportStatus"]), ("canary", "renamed", True))
        # uninstall removes the updater it installed
        self.invoke("--uninstall", "--apply")
        self.assertFalse((self.home / ".local/bin/glaeda-update").exists())

    def test_update_agent_is_not_unloaded_under_glaeda_update(self) -> None:
        ctx = ms.Context(self.home, True, "/usr/bin/python3", False, self.reclaim, None, None,
                         ms.CMUX_XCODE_APP, 50, hygiene_only=True)
        ctx.sandbox = False
        ctx.skip_launchctl = False
        label = ms.LABEL_PREFIX + "update"
        path = ms.agent_path(ctx, label)
        path.parent.mkdir(parents=True)
        path.write_bytes(b"old")
        act = {"kind": "agent", "label": label, "path": os.fspath(path), "state": "update", "loaded": True,
               "mode": "0o644", "sha256": "x", "owned": False}
        with mock.patch.object(ms, "bootout", mock.Mock(side_effect=AssertionError("must not unload"))), \
                mock.patch.object(ms, "bootstrap", mock.Mock(side_effect=AssertionError("must not reload"))), \
                mock.patch.dict(os.environ, {ms.UPDATE_ENV: "1"}):
            ms.apply_install(ctx, [act])
        self.assertEqual(path.read_bytes(), ms.agent_bytes(ctx, label))
        self.assertIn("deferred", act["note"])

    def test_git_below_2_55_is_installed_with_our_brew_or_handed_to_the_operator(self) -> None:
        ctx = ms.Context(self.home, False, "/usr/bin/python3", True, self.reclaim, None, None,
                         ms.CMUX_XCODE_APP, 50, hygiene_only=True)
        with mock.patch.object(ms, "newest_git", lambda: ("/usr/bin/git", (2, 55, 0))):
            self.assertEqual(ms.plan_git(ctx)["state"], "unchanged")
        old = lambda: ("/usr/bin/git", (2, 50, 1))
        with mock.patch.object(ms, "newest_git", old), mock.patch.object(ms, "owned_brew", lambda: "/opt/homebrew/bin/brew"):
            self.assertEqual(ms.plan_git(ctx)["state"], "blocked")  # sandbox HOME never runs brew
            ctx.sandbox = False
            act = ms.plan_git(ctx)
            self.assertEqual((act["state"], act["brew"]), ("create", "/opt/homebrew/bin/brew"))
        with mock.patch.object(ms, "newest_git", old), mock.patch.object(ms, "owned_brew", lambda: None):
            self.assertIn("Homebrew owner", ms.plan_git(ctx)["note"])
            linux = ms.Context(self.home, False, "/usr/bin/python3", True, self.reclaim, None, None,
                               ms.CMUX_XCODE_APP, 50, hygiene_only=True, platform_name="linux")
            self.assertIn("ppa:git-core/ppa", ms.plan_git(linux)["note"])

    def test_brew_git_apply_upgrades_an_outdated_keg(self) -> None:
        ctx = ms.Context(self.home, True, "/usr/bin/python3", True, self.reclaim, None, None,
                         ms.CMUX_XCODE_APP, 50, hygiene_only=True)
        versions = iter([(2, 50, 1), (2, 55, 0)])  # install leaves the old keg; upgrade fixes it
        act = {"kind": "gitpkg", "state": "create", "brew": "/opt/homebrew/bin/brew", "note": ""}
        calls = []

        def fake_brew(brew, formula, verb="install", timeout=0, no_auto_update=False):
            calls.append((verb, timeout, no_auto_update))
            return 0, "already installed"

        with mock.patch.object(ms, "brew_install", fake_brew), \
                mock.patch.object(ms, "newest_git", lambda: ("/opt/homebrew/bin/git", next(versions))):
            ms.apply_install(ctx, [act])
        self.assertTrue(act["applied"])
        self.assertEqual(calls, [("install", ms.GIT_BREW_TIMEOUT, True), ("upgrade", ms.GIT_BREW_TIMEOUT, True)])
        self.assertLess(2 * ms.GIT_BREW_TIMEOUT, 1800)  # inside glaeda-update's wait for setup
        self.assertEqual(act["value"], "2.55.0")

    def test_failed_git_install_does_not_fail_the_run(self) -> None:
        failing = {"kind": "gitpkg", "key": "git>=2.55", "state": "failed", "note": "brew exited 1"}
        with mock.patch.object(ms, "plan_git", lambda ctx: dict(failing)):
            receipt = self.invoke("--hygiene-only", "--apply")  # invoke asserts exit 0
        self.assertIn("action:git>=2.55", receipt["blocking"])

    def test_no_em_dashes(self) -> None:
        for name in ("glaeda-mini-setup", "glaeda-worktree-reclaim-all", "test-glaeda-mini-setup.py"):
            self.assertNotIn(chr(0x2014), (ROOT / "scripts" / name).read_text(), name)


if __name__ == "__main__":
    unittest.main()
