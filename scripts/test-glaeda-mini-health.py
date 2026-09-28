#!/usr/bin/env python3
"""Tests for scripts/glaeda-mini-health."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(name: str, path: Path):
    loader = importlib.machinery.SourceFileLoader(name, os.fspath(path))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(name, loader))
    sys.modules[name] = module
    loader.exec_module(module)
    return module


health = load("glaeda_mini_health", ROOT / "scripts" / "glaeda-mini-health")
NOW = 1_790_500_000.0
TM = health.TESTMANAGERD


class FakeMini(health.Mini):
    """Every read and heal from fields; heals are recorded, never run."""

    def __init__(self) -> None:
        super().__init__(Path("/nonexistent"))
        self._hook = False  # no runner hook: the module's own patterns
        self.uid = 501
        self.console_state = ("unlocked", "cmux")
        self.lw: dict = {}
        self.pm = " displaysleep         0\n"
        self.procs = [(10, 501, "S", TM)]
        self.lines = [(NOW - 600, "Created session with socket 3"), (NOW - 590, "Tearing down <XCIDESession>")]
        self.ts: tuple[str, str | None, str] | None = ("app", "Running", "")
        self.ts_after: list[str] = []
        self.agents = [("com.teamleaderleo.glaeda.cmux-runner", True, True, "(never exited)"),
                       ("com.teamleaderleo.glaeda.cmux-runner.1", True, True, "(never exited)")]
        self.recycler = lambda: "testmanagerd: stopped pid 10"
        self.did: list[str] = []

    def console(self):
        return self.console_state

    def loginwindow(self):
        return self.lw

    def pmset(self):
        return self.pm

    def processes(self):
        return self.procs

    def elapsed_s(self, pid):
        return 3600

    def session_lines(self, pid, minutes):
        return self.lines if self.lines is not None else None

    def tailscale(self):
        if self.ts_after and self.did and self.did[-1].startswith(("up", "vpn")):
            self.ts = (self.ts[0], self.ts_after.pop(0), "")
        return self.ts

    def vpn_service(self):
        return "C7E1635B-DD04-49D8-A16F-FB4E34804764"

    def runner_agents(self):
        return self.agents

    def recycle_testmanagerd(self):
        self.did.append("recycle")
        return self.recycler() if self.recycler else None

    def tailscale_up(self):
        self.did.append("up")

    def restart_vpn(self, service):
        self.did.append("vpn " + service)

    def kickstart(self, label):
        self.did.append("kickstart " + label)
        self.agents = [(l, lo, True if l == label else r, x) for l, lo, r, x in self.agents]

    def sleep(self, seconds):
        pass


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.state, self.report = self.dir / "state.json", self.dir / "health.json"
        self.mini = FakeMini()

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_once(self, at: float = NOW, apply: bool = True, heal_ok: bool = True) -> dict:
        return health.run(self.mini, apply, now=at, state_path=self.state, report_path=self.report, heal_ok=heal_ok)

    def ids(self, report: dict) -> list[str]:
        return [f["id"] for f in report["findings"]]


class Detection(Base):
    def test_healthy_mini_reports_nothing(self) -> None:
        report = self.run_once()
        self.assertEqual(report["findings"], [])
        self.assertEqual(json.loads(self.report.read_text())["schema"], "glaeda-mini-health/v1")

    def test_locked_console_autologin_key_and_display_sleep_need_root(self) -> None:
        self.mini.console_state = ("locked", "cmux")
        self.mini.lw = {"autoLoginUserScreenLocked": True}
        self.mini.pm = " displaysleep         10\n"
        report = self.run_once()
        self.assertEqual(self.ids(report), ["console_locked", "autologin_locks", "display_sleep"])
        self.assertTrue(all(f["auto_fix"] == "impossible" for f in report["findings"]))
        self.assertEqual(report["findings"][0]["severity"], "error")
        self.assertIn("10 min", report["findings"][2]["evidence"])
        self.assertEqual(self.mini.did, [])

    def test_no_console_user(self) -> None:
        self.mini.console_state = ("no_user", None)
        self.assertEqual(self.ids(self.run_once()), ["console_no_user"])

    def test_since_is_the_first_sighting(self) -> None:
        self.mini.pm = " displaysleep         10\n"
        self.run_once(NOW)
        report = self.run_once(NOW + 120)
        self.assertEqual(report["findings"][0]["since"], health.iso(NOW))
        self.mini.pm = " displaysleep         0\n"
        self.run_once(NOW + 240)
        self.mini.pm = " displaysleep         10\n"
        self.assertEqual(self.run_once(NOW + 360)["findings"][0]["since"], health.iso(NOW + 360))

    def test_plan_mode_writes_and_heals_nothing(self) -> None:
        self.mini.lines.append((NOW - 300, health.WEDGE_SIGNATURE))
        report = self.run_once(apply=False)
        self.assertEqual(self.ids(report), ["testmanagerd_wedged"])
        self.assertEqual(self.mini.did, [])
        self.assertFalse(self.report.exists())


class Testmanagerd(Base):
    def test_a_finished_teardown_is_healthy(self) -> None:
        self.mini.lines += [(NOW - 300, health.WEDGE_SIGNATURE), (NOW - 300, "Closing session <XCIDESession>"),
                            (NOW - 300, "Tearing down <XCIDESession>")]
        self.assertEqual(self.run_once()["findings"], [])

    def test_a_fresh_signature_is_not_yet_a_wedge(self) -> None:
        self.mini.lines.append((NOW - 5, health.WEDGE_SIGNATURE))
        self.assertEqual(self.run_once()["findings"], [])

    def test_wedge_is_recycled_while_no_test_runs(self) -> None:
        self.mini.lines.append((NOW - 300, health.WEDGE_SIGNATURE))
        report = self.run_once()
        self.assertEqual(self.mini.did, ["recycle"])
        self.assertEqual(report["findings"], [])
        self.assertEqual(report["healed"][0]["code"], "testmanagerd_wedged")
        self.assertEqual(report["healed"][0]["auto_fix"], "done")
        self.assertIn("stopped pid 10", report["healed"][0]["action"])

    def test_a_running_test_keeps_the_last_verdict(self) -> None:
        self.mini.lines.append((NOW - 300, health.WEDGE_SIGNATURE))
        self.mini.recycler = None  # a hook without the recycler: reported, never healed here
        first = self.run_once()
        self.assertEqual(first["findings"][0]["auto_fix"], "pending")
        self.assertIn("glaeda#1281", first["findings"][0]["note"])
        self.mini.procs.append((20, 501, "S", "/usr/bin/xcodebuild test-without-building -xctestrun x"))
        again = self.run_once(NOW + 120)
        self.assertEqual(self.ids(again), ["testmanagerd_wedged"])
        self.assertEqual(again["findings"][0]["since"], health.iso(NOW))
        self.assertEqual(self.mini.did, ["recycle"], "no heal while a test runs")

    def test_a_test_running_past_the_heal_gate_never_uses_up_tries(self) -> None:
        self.mini.lines.append((NOW - 300, health.WEDGE_SIGNATURE))
        self.mini.recycler = lambda: "testmanagerd: kept (a test is running on this mini)"
        self.run_once()
        self.mini.procs.append((20, 501, "S", "/usr/bin/xcodebuild test -scheme cmux"))
        for i in range(1, 6):
            report = self.run_once(NOW + i * health.HEAL_EVERY_S)
        self.assertEqual(self.mini.did, ["recycle"])
        self.assertEqual(report["findings"][0]["auto_fix"], "pending")

    def test_an_unreadable_log_keeps_the_last_verdict(self) -> None:
        self.mini.lines.append((NOW - 300, health.WEDGE_SIGNATURE))
        self.mini.recycler = None
        self.run_once()
        self.mini.lines = None
        self.assertEqual(self.ids(self.run_once(NOW + 120)), ["testmanagerd_wedged"])

    def test_tries_run_out(self) -> None:
        self.mini.lines.append((NOW - 300, health.WEDGE_SIGNATURE))
        self.mini.recycler = lambda: "testmanagerd: stuck: pid 10 outlived SIGKILL"
        for i in range(health.HEAL_TRIES):
            report = self.run_once(NOW + i * health.HEAL_EVERY_S)
            self.assertEqual(report["findings"][0]["auto_fix"], "pending")
        report = self.run_once(NOW + health.HEAL_TRIES * health.HEAL_EVERY_S)
        self.assertEqual(report["findings"][0]["auto_fix"], "impossible")
        self.assertEqual(self.mini.did.count("recycle"), health.HEAL_TRIES)

    def test_kill_switch_reports_without_healing(self) -> None:
        self.mini.lines.append((NOW - 300, health.WEDGE_SIGNATURE))
        report = self.run_once(heal_ok=False)
        self.assertEqual(self.mini.did, [])
        self.assertIn("healing is off", report["findings"][0]["note"])


class Tailscale(Base):
    def test_stopped_gets_up_on_the_second_run(self) -> None:
        self.mini.ts, self.mini.ts_after = ("app", "Stopped", ""), ["Running"]
        first = self.run_once()
        self.assertEqual(self.ids(first), ["tailscale_down"])
        self.assertEqual(self.mini.did, [], "one sighting never heals")
        second = self.run_once(NOW + 120)
        self.assertEqual(self.mini.did, ["up"])
        self.assertEqual(second["findings"], [])
        self.assertEqual(second["healed"][0]["action"], "tailscale up: Running")

    def test_stuck_starting_restarts_the_vpn_service(self) -> None:
        self.mini.ts, self.mini.ts_after = ("app", "Starting", ""), ["Running"]
        self.run_once()
        self.run_once(NOW + 120)
        self.assertEqual(self.mini.did, ["vpn C7E1635B-DD04-49D8-A16F-FB4E34804764"])

    def test_no_backend_state_is_a_probe_error_never_a_heal(self) -> None:
        self.mini.ts = ("app", None, "Tailscale status --json exited 0 without a BackendState: The Tailscale GUI "
                        "failed to start")
        self.run_once()
        report = self.run_once(NOW + 120)
        self.assertEqual(self.ids(report), ["tailscale_probe_error"])
        self.assertEqual(report["findings"][0]["auto_fix"], "impossible")
        self.assertIn("GUI failed to start", report["findings"][0]["evidence"])
        self.assertEqual(self.mini.did, [])

    def test_standalone_and_logged_out_are_reported_only(self) -> None:
        self.mini.ts = ("standalone", "Starting", "")
        self.assertEqual(self.run_once()["findings"][0]["auto_fix"], "impossible")
        self.mini.ts = ("app", "NeedsLogin", "")
        self.assertEqual(self.run_once(NOW + 120)["findings"][0]["auto_fix"], "impossible")
        self.run_once(NOW + 240)
        self.assertEqual(self.mini.did, [])


class TailscaleCli(unittest.TestCase):
    """Mini.tailscale against a stand-in CLI: which binary it runs, with what environment, and what it makes of it."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.saved = (health.TAILSCALE_APP, health.TAILSCALE_STANDALONE)
        health.TAILSCALE_APP, health.TAILSCALE_STANDALONE = str(self.dir / "app"), str(self.dir / "standalone")

    def tearDown(self) -> None:
        health.TAILSCALE_APP, health.TAILSCALE_STANDALONE = self.saved
        self.tmp.cleanup()

    def cli(self, path: str, body: str) -> None:
        Path(path).write_text("#!/bin/sh\n" + body + "\n")
        os.chmod(path, 0o755)

    def test_app_cli_runs_as_the_cli_when_no_standalone(self) -> None:
        # like the app binary: the status only with TAILSCALE_BE_CLI (or a terminal), else it starts as the GUI
        self.cli(health.TAILSCALE_APP, 'if [ "$TAILSCALE_BE_CLI" = 1 ]; then echo \'{"BackendState": "Running"}\'; '
                 'else echo "The Tailscale GUI failed to start"; fi')
        self.assertEqual(health.Mini().tailscale(), ("app", "Running", ""))

    def test_a_real_standalone_cli_wins(self) -> None:
        self.cli(health.TAILSCALE_APP, "echo '{\"BackendState\": \"Running\"}'")
        self.cli(health.TAILSCALE_STANDALONE, "echo '{\"BackendState\": \"Stopped\"}'")
        self.assertEqual(health.Mini().tailscale(), ("standalone", "Stopped", ""))

    def test_a_link_into_the_app_is_the_app(self) -> None:
        self.cli(health.TAILSCALE_APP, "echo '{\"BackendState\": \"Running\"}'")
        os.symlink(self.dir / "Tailscale.app", health.TAILSCALE_STANDALONE)  # dangling, as a broken "Install CLI"
        self.assertEqual(health.Mini().tailscale(), ("app", "Running", ""))

    def test_no_cli_is_no_tailscale(self) -> None:
        self.assertIsNone(health.Mini().tailscale())

    def test_an_error_without_json_is_a_probe_error(self) -> None:
        self.cli(health.TAILSCALE_APP, "echo 'failed to connect to local Tailscale service' >&2; exit 1")
        flavor, state, said = health.Mini().tailscale()
        self.assertEqual((flavor, state), ("app", None))
        self.assertIn("exited 1", said)
        self.assertIn("failed to connect", said)


