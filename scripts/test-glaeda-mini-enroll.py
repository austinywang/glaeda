#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-mini-enroll. Runs on Linux CI: no Mac, no cargo, no builds."""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_mini_enroll", os.fspath(ROOT / "scripts" / "glaeda-mini-enroll"))
spec = importlib.util.spec_from_loader("glaeda_mini_enroll", loader)
me = importlib.util.module_from_spec(spec)
sys.modules["glaeda_mini_enroll"] = me
loader.exec_module(me)


def enrollment(state: str, node: str = "cmux-mac-001", reason: str | None = None) -> dict:
    return {"nodeId": node, "state": state, "quarantineReason": reason}


class Plan(unittest.TestCase):
    def test_fresh_mini_runs_every_step(self) -> None:
        steps = me.plan(me.State(False, None, False), "cmux-mac-001", False, False)
        self.assertEqual(steps, ["install-glaeda", "bootstrap", "enroll", "accept-local", "eligible", "status"])

    def test_next_step_names_the_runner_not_the_retired_path(self) -> None:
        self.assertIn("glaeda-cmux-runner --apply", me.NEXT_STEP)
        self.assertIn("glaeda#1174", me.NEXT_STEP)
        self.assertNotIn("persistent-compile", me.NEXT_STEP + me.__doc__)

    def test_fresh_mini_needs_a_node_id(self) -> None:
        with self.assertRaisesRegex(me.Stop, "--node-id"):
            me.plan(me.State(True, None, False), None, False, False)

    def test_eligible_node_with_current_receipt_only_reports(self) -> None:
        self.assertEqual(me.plan(me.State(True, enrollment("eligible"), True), None, False, False), ["status"])

    def test_resume_after_a_rejected_acceptance(self) -> None:
        steps = me.plan(me.State(True, enrollment("enrolling"), False), None, False, False)
        self.assertEqual(steps, ["accept-local", "eligible", "status"])

    def test_accepted_but_not_transitioned(self) -> None:
        self.assertEqual(me.plan(me.State(True, enrollment("enrolling"), True), None, False, False), ["eligible", "status"])

    def test_draining_node_comes_back(self) -> None:
        self.assertEqual(me.plan(me.State(True, enrollment("draining"), True), None, False, False), ["eligible", "status"])

    def test_reaccept_on_an_enrolling_node_runs_acceptance(self) -> None:
        steps = me.plan(me.State(True, enrollment("enrolling"), True), None, True, False)
        self.assertEqual(steps, ["accept-local", "eligible", "status"])

    def test_reacceptance_of_an_eligible_or_draining_node_stops_with_the_transition(self) -> None:
        # cmux_fleet.py refuses accept-local unless the node is enrolling.
        for state, reaccept, current in (("eligible", True, True), ("draining", False, False)):
            with self.subTest(state=state), self.assertRaisesRegex(me.Stop, "--to enrolling"):
                me.plan(me.State(True, enrollment(state), current), None, reaccept, False)

    def test_no_accept_enrolls_without_acceptance(self) -> None:
        steps = me.plan(me.State(False, None, False), "cmux-mac-003", False, False, candidate=True, no_accept=True)
        self.assertEqual(steps, ["stage", "bootstrap", "enroll", "status"])
        # An enrolling node with a current receipt still transitions; one without stays enrolling.
        self.assertEqual(me.plan(me.State(True, enrollment("enrolling"), True), None, False, False, no_accept=True),
                         ["eligible", "status"])
        self.assertEqual(me.plan(me.State(True, enrollment("enrolling"), False), None, False, False, no_accept=True),
                         ["status"])
        with self.assertRaises(me.Stop):
            me.plan(me.State(True, enrollment("enrolling"), False), None, True, False, no_accept=True)

    def test_class_receipt_replaces_accept_local(self) -> None:
        steps = me.plan(me.State(False, None, False), "cmux-mac-003", False, False, candidate=True,
                        class_receipt=True)
        self.assertEqual(steps, ["stage", "bootstrap", "enroll", "adopt-class", "eligible", "status"])
        self.assertEqual(me.plan(me.State(True, enrollment("enrolling"), False), None, False, False,
                                 class_receipt=True), ["adopt-class", "eligible", "status"])
        self.assertEqual(me.plan(me.State(True, enrollment("eligible"), True), None, False, False,
                                 class_receipt=True), ["status"])
        with self.assertRaisesRegex(me.Stop, "adopt-class runs only on an enrolling node"):
            me.plan(me.State(True, enrollment("eligible"), False), None, False, False, class_receipt=True)
        for reaccept, no_accept in ((True, False), (False, True)):
            with self.assertRaisesRegex(me.Stop, "drop --no-accept and --reaccept"):
                me.plan(me.State(True, None, False), "n", reaccept, False, no_accept=no_accept, class_receipt=True)

    def test_rebuild_does_not_force_acceptance(self) -> None:
        steps = me.plan(me.State(True, enrollment("eligible"), True), None, False, True)
        self.assertEqual(steps, ["install-glaeda", "status"])

    def test_a_different_node_id_is_refused(self) -> None:
        with self.assertRaisesRegex(me.Stop, "already enrolled as cmux-mac-001"):
            me.plan(me.State(True, enrollment("eligible"), True), "cmux-mac-002", False, False)

    def test_quarantined_and_retired_stop(self) -> None:
        with self.assertRaisesRegex(me.Stop, "disk_pressure"):
            me.plan(me.State(True, enrollment("quarantined", reason="disk_pressure"), False), None, False, False)
        with self.assertRaisesRegex(me.Stop, "retired"):
            me.plan(me.State(True, enrollment("retired"), False), None, False, False)