class Runners(Base):
    def test_stopped_agent_is_kickstarted_after_two_runs(self) -> None:
        self.mini.agents[1] = ("com.teamleaderleo.glaeda.cmux-runner.1", True, False, "0")
        first = self.run_once()
        self.assertEqual(self.ids(first), ["runner_stopped:1"])
        self.assertIn("last exit code 0", first["findings"][0]["evidence"])
        second = self.run_once(NOW + 120)
        self.assertEqual(self.mini.did, ["kickstart com.teamleaderleo.glaeda.cmux-runner.1"])
        self.assertEqual(second["findings"], [])

    def test_a_runner_that_keeps_exiting_stops_being_kickstarted(self) -> None:
        stopped = ("com.teamleaderleo.glaeda.cmux-runner.1", True, False, "0")
        t = NOW
        for _ in range(12):
            self.mini.agents[1] = stopped  # it exits cleanly again after every kickstart
            report = self.run_once(t)
            t += health.HEAL_EVERY_S
        self.assertEqual(self.mini.did.count("kickstart com.teamleaderleo.glaeda.cmux-runner.1"), health.HEAL_TRIES)
        self.assertEqual(report["findings"][0]["auto_fix"], "impossible")

    def test_a_held_runner_is_not_a_finding(self) -> None:
        held = self.dir / "held"
        (held / "actions-runner-glaeda-1").parent.mkdir(parents=True)
        (held / "actions-runner-glaeda-1").touch()
        saved, health.HELD = health.HELD, held
        self.addCleanup(setattr, health, "HELD", saved)
        self.mini.agents[1] = ("com.teamleaderleo.glaeda.cmux-runner.1", False, False, "?")
        self.assertEqual(self.run_once()["findings"], [])

    def test_the_first_runner_agent_is_zero(self) -> None:
        self.mini.agents[0] = ("com.teamleaderleo.glaeda.cmux-runner", True, False, "0")
        self.run_once()
        self.run_once(NOW + 120)
        self.assertEqual(self.mini.did, ["kickstart com.teamleaderleo.glaeda.cmux-runner"])

    def test_unloaded_agent_is_left_to_an_operator(self) -> None:
        self.mini.agents[0] = ("com.teamleaderleo.glaeda.cmux-runner", False, False, "?")
        self.run_once()
        report = self.run_once(NOW + 120)
        self.assertEqual(self.ids(report), ["runner_unloaded:0"])
        self.assertEqual(report["findings"][0]["auto_fix"], "impossible")
        self.assertEqual(self.mini.did, [])


class Parsing(unittest.TestCase):
    def test_no_runner_is_a_no_op(self) -> None:
        with tempfile.TemporaryDirectory() as home:
            self.assertEqual(health.Mini(Path(home)).runner_dirs(), [])

    def test_xctest_pattern(self) -> None:
        for command in ("/usr/bin/xcodebuild test -scheme cmux", "/Applications/Xcode.app/x/usr/bin/xctest a.xctest"):
            self.assertTrue(health.XCTEST_RUNNING.search(command), command)
        self.assertFalse(health.XCTEST_RUNNING.search("/usr/bin/xcodebuild build -scheme cmux"))


if __name__ == "__main__":
    unittest.main()