class CandidatePlan(unittest.TestCase):
    """A reviewed candidate replaces the source build; nothing compiles Rust on the node."""

    def test_fresh_mini_stages_instead_of_building(self) -> None:
        steps = me.plan(me.State(False, None, False), "cmux-mac-001", False, False, candidate=True)
        self.assertEqual(steps, ["stage", "bootstrap", "enroll", "accept-local", "eligible", "status"])

    def test_a_staged_generation_is_not_restaged(self) -> None:
        steps = me.plan(me.State(True, enrollment("eligible"), True), None, False, False, candidate=True)
        self.assertEqual(steps, ["status"])

    def test_a_new_candidate_on_an_eligible_node_stops_before_staging_work(self) -> None:
        with self.assertRaisesRegex(me.Stop, "fresh acceptance"):
            me.plan(me.State(False, enrollment("eligible"), False), None, False, False, candidate=True)

    def test_staged_code_is_inspected_before_it_runs(self) -> None:
        order = []
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch.object(me, "read_json", return_value=None), \
             mock.patch.object(me.Path, "is_file", return_value=True), \
             mock.patch.object(me.Path, "exists", return_value=False), \
             mock.patch.object(me.Runner, "inspect", side_effect=lambda *a: order.append("inspect")), \
             mock.patch.object(me.Runner, "acceptance_current", side_effect=lambda *a: order.append("run") or False), \
             mock.patch("builtins.print"):
            me.main(["--cmux-root", "/c", "--node-id", "n", "--candidate", "/a", "--sha256", "s",
                     "--source", "36e07e36ea7b9bc9e04c366547a5312dd348024d"])
        self.assertEqual(order, ["inspect", "run"])

    def test_an_unreadable_enrollment_is_not_replaced(self) -> None:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch.object(me, "read_json", return_value=None), \
             mock.patch.object(me.Path, "exists", return_value=True), \
             mock.patch("builtins.print"):
            self.assertEqual(me.main(["--cmux-root", "/c", "--node-id", "n"]), 2)

    def test_candidate_steps_run_the_generation_tools_and_binary(self) -> None:
        generation = Path("/Users/op/Projects/glaeda-generations/36e07e36ea7b")
        runner = me.Runner("/py", me.Paths(Path("/Users/op")), Path("/cmux"), generation)
        self.assertEqual(runner.fleet_py, "/Users/op/Projects/glaeda-generations/36e07e36ea7b/scripts/cmux_fleet.py")
        self.assertEqual(runner.glaeda_bin, generation / "bin/glaeda")
        self.assertEqual(me.Paths(Path("/Users/op")).generation("36e07e36ea7b9bc9e04c366547a5312dd348024d"),
                         generation)

    def test_fleet_tools_run_without_writing_bytecode(self) -> None:
        runner = me.Runner("/py", me.Paths(Path("/Users/op")), Path("/cmux"), Path("/g"))
        with mock.patch.object(me.subprocess, "run") as run:
            runner.fleet("status", "e")
        self.assertEqual(run.call_args.args[0][:3], ["/py", "-B", "/g/scripts/cmux_fleet.py"])

    def test_candidate_flags_go_together(self) -> None:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch("builtins.print"):
            self.assertEqual(me.main(["--cmux-root", "/c", "--candidate", "/a.tar.gz"]), 2)


class ClassReceipt(unittest.TestCase):
    SHA = "sha256:" + "a" * 64

    def main(self, *extra: str) -> int:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch("builtins.print"):
            return me.main(["--cmux-root", "/c", *extra])

    def test_class_flags_go_together_and_need_a_candidate(self) -> None:
        candidate = ["--candidate", "/a", "--sha256", "s", "--source", "3" * 40]
        self.assertEqual(self.main("--class-receipt", "/r.json", *candidate), 2)
        self.assertEqual(self.main("--class-receipt", "/r.json", "--class-receipt-sha256", self.SHA,
                                   "--fleet-class", "std"), 2)
        self.assertEqual(self.main("--class-receipt", "/r.json", "--class-receipt-sha256", "abc",
                                   "--fleet-class", "std", *candidate), 2)
        self.assertEqual(self.main("--class-receipt", "/r.json", "--class-receipt-sha256", self.SHA,
                                   "--fleet-class", "STD", *candidate), 2)

    def test_stdin_receipt_is_bounded_and_must_be_json(self) -> None:
        for data in (b"", b"not json", b"{" + b" " * me.MAX_CLASS_RECEIPT_BYTES + b"}"):
            with mock.patch.object(me.sys, "stdin", mock.Mock(buffer=io.BytesIO(data))), \
                 self.assertRaises(me.Stop):
                me.read_class_receipt("-")
        with mock.patch.object(me.sys, "stdin", mock.Mock(buffer=io.BytesIO(b'{"schema": "x"}'))):
            self.assertEqual(me.read_class_receipt("-"), b'{"schema": "x"}')

    def test_adopt_class_keeps_the_receipt_and_asks_the_generation(self) -> None:
        generation = Path("/g")
        runner = me.Runner("/py", me.Paths(Path("/Users/op")), Path("/cmux"), generation)
        class_path = me.Paths(Path("/Users/op")).class_receipt("std")
        pending = class_path.with_name(".std.json.pending")
        written = {}
        accepted = json.dumps({"result": "accepted"})
        with mock.patch.object(me.Runner, "write_private", side_effect=lambda p, t: written.__setitem__(p, t)), \
             mock.patch.object(me.os, "replace") as replace, mock.patch.object(me.Path, "unlink"), \
             mock.patch.object(me.subprocess, "run",
                               return_value=me.subprocess.CompletedProcess([], 0, stdout=accepted)) as run, \
             mock.patch("builtins.print"):
            runner.adopt_class(b'{"k": 1}', self.SHA, "std")
        argv = run.call_args.args[0]
        self.assertEqual(argv[:4], ["/py", "-B", "/g/scripts/cmux_fleet.py", "adopt-class-acceptance"])
        self.assertEqual(argv[argv.index("--class-receipt") + 1], os.fspath(pending))
        self.assertEqual(argv[argv.index("--expected-sha256") + 1], self.SHA)
        self.assertEqual(argv[argv.index("--glaeda") + 1], "/g/bin/glaeda")
        self.assertIn("--cache-root", argv)
        self.assertEqual(written[pending], '{"k": 1}')
        replace.assert_called_once_with(pending, class_path)
        self.assertEqual(written[me.Paths(Path("/Users/op")).acceptance], accepted)
        # A receipt this node does not match is neither kept nor turned into an acceptance.
        with mock.patch.object(me.Runner, "write_private") as write, \
             mock.patch.object(me.os, "replace") as replace, mock.patch.object(me.Path, "unlink") as unlink, \
             mock.patch.object(me.subprocess, "run",
                               return_value=me.subprocess.CompletedProcess([], 1, stdout="")), \
             mock.patch("builtins.print"), self.assertRaisesRegex(me.Stop, "run accept-local"):
            runner.adopt_class(b"{}", self.SHA, "std")
        replace.assert_not_called()
        unlink.assert_called_once()
        self.assertEqual(write.call_count, 1)

    def test_plan_mode_does_not_read_the_receipt(self) -> None:
        with mock.patch.object(me, "read_class_receipt") as read, \
             mock.patch.object(me, "read_json", return_value=None), \
             mock.patch.object(me.Path, "is_file", return_value=False), \
             mock.patch.object(me.Path, "exists", return_value=False):
            code = self.main("--node-id", "cmux-mac-003", "--candidate", "/a", "--sha256", "s", "--source", "3" * 40,
                             "--class-receipt", "-", "--class-receipt-sha256", self.SHA, "--fleet-class", "std")
        self.assertEqual(code, 0)
        read.assert_not_called()

    def test_a_receipt_that_is_not_utf8_json_stops(self) -> None:
        with mock.patch.object(me.sys, "stdin", mock.Mock(buffer=io.BytesIO(b'{"a": "\xff"}'))), \
             self.assertRaisesRegex(me.Stop, "UTF-8"):
            me.read_class_receipt("-")


class Renew(unittest.TestCase):
    """--renew moves an enrolled node onto a new candidate or toolchain in one run."""

    def test_a_new_candidate_on_an_eligible_node_renews_then_accepts(self) -> None:
        steps = me.plan(me.State(False, enrollment("eligible"), False, stale=True), "cmux-mac-001", False, False,
                        candidate=True, renew=True)
        self.assertEqual(steps, ["stage", "bootstrap", "renew", "accept-local", "eligible", "status"])
        steps = me.plan(me.State(True, enrollment("eligible"), True, stale=True), None, False, False,
                        candidate=True, class_receipt=True, renew=True)
        self.assertEqual(steps, ["bootstrap", "renew", "adopt-class", "eligible", "status"])

    def test_a_current_node_is_left_alone(self) -> None:
        steps = me.plan(me.State(True, enrollment("eligible"), True), None, False, False, candidate=True,
                        class_receipt=True, renew=True)
        self.assertEqual(steps, ["status"])

    def test_quarantined_and_drifted_nodes_renew_instead_of_stopping(self) -> None:
        for state, current in (("quarantined", False), ("eligible", False), ("draining", False), ("enrolling", False)):
            with self.subTest(state=state):
                steps = me.plan(me.State(True, enrollment(state, reason="failed_acceptance" if state == "quarantined"
                                                          else None), current), None, False, False,
                                candidate=True, renew=True)
                self.assertEqual(steps[:2], ["bootstrap", "renew"])

    def test_renew_keeps_the_refusals(self) -> None:
        with self.assertRaisesRegex(me.Stop, "retired"):
            me.plan(me.State(True, enrollment("retired"), False), None, False, False, candidate=True, renew=True)
        with self.assertRaisesRegex(me.Stop, "already enrolled as cmux-mac-001"):
            me.plan(me.State(True, enrollment("eligible"), True, stale=True), "cmux-mac-002", False, False,
                    candidate=True, renew=True)
        with self.assertRaisesRegex(me.Stop, "--candidate"):
            me.plan(me.State(True, enrollment("eligible"), True), None, False, False, renew=True)
        # A fresh mini under --renew is a first enrollment.
        self.assertEqual(me.plan(me.State(False, None, False), "cmux-mac-009", False, False, candidate=True, renew=True),
                         ["stage", "bootstrap", "enroll", "accept-local", "eligible", "status"])

    def test_reasons(self) -> None:
        self.assertEqual(me.renewal_reason(True, True), "stale_glaeda_generation")
        self.assertEqual(me.renewal_reason(False, True), "toolchain_mismatch")
        self.assertEqual(me.renewal_reason(False, False), "failed_acceptance")

    def fleet_calls(self, enrolled: dict, observed: dict) -> list[list[str]]:
        runner = me.Runner("/py", me.Paths(Path("/Users/op")), Path("/cmux"), Path("/g"))
        calls: list[list[str]] = []

        def fleet(*args: str, capture: bool = True):
            calls.append(list(args))
            out = json.dumps({"planSha256": "sha256:" + "e" * 64}) if args[0] == "renew-enrollment" else "{}"
            return me.subprocess.CompletedProcess([], 0, stdout=out)

        documents = iter([enrolled, observed, {"nodeId": "cmux-mac-001", "enrollmentGeneration": 3}])
        with mock.patch.object(runner, "fleet", side_effect=fleet), \
             mock.patch.object(me, "read_json", side_effect=lambda path: next(documents)), \
             mock.patch("builtins.print"):
            runner.renew(Path("/Users/op/.config/glaeda/cmux-fleet/bootstrap.json"))
        return calls

    def test_renew_quarantines_with_the_reason_then_applies_the_previewed_plan(self) -> None:
        old = {"nodeId": "cmux-mac-001", "state": "eligible", "glaedaGeneration": "sha256:old",
               "supportedToolchainGenerations": ["sha256:tc"]}
        calls = self.fleet_calls(old, {"glaedaGeneration": "sha256:new", "toolchainGeneration": "sha256:tc"})
        self.assertEqual([c[0] for c in calls], ["transition-apply", "renew-enrollment", "renew-enrollment-apply"])
        self.assertEqual(calls[0][-4:], ["--to", "quarantined", "--reason", "stale_glaeda_generation"])
        self.assertEqual(calls[2][-2:], ["--expected-plan-sha256", "sha256:" + "e" * 64])
        # Already quarantined: no second quarantine; already renewed and enrolling: nothing at all.
        calls = self.fleet_calls({**old, "state": "quarantined"},
                                 {"glaedaGeneration": "sha256:old", "toolchainGeneration": "sha256:tc"})
        self.assertEqual([c[0] for c in calls], ["renew-enrollment", "renew-enrollment-apply"])
        calls = self.fleet_calls({**old, "state": "enrolling"},
                                 {"glaedaGeneration": "sha256:old", "toolchainGeneration": "sha256:tc"})
        self.assertEqual(calls, [])

    def test_a_node_already_on_the_candidate_is_replanned_after_staging_not_renewed(self) -> None:
        # Its generation directory was removed, so it read as stale; staging the same bytes shows it is current.
        enrolled = {"nodeId": "cmux-mac-001", "state": "eligible", "glaedaGeneration": "sha256:same"}
        done = []
        with mock.patch.object(me, "DARWIN_REQUIRED", False), mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch.object(me, "read_json", return_value=enrolled), \
             mock.patch.object(me.Path, "is_file", return_value=False), \
             mock.patch.object(me.Path, "exists", return_value=False), \
             mock.patch.object(me, "glaeda_digest", return_value="sha256:same"), \
             mock.patch.object(me.Runner, "stage", side_effect=lambda *a: done.append("stage")), \
             mock.patch.object(me.Runner, "acceptance_current", return_value=True), \
             mock.patch.object(me.Runner, "bootstrap", side_effect=lambda: done.append("bootstrap")), \
             mock.patch.object(me.Runner, "renew", side_effect=lambda *a: done.append("renew")), \
             mock.patch.object(me.Runner, "status", side_effect=lambda: done.append("status") or
                               {"routingCandidateEligible": True}), \
             mock.patch("builtins.print"):
            code = me.main(["--cmux-root", "/c", "--candidate", "/a", "--sha256", "s", "--renew", "--apply",
                            "--source", "36e07e36ea7b9bc9e04c366547a5312dd348024d"])
        self.assertEqual((code, done), (0, ["stage", "status"]))

    def test_renew_flag_needs_a_candidate(self) -> None:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), mock.patch.object(me, "pick_python", return_value="/py"), \
             contextlib.redirect_stderr(io.StringIO()) as err:
            code = me.main(["--cmux-root", "/cmux", "--renew"])
        self.assertEqual(code, 2)
        self.assertIn("--renew needs --candidate", err.getvalue())


class Python(unittest.TestCase):
    def test_picks_the_first_313_or_newer(self) -> None:
        versions = {"/a": (3, 12), "/b": (3, 13), "/c": (3, 14)}
        with mock.patch.object(me, "PYTHON_CANDIDATES", ("/a", "/b", "/c")), \
             mock.patch.object(me.shutil, "which", return_value=None), \
             mock.patch.object(me.os, "access", return_value=True), \
             mock.patch.object(me, "python_version", side_effect=versions.get):
            self.assertEqual(me.pick_python(None), "/b")

    def test_none_when_only_old_pythons(self) -> None:
        with mock.patch.object(me, "PYTHON_CANDIDATES", ("/a",)), \
             mock.patch.object(me.shutil, "which", return_value=None), \
             mock.patch.object(me.os, "access", return_value=True), \
             mock.patch.object(me, "python_version", return_value=(3, 9)):
            self.assertIsNone(me.pick_python(None))

    def test_finds_a_user_local_python_when_homebrew_has_none(self) -> None:
        local = "/home/op/.local/bin/python3"
        with mock.patch.object(me, "PYTHON_CANDIDATES", ("/a",)), \
             mock.patch.object(me.shutil, "which", return_value=None), \
             mock.patch.object(me.os, "access", return_value=True), \
             mock.patch.object(me, "python_version", side_effect={"/a": (3, 9), local: (3, 14)}.get):
            self.assertEqual(me.pick_python(None, Path("/home/op")), local)

    def test_path_lookup_never_probes_the_usr_bin_stub(self) -> None:
        probed: list[str] = []
        with mock.patch.object(me, "PYTHON_CANDIDATES", ()), \
             mock.patch.object(me.shutil, "which", return_value="/usr/bin/python3"), \
             mock.patch.object(me.os, "access", side_effect=lambda p, m: p == "/usr/bin/python3"), \
             mock.patch.object(me, "python_version", side_effect=lambda p: probed.append(p)):
            self.assertIsNone(me.pick_python(None, Path("/home/op")))
        self.assertEqual(probed, [])

    def test_missing_python_names_both_fixes(self) -> None:
        err = io.StringIO()
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value=None), contextlib.redirect_stderr(err):
            self.assertEqual(me.main(["--cmux-root", "/c"]), 2)
        self.assertIn("python@3.13", err.getvalue())
        self.assertIn("~/.local/bin/python3", err.getvalue())

    def test_an_explicit_old_python_is_refused(self) -> None:
        with mock.patch.object(me.os, "access", return_value=True), \
             mock.patch.object(me, "python_version", return_value=(3, 12)):
            self.assertIsNone(me.pick_python("/usr/bin/python3"))


class Paths(unittest.TestCase):
    def test_paths_match_the_enrollment_runbook_and_mini_setup(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            paths = me.Paths(Path("/Users/op"))
            self.assertEqual(paths.enrollment, Path("/Users/op/.config/glaeda/cmux-fleet/enrollment.json"))
            self.assertEqual(paths.acceptance,
                             Path("/Users/op/.config/glaeda/cmux-fleet/acceptance/cmux_macos_native_build.json"))
            self.assertEqual(paths.glaeda_bin, Path("/Users/op/.local/share/glaeda/cmux-fleet/glaeda"))
        setup = (ROOT / "scripts" / "glaeda-mini-setup").read_text()
        self.assertIn('".cache/glaeda/cmux-native-cache"', setup)
        self.assertIn('.local/share/glaeda/cmux-fleet/glaeda"', setup)


class PlanOnlyHasNoSideEffects(unittest.TestCase):
    def test_plan_mode_runs_no_step(self) -> None:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch.object(me.Runner, "acceptance_current", return_value=False), \
             mock.patch.object(me, "read_json", return_value=None), \
             mock.patch.object(me.Runner, "install_glaeda") as install, \
             mock.patch.object(me.Runner, "bootstrap") as bootstrap, \
             mock.patch.object(me.Runner, "accept_local") as accept, \
             mock.patch("builtins.print"):
            code = me.main(["--cmux-root", "/tmp/cmux", "--node-id", "cmux-mac-001"])
        self.assertEqual(code, 0)
        install.assert_not_called()
        bootstrap.assert_not_called()
        accept.assert_not_called()



class BootstrapProblem(unittest.TestCase):
    def test_eligible_output_is_no_problem(self) -> None:
        self.assertIsNone(me.bootstrap_problem(0, json.dumps({"eligibleForEnrollment": True}), ""))

    def test_blocking_checks_come_with_their_fixes(self) -> None:
        doc = {"eligibleForEnrollment": False, "blockingChecks": ["metalToolchain", "workloadToolPath"],
               "observed": {"toolsMissingFromWorkloadPath": ["cargo", "zig"]}}
        text = me.bootstrap_problem(0, json.dumps(doc), "")
        self.assertIn("metalToolchain: Metal Toolchain missing; run xcodebuild -downloadComponent MetalToolchain", text)
        self.assertIn("workloadToolPath: not on the workload PATH: cargo, zig; brew install rustup", text)
        self.assertIn("brew install zig", text)
        self.assertIn("glaeda-mini-fleet preflight", text)

    def test_an_observation_error_is_explained_not_called_unreadable(self) -> None:
        # What cmux-austin-mini-1 printed on 2026-09-24 with a shallow clone and no submodules.
        stderr = json.dumps({"error": "[Errno 2] No such file or directory: '/Users/cmux/cmux/ghostty/build.zig.zon'"})
        text = me.bootstrap_problem(1, "", stderr + "\n")
        self.assertIn("submodules not initialized; run git submodule update --init", text)
        self.assertNotIn("unreadable", text)
        text = me.bootstrap_problem(1, "", json.dumps({"error": "required command failed: xcrun"}))
        self.assertIn("xcodebuild -downloadComponent MetalToolchain", text)

    def test_a_refusal_without_blocking_checks_falls_back_to_stderr(self) -> None:
        stdout = json.dumps({"eligibleForEnrollment": True, "blockingChecks": []})
        stderr = json.dumps({"error": "bootstrap receipt exceeds size ceiling"})
        self.assertEqual(me.bootstrap_problem(1, stdout, stderr),
                         "bootstrap could not observe this mini: bootstrap receipt exceeds size ceiling")

    def test_output_without_a_verdict_says_so(self) -> None:
        self.assertEqual(me.bootstrap_problem(1, "", "Traceback (most recent call last):\nBoom"),
                         "bootstrap exited 1 without a verdict: Boom")


if __name__ == "__main__":
    unittest.main()
