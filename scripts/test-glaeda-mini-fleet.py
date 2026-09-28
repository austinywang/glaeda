#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-mini-fleet. Runs on Linux CI: SSH and observation are stubbed."""

from __future__ import annotations

import base64
import contextlib
import copy
import importlib.machinery
import importlib.util
import io
import json
import re
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_mini_fleet", os.fspath(ROOT / "scripts" / "glaeda-mini-fleet"))
spec = importlib.util.spec_from_loader("glaeda_mini_fleet", loader)
mf = importlib.util.module_from_spec(spec)
sys.modules["glaeda_mini_fleet"] = mf
loader.exec_module(mf)

EXAMPLE = ROOT / "examples" / "mini-fleet" / "manifest.example.json"
FP = {
    "coordinator": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    "operator": "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
    "service-cache": "SHA256:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
}
STRAY = "SHA256:DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD"


def fleet_cas_text(mode: str = "read-only", node: str = "answers", build_mode: str | None = "present",
                   **kinds: str) -> str:
    """Probe lines for a conforming fleet-cas host; kinds overrides a path's kind (src_cmux="symlink")."""
    lines = [f"fleet_cas_path\t{rel}|{kinds.get(rel.replace('xcode/', '').replace('/', '_').replace('.', '_').replace('-', '_'), want)}"
             for rel, want in {**mf.FLEET_CAS_PATHS, **mf.FLEET_CAS_WRITER_PATHS}.items()]
    lines += [f"fleet_cas_node_mode\t{mode}", f"fleet_cas_node\t{node}"]
    if build_mode is None:
        lines.append("fleet_recipe_released\tnone")
    else:
        lines += ["fleet_recipe_released\t559403ae3d50", f"fleet_recipe_build_mode\t{build_mode}"]
    return "\n".join(lines) + "\n"


def probe_text(hostname: str = "build-mini-1", keys: tuple[str, ...] = ("coordinator", "operator"),
               extra_keys: tuple[str, ...] = (), clone: bool = True, license_: str = "26.3",
               worker: str = "running pid=10", node_id: str | None = "cmux-mac-001",
               worker_proc: str = "running", fleet_cas: str | None = None) -> str:
    lines = [
        "user\tbuilder", "uid\t501", "admin_group\tyes", f"hostname\t{hostname}", "computer_name\tMini",
        "model\tMac16,11", "chip\tApple M4 Pro", "cpus\t14", f"memory_bytes\t{48 * 2**30}",
        "macos\t26.5.1", "macos_build\t25F80",
        "xcode_app\t/Applications/Xcode.app|dir|26.3|17C529",
        "xcode_select\t/Applications/Xcode.app/Contents/Developer",
        f"xcode_license_accepted\t{license_}",
        "disk\t/System/Volumes/Data|460|220",
        f"launchd\t/Library/LaunchDaemons|com.example.build-worker|{worker}",
        "fleet_root\tpresent", f"fleet_worker_proc\t{worker_proc}",
        "runner_worker\tabsent", "host_lock\tfree",
        "ak_file\tauthorized_keys|600",
    ]
    if node_id is not None:
        lines.append(f"fleet_node_id\t{node_id}")
    if clone:
        lines.append("xcode_app\t/Applications/Xcode_26.3.app|dir|26.3|17C529")
    for key in keys:
        lines.append(f"ak_key\tauthorized_keys|no|256 {FP[key]} {key} comment (ED25519)")
    for fp in extra_keys:
        lines.append(f"ak_key\tauthorized_keys|no|256 {fp} someone@laptop (ED25519)")
    return "\n".join(lines) + "\n" + (fleet_cas_text() if fleet_cas is None else fleet_cas)


def observed(**hosts: str) -> dict:
    return {"hosts": {name: {"host": name, "reachable": True, **mf.parse_probe(text)} for name, text in hosts.items()}}


class ManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def test_example_loads(self) -> None:
        self.assertEqual(set(self.manifest["hosts"]), {"build-mini-1", "build-mini-2", "small-mini"})

    def test_never_touch_host_is_refused(self) -> None:
        with self.assertRaisesRegex(mf.Failure, "never_touch"):
            mf.select_hosts(self.manifest, ["coordinator-mini"])

    def test_never_touch_cannot_also_be_a_host(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["coordinator-mini"] = {"class": "m4-16"}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "also in never_touch"):
                mf.load_manifest(path)

    def test_never_touch_matches_case_and_local_suffix(self) -> None:
        for name in ("Coordinator-Mini", "coordinator-mini.local"):
            with self.assertRaisesRegex(mf.Failure, "never_touch"):
                mf.select_hosts(self.manifest, [name])
        data = copy.deepcopy(self.manifest)
        data["hosts"]["alias"] = {"hostname": "Coordinator-Mini.local", "class": "m4-16"}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "also in never_touch"):
                mf.load_manifest(path)

    def test_public_key_must_be_one_bare_line(self) -> None:
        for bad in ("ssh-ed25519 AAAA a\nssh-ed25519 BBBB b", 'command="x" ssh-ed25519 AAAA a'):
            data = copy.deepcopy(self.manifest)
            data["keys"]["coordinator"]["public_key"] = bad
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "m.json"
                path.write_text(json.dumps(data))
                with self.assertRaisesRegex(mf.Failure, "one bare key line"):
                    mf.load_manifest(path)

    def test_bad_observation_exits_2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            obs = Path(tmp) / "obs.json"
            for text in ("not json", "{}", json.dumps({"hosts": {"build-mini-1": {"reachable": True}}})):
                obs.write_text(text)
                with contextlib.redirect_stderr(io.StringIO()):
                    code = mf.main(["check", "--manifest", os.fspath(EXAMPLE), "--host", "build-mini-1",
                                    "--observed", os.fspath(obs)])
                self.assertEqual(code, 2, text)

    def test_unknown_role_is_refused(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["build-mini-1"]["roles"] = ["dev-builds", "coffee"]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "unknown roles"):
                mf.load_manifest(path)

    def test_unknown_key_reference_is_refused(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["defaults"]["authorized_keys"]["allow"].append("ghost")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "unknown key 'ghost'"):
                mf.load_manifest(path)

    def test_node_ids_must_be_opaque_and_unique(self) -> None:
        for host, value, error in (("build-mini-1", "mac-3", "not an opaque cmux-"),
                                   ("build-mini-1", 3, "not an opaque cmux-"),
                                   ("build-mini-2", "cmux-mac-001", "share node_id cmux-mac-001")):
            data = copy.deepcopy(self.manifest)
            data["hosts"][host]["node_id"] = value
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "m.json"
                path.write_text(json.dumps(data))
                with self.assertRaisesRegex(mf.Failure, error):
                    mf.load_manifest(path)

    def test_overrides_merge_over_defaults(self) -> None:
        policy = mf.host_policy(self.manifest, "small-mini")
        self.assertEqual(policy["disk"]["min_free_gib"], 40)
        self.assertEqual(policy["macos"]["major"], 26)


class ClassAndPoolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def write(self, data: dict) -> Path:
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp)
        path = Path(tmp) / "m.json"
        path.write_text(json.dumps(data))
        return path

    def test_example_splits_hardware_from_class(self) -> None:
        host = self.manifest["hosts"]["build-mini-1"]
        self.assertEqual((host["hardware"], host["class"], host["availability"]), ("m4pro-48", "std", "dedicated"))
        self.assertIn("m4pro-48", self.manifest["hardware"])

    def test_legacy_classes_table_still_loads_as_hardware(self) -> None:
        data = json.loads(EXAMPLE.read_text())
        data["classes"] = data.pop("hardware")
        for host in data["hosts"].values():
            host["class"] = host.pop("hardware")
            host.pop("availability", None)
        loaded = mf.load_manifest(self.write(data))
        self.assertEqual(loaded["hosts"]["small-mini"]["hardware"], "m4-16")
        self.assertNotIn("class", loaded["hosts"]["small-mini"])

    def test_unknown_class_availability_and_hardware_are_refused(self) -> None:
        for field, value, message in (("class", "huge", "unknown class"),
                                      ("availability", "sometimes", "unknown availability"),
                                      ("hardware", "m9-1", "unknown hardware")):
            data = json.loads(EXAMPLE.read_text())
            data["hosts"]["build-mini-1"][field] = value
            with self.assertRaisesRegex(mf.Failure, message):
                mf.load_manifest(self.write(data))

    def test_hardware_drift_names_the_hardware(self) -> None:
        obs = observed(**{"build-mini-1": probe_text().replace("cpus\t14", "cpus\t12")})
        issues = mf.check(self.manifest, obs, ["build-mini-1"])
        self.assertTrue(any(i["area"] == "hardware" and "hardware m4pro-48" in i["detail"] for i in issues))

    def test_pools_count_declared_and_conforming_once_per_version(self) -> None:
        obs = observed(**{"build-mini-1": probe_text()})
        result = mf.pools(self.manifest, obs, list(self.manifest["hosts"]))
        # build-mini-2 lacks ci-runner and small-mini has no runner role, so only build-mini-1 counts.
        self.assertEqual(list(result), ["glaeda-std-xcode-26.3"])
        self.assertEqual(result["glaeda-std-xcode-26.3"]["declared"], ["build-mini-1"])
        self.assertEqual(result["glaeda-std-xcode-26.3"]["conforming_count"], 1)

    def test_pools_need_an_exact_real_xcode_to_conform(self) -> None:
        for text in (probe_text().replace("|dir|26.3|17C529", "|symlink:/x|26.3|17C529"),
                     probe_text().replace("|dir|26.3|17C529", "|dir|26.3|17C528")):
            result = mf.pools(self.manifest, observed(**{"build-mini-1": text}), ["build-mini-1"])
            self.assertEqual(result["glaeda-std-xcode-26.3"]["conforming"], [])
        self.assertEqual(mf.pools(self.manifest, None, ["build-mini-1"])["glaeda-std-xcode-26.3"]["conforming_count"], 0)

    def test_opportunistic_and_dev_members_are_not_pooled(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["build-mini-1"]["availability"] = "opportunistic"
        self.assertEqual(mf.pools(data, None, ["build-mini-1"]), {})
        data["hosts"]["build-mini-1"].update(availability="dedicated", **{"class": "dev"})
        self.assertEqual(mf.pools(data, None, ["build-mini-1"]), {})

    def test_classed_host_must_state_availability(self) -> None:
        data = json.loads(EXAMPLE.read_text())
        del data["hosts"]["build-mini-1"]["availability"]
        with self.assertRaisesRegex(mf.Failure, "class but no availability"):
            mf.load_manifest(self.write(data))

    def test_host_xcode_override_sets_its_pool(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["build-mini-1"]["overrides"] = {"xcode": {"apps": [
            {"path": "/Applications/Xcode_26.6.app", "version": "26.6", "build": "17F113"}]}}
        text = probe_text() + "xcode_app\t/Applications/Xcode_26.6.app|dir|26.6|17F113\n"
        result = mf.pools(data, observed(**{"build-mini-1": text}), ["build-mini-1"])
        self.assertEqual(list(result), ["glaeda-std-xcode-26.6"])
        self.assertEqual(result["glaeda-std-xcode-26.6"]["conforming"], ["build-mini-1"])

    def test_unreachable_host_is_declared_not_conforming(self) -> None:
        obs = {"hosts": {"build-mini-1": {"host": "build-mini-1", "reachable": False, "error": "timeout"}}}
        result = mf.pools(self.manifest, obs, ["build-mini-1"])
        self.assertEqual((result["glaeda-std-xcode-26.3"]["declared_count"],
                          result["glaeda-std-xcode-26.3"]["conforming_count"]), (1, 0))

    def test_pool_label_matches_the_runner_rule(self) -> None:
        self.assertEqual(mf.pool_label("std", "26.6"), "glaeda-std-xcode-26.6")


class BorrowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = copy.deepcopy(mf.load_manifest(EXAMPLE))
        # The public example's first mini is also a dev-build host. Developer borrowing deliberately
        # accepts only an ordinary CI member, which matches the production PR minis.
        self.manifest["hosts"]["build-mini-1"]["roles"] = ["ci-runner", "ios-simulators"]

    def obs(self, extra: str = "") -> dict:
        return observed(**{"build-mini-1": probe_text() + extra})

    def test_idle_ordinary_ci_member_is_a_candidate(self) -> None:
        got, excluded = mf.borrow_candidates(self.manifest, self.obs(), ["build-mini-1"], "std")
        self.assertEqual([row["host"] for row in got], ["build-mini-1"])
        self.assertEqual(excluded, {})
        self.assertEqual((got[0]["memory_gib"], got[0]["cpus"]), (48, 14))

    def test_busy_reserved_and_special_hosts_fail_closed(self) -> None:
        for extra, why in (
            ("runner_worker\trunning\n", "Runner.Worker running"),
            ("host_lock\theld\n", "host lock held"),
            (reservation_line(), "reserved"),
            (reservation_line(raw=b"garbage"), "invalid reservation"),
        ):
            got, excluded = mf.borrow_candidates(self.manifest, self.obs(extra), ["build-mini-1"], "std")
            self.assertEqual(got, [], extra)
            self.assertEqual(excluded["build-mini-1"], why)

        special = copy.deepcopy(self.manifest)
        special["hosts"]["build-mini-1"]["roles"].append("cache-host")
        got, excluded = mf.borrow_candidates(special, self.obs(), ["build-mini-1"], "std")
        self.assertEqual(got, [])
        self.assertIn("cache-host", excluded["build-mini-1"])

    def test_wrong_enrollment_and_hardware_are_not_candidates(self) -> None:
        for text, why in (
            (probe_text(node_id="cmux-mac-099"), "fleet enrollment does not match"),
            (probe_text().replace("memory_bytes\t51539607552", "memory_bytes\t17179869184"), "hardware memory_gib does not match"),
        ):
            got, excluded = mf.borrow_candidates(
                self.manifest, observed(**{"build-mini-1": text}), ["build-mini-1"], "std"
            )
            self.assertEqual(got, [])
            self.assertEqual(excluded["build-mini-1"], why)

    def test_dry_run_json_is_a_cmux_consumable_lease_document(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest_path = Path(tmp) / "manifest.json"
            manifest_path.write_text(json.dumps(self.manifest))
            obs_path = Path(tmp) / "observed.json"
            obs_path.write_text(json.dumps(self.obs()))
            with contextlib.redirect_stdout(io.StringIO()) as out:
                code = mf.main([
                    "borrow", "--manifest", os.fspath(manifest_path), "--host", "build-mini-1",
                    "--observed", os.fspath(obs_path), "--class", "std", "--for", "interactive development",
                    "--hours", "4", "--json",
                ])
        self.assertEqual(code, 0)
        doc = json.loads(out.getvalue())
        self.assertEqual((doc["schema"], doc["state"], doc["host"]), (
            "glaeda-developer-lease/v1", "dry_run", "build-mini-1"
        ))
        self.assertEqual(doc["connection"], {"kind": "ssh", "host": "build-mini-1", "user": "builder"})
        self.assertEqual(doc["reservation_schema"], mf.RESERVATION_SCHEMA)
        self.assertIsNone(doc["release"])

    OWNER = "leo@air+0123456789ab"

    def borrow(self, observed_doc: dict, hosts: list[str] | None = None, json_output: bool = False) -> tuple[int, str]:
        with mock.patch.object(mf.secrets, "token_hex", return_value="0123456789ab"), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            code = mf.cmd_borrow(self.manifest, hosts or ["build-mini-1"], observed_doc, "std", "interactive", 1,
                                 None, "leo@air", True, json_output)
        return code, out.getvalue()

    def two_hosts(self) -> dict:
        self.manifest["hardware"]["m4pro-64"] = dict(self.manifest["hardware"]["m4pro-48"], memory_gib=64)
        mini2 = self.manifest["hosts"]["build-mini-2"]
        mini2["roles"] = ["ci-runner"]
        mini2["hardware"] = "m4pro-64"
        text2 = probe_text(hostname="Build-Mini-2", node_id="cmux-mac-002").replace(
            f"memory_bytes\t{48 * 2**30}", f"memory_bytes\t{64 * 2**30}")
        return observed(**{"build-mini-1": probe_text(), "build-mini-2": text2})

    def test_each_borrow_gets_its_own_owner(self) -> None:
        first, second = mf.borrow_owner("leo@air"), mf.borrow_owner("leo@air")
        self.assertNotEqual(first, second)
        self.assertTrue(first.startswith("leo@air+"))
        self.assertLessEqual(len(mf.borrow_owner("a" * 100)), 100)

    def test_ranking_prefers_memory_then_host_name(self) -> None:
        got, excluded = mf.borrow_candidates(self.manifest, self.two_hosts(), ["build-mini-1", "build-mini-2"], "std")
        self.assertEqual([row["host"] for row in got], ["build-mini-2", "build-mini-1"])
        self.assertEqual(excluded, {})

    def test_same_operator_cannot_borrow_a_host_another_session_holds(self) -> None:
        # Session A holds build-mini-1 under its own lease owner; session B (same $USER@host) selected from an
        # older observation. B's reserve must refuse A's hold instead of extending it, and never write.
        now = int(time.time())
        held = {"valid": True, "owner": "leo@air+aaaaaaaaaaaa", "purpose": "interactive", "since": now - 5,
                "until": now + 3600}
        current = {"state": "active", "token": "t" * 64, "reservation": held}
        with mock.patch.object(mf, "read_reservation", return_value=current), \
                mock.patch.object(mf, "reservation_ssh") as ssh, \
                mock.patch.object(mf, "observe") as observe:
            code, out = self.borrow(self.obs())
        self.assertEqual(code, 1)
        ssh.assert_not_called()
        observe.assert_not_called()
        self.assertIn("no idle eligible std host", out)

    def test_reserve_failure_falls_through_to_the_next_candidate(self) -> None:
        obs = self.two_hosts()
        now = int(time.time())
        fresh = observed(**{"build-mini-1": probe_text() + reservation_line(
            owner=self.OWNER, purpose="interactive", since=now, until=now + 3600)})

        def reserve(manifest, hosts, *args):
            print(f"{hosts[0]}: refused: reserved by someone")
            return 1 if hosts == ["build-mini-2"] else 0

        with mock.patch.object(mf, "cmd_reserve", side_effect=reserve), \
                mock.patch.object(mf, "read_reservation", return_value={"state": "absent", "reservation": None}), \
                mock.patch.object(mf, "cmd_release") as release, \
                mock.patch.object(mf, "observe", return_value=fresh):
            code, out = self.borrow(obs, ["build-mini-1", "build-mini-2"], json_output=True)
        self.assertEqual(code, 0)
        doc = json.loads(out)
        self.assertEqual((doc["host"], doc["owner"]), ("build-mini-1", self.OWNER))
        self.assertEqual(doc["release"], ["glaeda-mini-fleet", "release", "build-mini-1", "--owner", self.OWNER, "--yes"])
        self.assertIn("reserved by someone", doc["selection"]["excluded"]["build-mini-2"])
        release.assert_not_called()

    def test_failed_reserve_drops_its_own_stray_hold(self) -> None:
        now = int(time.time())
        stray = {"state": "active", "reservation": {"valid": True, "owner": self.OWNER, "purpose": "interactive",
                                                    "since": now, "until": now + 3600}}
        with mock.patch.object(mf, "cmd_reserve", return_value=1), \
                mock.patch.object(mf, "read_reservation", return_value=stray), \
                mock.patch.object(mf, "cmd_release", return_value=0) as release:
            code, out = self.borrow(self.obs())
        self.assertEqual(code, 1)
        release.assert_called_once_with(self.manifest, ["build-mini-1"], self.OWNER, False, True)
        with mock.patch.object(mf, "cmd_reserve", return_value=1), \
                mock.patch.object(mf, "read_reservation", return_value=stray), \
                mock.patch.object(mf, "cmd_release", return_value=1), \
                self.assertRaisesRegex(mf.Failure, "hold may remain"):
            self.borrow(self.obs())

    def test_unreadable_host_is_skipped_without_a_hold_to_drop(self) -> None:
        with mock.patch.object(mf, "read_reservation", return_value={"state": "error", "error": "ssh exit 255"}), \
                mock.patch.object(mf, "reservation_ssh") as ssh, \
                mock.patch.object(mf, "cmd_release") as release:
            code, out = self.borrow(self.obs(), json_output=True)
        self.assertEqual(code, 1)
        ssh.assert_not_called()
        release.assert_not_called()
        self.assertIn("cannot read reservation", json.loads(out)["selection"]["excluded"]["build-mini-1"])

    def test_fresh_post_reservation_worker_race_releases_and_refuses(self) -> None:
        now = int(time.time())
        fresh = self.obs(
            reservation_line(owner=self.OWNER, purpose="interactive", since=now - 1, until=now + 3600)
            + "runner_worker\trunning\n"
        )
        with mock.patch.object(mf, "cmd_reserve", return_value=0), \
                mock.patch.object(mf, "cmd_release", return_value=0) as release, \
                mock.patch.object(mf, "observe", return_value=fresh):
            code, out = self.borrow(self.obs(), json_output=True)
        self.assertEqual(code, 1)
        release.assert_called_once_with(self.manifest, ["build-mini-1"], self.OWNER, False, True)
        doc = json.loads(out)
        self.assertEqual(doc["state"], "unavailable")
        self.assertIn("became busy", doc["selection"]["excluded"]["build-mini-1"])
        self.assertNotIn("excluded", doc)

    def test_release_failure_after_a_race_raises(self) -> None:
        fresh = self.obs("runner_worker\trunning\n")
        with mock.patch.object(mf, "cmd_reserve", return_value=0), \
                mock.patch.object(mf, "cmd_release", return_value=1), \
                mock.patch.object(mf, "observe", return_value=fresh), \
                self.assertRaisesRegex(mf.Failure, "could not be released"):
            self.borrow(self.obs())

    def test_unreachable_reprobe_leaves_the_reservation_and_says_so(self) -> None:
        fresh = {"hosts": {"build-mini-1": {"host": "build-mini-1", "reachable": False}}}
        with mock.patch.object(mf, "cmd_reserve", return_value=0), \
                mock.patch.object(mf, "cmd_release") as release, \
                mock.patch.object(mf, "observe", return_value=fresh), \
                self.assertRaisesRegex(mf.Failure, "left in place"):
            self.borrow(self.obs())
        release.assert_not_called()

    def test_fresh_post_reservation_idle_host_is_returned(self) -> None:
        now = int(time.time())
        fresh = self.obs(
            reservation_line(owner=self.OWNER, purpose="interactive", since=now - 1, until=now + 3600)
        )
        with mock.patch.object(mf, "cmd_reserve", return_value=0), \
                mock.patch.object(mf, "cmd_release", return_value=0) as release, \
                mock.patch.object(mf, "observe", return_value=fresh):
            code, out = self.borrow(self.obs(), json_output=True)
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out)["state"], "reserved")
        release.assert_not_called()

    def test_unavailable_json_uses_the_selection_shape(self) -> None:
        with contextlib.redirect_stdout(io.StringIO()) as out:
            code = mf.cmd_borrow(self.manifest, ["build-mini-1"], self.obs("host_lock\theld\n"), "std",
                                 "interactive", 1, None, "leo@air", True, True)
        self.assertEqual(code, 1)
        doc = json.loads(out.getvalue())
        self.assertEqual(doc["state"], "unavailable")
        self.assertEqual(doc["selection"]["excluded"], {"build-mini-1": "host lock held"})


class CheckTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def issues(self, text: str, host: str = "build-mini-1") -> list[dict]:
        return mf.check(self.manifest, observed(**{host: text}), [host])

    def test_conforming_host_has_only_pending(self) -> None:
        issues = self.issues(probe_text())
        self.assertEqual([i["area"] for i in issues], ["pending"])

    def test_unenrolled_host_is_pending_with_its_assigned_node_id(self) -> None:
        issues = [i for i in self.issues(probe_text(node_id=None)) if "enrolled" in i["detail"]]
        self.assertEqual([i["area"] for i in issues], ["pending"])
        self.assertIn("--node-id cmux-mac-001", issues[0]["fix"])

    def test_enrolled_under_another_node_id_is_drift(self) -> None:
        issues = [i for i in self.issues(probe_text(node_id="cmux-mac-009")) if i["area"] == "enrollment"]
        self.assertEqual(len(issues), 1)
        self.assertIn("enrolled as cmux-mac-009, manifest assigns cmux-mac-001", issues[0]["detail"])

    def test_host_without_node_id_is_not_checked_for_enrollment(self) -> None:
        issues = self.issues(probe_text(node_id=None), host="small-mini")
        self.assertFalse([i for i in issues if "enrolled" in i["detail"]])

    def test_probe_reads_the_enrollment_node_id_without_python(self) -> None:
        probe = mf.PROBE.read_text()
        self.assertIn("plutil -extract nodeId raw", probe)
        self.assertIn("glaeda/cmux-fleet/enrollment.json", probe)

    def test_missing_required_key_is_drift_with_add_action(self) -> None:
        issues = self.issues(probe_text(keys=("operator",)))
        add = [i for i in issues if i.get("action", {}).get("kind") == "add_key"]
        self.assertEqual(add[0]["action"]["key"], "coordinator")

    def test_unlisted_key_is_drift_but_never_an_action(self) -> None:
        issues = self.issues(probe_text(extra_keys=(STRAY,)))
        stray = [i for i in issues if "not allowed" in i["detail"]]
        self.assertEqual(len(stray), 1)
        self.assertNotIn("action", stray[0])

    def restricted(self, options: str = 'from="10.0.0.0/8",no-pty') -> None:
        self.manifest["keys"]["coordinator"]["options"] = options

    def test_declared_options_that_match_are_not_drift(self) -> None:
        self.restricted()
        digest = mf.options_digest('from="10.0.0.0/8",no-pty')
        text = probe_text(keys=("operator",)) + \
            f"ak_key\tauthorized_keys|{digest}|256 {FP['coordinator']} coordinator comment (ED25519)\n"
        self.assertFalse([i for i in self.issues(text) if i["area"] == "ssh"])

    def test_restricted_key_without_its_options_is_drift(self) -> None:
        self.restricted()
        for flag in ("no", mf.options_digest("no-pty")):
            text = probe_text(keys=("operator",)) + \
                f"ak_key\tauthorized_keys|{flag}|256 {FP['coordinator']} coordinator comment (ED25519)\n"
            ssh = [i for i in self.issues(text) if i["area"] == "ssh"]
            self.assertEqual(len(ssh), 1, ssh)
            self.assertIn("lacks its declared options", ssh[0]["detail"])

    def test_options_must_be_one_printable_field(self) -> None:
        for bad in ("", " no-pty", "no-pty\nssh-ed25519 AAAA x", 7, 'command="x', 'no-pty, from="x"'):
            data = copy.deepcopy(self.manifest)
            data["keys"]["coordinator"]["options"] = bad
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "m.json"
                path.write_text(json.dumps(data))
                with self.assertRaisesRegex(mf.Failure, "options must be"):
                    mf.load_manifest(path)

    def test_allowed_review_key_is_not_drift(self) -> None:
        issues = self.issues(probe_text(keys=("coordinator", "operator", "service-cache")))
        self.assertFalse([i for i in issues if i["area"] == "ssh"])

    def test_missing_clone_carries_exact_source_identity(self) -> None:
        issues = self.issues(probe_text(clone=False))
        action = next(i["action"] for i in issues if i.get("action", {}).get("kind") == "clone_xcode")
        self.assertEqual(action, {"kind": "clone_xcode", "path": "/Applications/Xcode_26.3.app",
                                  "source": "/Applications/Xcode.app", "version": "26.3", "build": "17C529"})

    def test_licence_and_first_launch_come_from_xcodebuild(self) -> None:
        base = probe_text(license_="26.0")
        ok = base + "xcode_ready\t/Applications/Xcode.app|accepted|done\n" \
            + "xcode_ready\t/Applications/Xcode_26.3.app|accepted|done\n"
        self.assertFalse([i for i in self.issues(ok) if "licence" in i["detail"] or "first launch" in i["detail"]])
        bad = base + "xcode_ready\t/Applications/Xcode.app|accepted|done\n" \
            + "xcode_ready\t/Applications/Xcode_26.3.app|needed|needed\n"
        details = [i["detail"] for i in self.issues(bad)]
        self.assertIn("/Applications/Xcode_26.3.app licence not accepted", details)
        self.assertIn("/Applications/Xcode_26.3.app first launch not run", details)
        partial = base + "xcode_ready\t/Applications/Xcode.app|accepted|done\n"
        self.assertTrue([i for i in self.issues(partial) if "no runnable xcodebuild" in i["detail"]])
        self.assertEqual(mf.parse_probe("xcode_ready\t/Applications/X|y.app|accepted|done\n")["xcode_ready"],
                         {"/Applications/X|y.app": {"licence": "accepted", "first_launch": "done"}})

    def test_newer_licence_covers_older_xcode(self) -> None:
        self.assertFalse([i for i in self.issues(probe_text(license_="26.5")) if "licence" in i["detail"]])
        self.assertTrue([i for i in self.issues(probe_text(license_="")) if "licence" in i["detail"]])

    def test_hostname_and_launchd_drift(self) -> None:
        issues = self.issues(probe_text(hostname="other", worker="not-loaded"))
        self.assertTrue(any(i["area"] == "identity" for i in issues))
        self.assertTrue(any(i["area"] == "launchd" and "not-loaded" in i["detail"] for i in issues))

    def test_key_in_authorized_keys2_is_live(self) -> None:
        text = probe_text() + f"ak_key\tauthorized_keys2|no|256 {STRAY} hidden (ED25519)\n"
        details = [i["detail"] for i in self.issues(text)]
        self.assertTrue(any("not allowed" in d for d in details))
        self.assertTrue(any("authorized_keys2" in d for d in details))

    def test_backup_copies_are_not_live(self) -> None:
        text = probe_text() + f"ak_key\tauthorized_keys.bak|no|256 {STRAY} old (ED25519)\n"
        self.assertFalse([i for i in self.issues(text) if "not allowed" in i["detail"]])

    def test_symlinked_xcode_is_drift(self) -> None:
        text = probe_text(clone=False) + "xcode_app\t/Applications/Xcode_26.3.app|symlink:Xcode.app|26.3|17C529\n"
        self.assertTrue([i for i in self.issues(text) if "real directory" in i["detail"]])

    def test_sudo_and_token_drift(self) -> None:
        self.manifest["hosts"]["build-mini-1"]["sudo"] = "password"
        self.manifest["defaults"]["controller_token"] = "present"
        text = probe_text() + "sudo\tnopasswd\ncontroller_token\tmissing\n"
        areas = {i["area"] for i in self.issues(text)}
        self.assertTrue({"sudo", "worker"} <= areas)

    def test_versions_compare_padded(self) -> None:
        self.assertEqual(mf.version_tuple("26.3"), mf.version_tuple("26.3.0"))
        self.assertLess(mf.version_tuple("26.4"), mf.version_tuple("26.5"))
        self.assertEqual(mf.version_tuple(""), (0, 0, 0))

    def test_unreachable_host_is_one_finding(self) -> None:
        issues = mf.check(self.manifest, {"hosts": {"build-mini-1": {"reachable": False, "error": "timeout"}}},
                          ["build-mini-1"])
        self.assertEqual([(i["area"], i["detail"]) for i in issues], [("reach", "timeout")])

    def test_exit_code_ignores_pending(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            obs = Path(tmp) / "obs.json"
            obs.write_text(json.dumps(observed(**{"build-mini-1": probe_text()})))
            with contextlib.redirect_stdout(io.StringIO()) as out:
                code = mf.main(["check", "--manifest", os.fspath(EXAMPLE), "--host", "build-mini-1",
                                "--observed", os.fspath(obs)])
        self.assertEqual(code, 0, out.getvalue())
        self.assertIn("ok (1 pending)", out.getvalue())


class DiskAgentTests(unittest.TestCase):
    """The glaeda-disk agents run on timers: loaded and idle is healthy, a failed run is pending, unloaded is drift."""

    def launchd_issues(self, state: str) -> list[tuple[str, str]]:
        manifest = mf.load_manifest(EXAMPLE)
        label = "com.teamleaderleo.glaeda.disk-pressure"
        manifest["defaults"]["launchd"]["running"] = [label]
        text = probe_text() + f"launchd\t~/Library/LaunchAgents|{label}|{state}\n"
        return [(i["area"], i["detail"]) for i in mf.check(manifest, observed(**{"build-mini-1": text}), ["build-mini-1"])
                if label in i["detail"]]

    def test_idle_between_runs_is_ok(self) -> None:
        self.assertEqual(self.launchd_issues("not running last_exit=0"), [])

    def test_failed_run_is_pending_and_unloaded_is_drift(self) -> None:
        self.assertEqual(self.launchd_issues("not running last_exit=2"),
                         [("pending", "com.teamleaderleo.glaeda.disk-pressure last exited 2; loaded, runs on its timer")])
        self.assertEqual(self.launchd_issues("not-loaded"),
                         [("launchd", "com.teamleaderleo.glaeda.disk-pressure is not-loaded")])


FILL = "ai.manaflow.cmux-build-catch-up-fill"


class CatchUpTests(unittest.TestCase):
    """The fleet compile-cache catch-up roles: one writer that fills, dev-build workers that read."""

    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        writer = self.manifest["hosts"]["build-mini-2"]
        writer["roles"] = ["catch-up-writer"]
        writer["overrides"] = {"launchd": {"running": [FILL]}}

    def writer(self, fill: str | None = "not running last_exit=0", worker_proc: str = "absent",
               fleet_cas: str | None = None) -> list[tuple[str, str]]:
        text = probe_text(hostname="Build-Mini-2", node_id="cmux-mac-002", worker_proc=worker_proc,
                          fleet_cas=fleet_cas_text(mode="signing") if fleet_cas is None else fleet_cas)
        if fill is not None:
            text += f"launchd\t/Library/LaunchDaemons|{FILL}|{fill}\n"
        issues = mf.check(self.manifest, observed(**{"build-mini-2": text}), ["build-mini-2"])
        return [(i["area"], i["detail"]) for i in issues if i["area"] != "pending"]

    def reader(self, fleet_cas: str, pending: bool = False) -> list[dict]:
        issues = mf.check(self.manifest, observed(**{"build-mini-1": probe_text(fleet_cas=fleet_cas)}), ["build-mini-1"])
        if pending:
            return [i for i in issues if i["area"] == "pending" and "cmux-build-fleet" in i["detail"]]
        return [i for i in issues if i["area"] != "pending"]

    def test_writer_ok_between_fill_ticks_with_a_drained_worker(self) -> None:
        # The worker daemon stays loaded (its wrapper execs /bin/sleep); only bin/worker running counts.
        self.assertEqual(self.writer(), [])
        self.assertEqual(self.writer(fill="running pid=4242"), [])

    def test_writer_running_the_hq_worker_is_drift(self) -> None:
        (issue,) = self.writer(worker_proc="running")
        self.assertEqual(issue[0], "catch-up")
        self.assertIn("dev-build worker", issue[1])

    def test_writer_without_its_fill_daemon_is_drift(self) -> None:
        self.assertEqual(self.writer(fill=None), [("launchd", f"{FILL} not installed")])
        self.assertEqual(self.writer(fill="not-loaded"), [("launchd", f"{FILL} is not-loaded")])
        # A writer whose manifest omits the fill from launchd.running is still held to it.
        self.manifest["hosts"]["build-mini-2"]["overrides"] = {}
        self.manifest["defaults"]["launchd"]["running"] = []
        issues = mf.check(self.manifest, observed(**{"build-mini-2": probe_text(
            hostname="Build-Mini-2", node_id="cmux-mac-002", worker_proc="absent",
            fleet_cas=fleet_cas_text(mode="signing"))}), ["build-mini-2"])
        issues = [i for i in issues if i["area"] != "pending"]
        self.assertEqual([(i["area"], i["detail"]) for i in issues], [("launchd", f"{FILL} not installed")])
        self.assertIn("install-catch-up-fill.sh", issues[0]["fix"])

    def test_writer_needs_its_build_wrapper_and_the_recipe(self) -> None:
        (issue,) = self.writer(fleet_cas=fleet_cas_text(mode="signing", bin_fleet_cas_writer_build_sh="missing"))
        self.assertEqual(issue, ("fleet-cas", f"{mf.FLEET_ROOT}/xcode/bin/fleet-cas-writer-build.sh missing"))
        (issue,) = self.writer(fleet_cas=fleet_cas_text(mode="signing", build_mode="missing"))
        self.assertEqual(issue[0], "recipe")

    def test_failing_fill_is_pending_not_hidden(self) -> None:
        text = probe_text(hostname="Build-Mini-2", node_id="cmux-mac-002", worker_proc="absent",
                          fleet_cas=fleet_cas_text(mode="signing"))
        text += f"launchd\t/Library/LaunchDaemons|{FILL}|not running last_exit=1\n"
        issues = mf.check(self.manifest, observed(**{"build-mini-2": text}), ["build-mini-2"])
        self.assertIn(("pending", f"{FILL} last exited 1; loaded, runs on its timer"),
                      [(i["area"], i["detail"]) for i in issues])
        self.assertEqual(self.writer(fill="not running last_exit=1"), [])

    def test_sudoers_rule_leaves_the_fill_timer_out(self) -> None:
        self.manifest["hosts"]["build-mini-2"]["overrides"] = {"launchd": {"running": [FILL, "com.cmux.caffeinate"]}}
        rule = mf.sudoers_rule(self.manifest, "build-mini-2")
        self.assertIn("com.cmux.caffeinate", rule)
        self.assertNotIn(FILL, rule)

    def test_writer_node_must_sign(self) -> None:
        self.assertEqual(self.writer(fleet_cas=fleet_cas_text(mode="read-only")),
                         [("fleet-cas", "the node is read-only (--read-only-kv); a catch-up-writer node runs --sign-key")])

    def test_reader_ok(self) -> None:
        self.assertEqual(self.reader(fleet_cas_text()), [])

    def test_symlinked_fixed_path_is_drift_and_never_a_reclone(self) -> None:
        for kind in ("src_cmux", "DerivedData", "fleet_cas_sock", "src"):
            (issue,) = self.reader(fleet_cas_text(**{kind: "symlink"}))
            self.assertEqual(issue["area"], "fleet-cas", kind)
            self.assertIn("is a symlink", issue["detail"])
            self.assertNotIn("action", issue)
            self.assertIn("never delete or re-clone", issue["fix"])
        (issue,) = self.reader(fleet_cas_text(DerivedData="file"))
        self.assertEqual(issue["detail"], f"{mf.FLEET_ROOT}/xcode/DerivedData is a file, want a dir")
        # Missing (fresh host, install --reset) is pending until the first catch-up build, not drift.
        self.assertEqual(self.reader(fleet_cas_text(src_cmux="missing")), [])
        (issue,) = self.reader(fleet_cas_text(src_cmux="missing"), pending=True)
        self.assertEqual(issue["fix"], "the next catch-up build creates it")

    def test_reader_catch_up_disabled_is_info_not_drift(self) -> None:
        text = fleet_cas_text() + "fleet_catch_up\t0\n"
        (issue,) = self.reader(text)
        self.assertEqual(issue["area"], "disabled")
        self.assertIn("disabled", mf.INFO_AREAS)  # so it never counts as drift
        self.assertIn("catch-up disabled (CMUX_CI_CATCH_UP=0", issue["detail"])
        self.assertEqual(self.reader(fleet_cas_text() + "fleet_catch_up\t1\n"), [])

    def test_reader_node_not_read_only_is_drift(self) -> None:
        for mode, got in (("signing", "the node signs (--sign-key)"), ("other", "the node has neither flag"),
                          ("missing", "no xcode/run/node.args"), ("unreadable", "xcode/run/node.args is unreadable")):
            (issue,) = self.reader(fleet_cas_text(mode=mode))
            self.assertEqual((issue["area"], issue["detail"]), ("fleet-cas", f"{got}; a reader node runs --read-only-kv"))

    def test_reader_node_that_does_not_answer_is_drift(self) -> None:
        (issue,) = self.reader(fleet_cas_text(node="down"))
        self.assertIn("does not answer", issue["detail"])
        (issue,) = self.reader(fleet_cas_text(**{"bin_fleet_cas_settings_sh": "missing"}))
        self.assertIn("fleet-cas-settings.sh missing", issue["detail"])

    def test_reader_recipe_without_build_mode_is_drift(self) -> None:
        (issue,) = self.reader(fleet_cas_text(build_mode="missing"))
        self.assertEqual((issue["area"], issue["detail"]),
                         ("recipe", "released recipe 559403ae3d50 has no cmux-build-mode.py, so no catch-up mode"))
        (issue,) = self.reader(fleet_cas_text(build_mode=None))
        self.assertEqual(issue["detail"], f"no released recipe at {mf.FLEET_ROOT}/recipes")

    def test_host_without_a_fleet_root_is_one_finding(self) -> None:
        (issue,) = self.reader("")
        self.assertIn("no fleet-cas observation", issue["detail"])

    def test_other_roles_are_not_held_to_catch_up(self) -> None:
        self.manifest["hosts"]["build-mini-1"]["roles"] = ["ci-runner"]
        self.assertEqual(self.reader(""), [])

    def test_manifest_refuses_a_writer_that_is_also_a_worker(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            for roles, running in ((["catch-up-writer", "dev-builds"], [FILL]),
                                   (["catch-up-writer"], [FILL, "ai.manaflow.cmux-build-worker"])):
                data = json.loads(EXAMPLE.read_text())
                data["hosts"]["build-mini-2"]["roles"] = roles
                data["hosts"]["build-mini-2"]["overrides"] = {"launchd": {"running": running}}
                with self.assertRaisesRegex(mf.Failure, "catch-up-writer", msg=roles):
                    mf.load_manifest(write_manifest(tmp, data))
            data["hosts"]["build-mini-2"]["roles"] = ["catch-up-writer", "nightly", "ci-runner"]
            data["hosts"]["build-mini-2"]["overrides"] = {"launchd": {"running": [FILL]}}
            with self.assertRaisesRegex(mf.Failure, "trustedRef"):
                mf.load_manifest(write_manifest(tmp, data))
            # cmuxterm-hq#623's cmux15: a trusted-only runner beside the writer.
            data["hosts"]["build-mini-2"]["overrides"]["runner"] = {"trustedRef": "refs/heads/main",
                                                                    "trustedRepo": "manaflow-ai/cmux"}
            self.assertIn("catch-up-writer", mf.load_manifest(write_manifest(tmp, data))["hosts"]["build-mini-2"]["roles"])
            data["hosts"]["small-mini"]["roles"] = ["catch-up-writer"]
            with self.assertRaisesRegex(mf.Failure, "one writer"):
                mf.load_manifest(write_manifest(tmp, data))

    def test_probe_reports_kinds_and_modes_only(self) -> None:
        text = mf.PROBE.read_text()
        for key in ("fleet_cas_path", "fleet_cas_node_mode", "fleet_cas_node", "fleet_recipe_build_mode"):
            self.assertIn(f"e {key}", text)
        # Read-only: nothing in the fleet-cas section is removed, moved, cloned or written, no file under the
        # fleet root is run, and every child that could read stdin gets /dev/null (the probe arrives on stdin).
        start = text.index("# Fleet compile cache")
        body = [line for line in text[start:text.index("# Reservation marker")].splitlines()
                if not line.strip().startswith("#")]
        for line in body:
            self.assertNotRegex(line, r"(?<![\w.$-])(rm|mv|git|mkdir|touch|ln|cp|bash|sh|source|install)(?=\s)|(^|[;&|] *)\.\s|(^|\s)\d?>>?\s*(?!/dev/null|&)\S", line)
            self.assertNotRegex(line, r'(^|[;&|] *)"\$[A-Z]', line)
            if re.search(r"\b(grep|perl)\b", line):
                self.assertTrue("</dev/null" in line or "</dev/null" in body[body.index(line) + 2], line)
        parsed = mf.parse_probe(fleet_cas_text(src_cmux="symlink"))["fleet_cas"]
        self.assertEqual(parsed["paths"]["xcode/src/cmux"], "symlink")
        self.assertEqual((parsed["node_mode"], parsed["node"], parsed["build_mode"]), ("read-only", "answers", "present"))


class IosSimulatorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        self.manifest["defaults"]["ios_simulator"] = {"runtimes": ["23F77"], "exclusive": True,
                                                      "devices": ["iPhone 17 Pro Max"]}

    def ios(self, extra: str) -> list[str]:
        obs = observed(**{"build-mini-1": probe_text() + extra})
        return [i["detail"] for i in mf.check(self.manifest, obs, ["build-mini-1"]) if i["area"] == "ios"]

    def test_exact_runtime_and_device_conform(self) -> None:
        text = ("ios_runtime\t26.5|26.5|23F77|com.apple.CoreSimulator.SimRuntime.iOS-26-5\n"
                "ios_device\t26.5|iPhone 17 Pro Max\nios_device\t26.5|iPad (A16)\n")
        self.assertEqual(self.ios(text), [])

    def test_missing_extra_and_deviceless_runtimes_drift(self) -> None:
        text = ("ios_runtime\t26.3|26.3.1|23D8133|com.apple.CoreSimulator.SimRuntime.iOS-26-3\n"
                "ios_device\t26.3|iPhone 17 Pro Max\n")
        self.assertEqual(self.ios(text), ["iOS simulator runtime 23F77 missing",
                                          "extra iOS simulator runtime 26.3.1 (23D8133)"])
        self.manifest["defaults"]["ios_simulator"]["exclusive"] = False
        self.assertEqual(self.ios(text), ["iOS simulator runtime 23F77 missing"])
        bare = "ios_runtime\t26.5|26.5|23F77|com.apple.CoreSimulator.SimRuntime.iOS-26-5\n"
        self.assertEqual(self.ios(bare), ["no available iPhone 17 Pro Max simulator on iOS 26.5"])

    def test_the_fix_names_the_runtime_identifier(self) -> None:
        text = ("ios_runtime\t26.5|26.5|23F77|com.apple.CoreSimulator.SimRuntime.iOS-26-5\n"
                "ios_device\t26.5|iPhone 17 Pro Max\n"
                "ios_runtime\t27.0|27.0|24A5390f|com.apple.CoreSimulator.SimRuntime.iOS-27-0\n")
        obs = observed(**{"build-mini-1": probe_text() + text})
        fixes = [i["fix"] for i in mf.check(self.manifest, obs, ["build-mini-1"]) if i["area"] == "ios"]
        self.assertEqual(fixes, ["xcrun simctl runtime delete 24A5390f"])

    def test_unavailable_and_malformed_runtimes(self) -> None:
        text = ("ios_runtime\t26.5|26.5|23F77|com.apple.CoreSimulator.SimRuntime.iOS-26-5|unavailable\n"
                "ios_device\t26.5|iPhone 17 Pro Max\n"
                "ios_runtime\t26.0|26.0|x;rm -rf|com.apple.CoreSimulator.SimRuntime.iOS-26-0\n")
        # An unusable declared runtime counts as missing; a malformed line is dropped, never shown.
        self.assertEqual(self.ios(text), ["iOS simulator runtime 23F77 missing"])
        stale = ("ios_runtime\t26.5|26.5|23F77|com.apple.CoreSimulator.SimRuntime.iOS-26-5\n"
                 "ios_device\t26.5|iPhone 17 Pro Max\n"
                 "ios_runtime\t17.0|17.0|21A328|com.apple.CoreSimulator.SimRuntime.iOS-17-0|unavailable\n")
        self.assertEqual(self.ios(stale), ["extra iOS simulator runtime 17.0 (21A328)"])

    def test_declaration_is_validated(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            for bad in ({"runtimes": ["26.5"]}, {"runtimes": ["23F77"], "devices": ["iPhone|x"]},
                        {"exclusive": True}, {"runtimes": ["23F77"], "extra": 1}):
                data = json.loads(EXAMPLE.read_text())
                data["defaults"]["ios_simulator"] = bad
                with self.assertRaises(mf.Failure, msg=bad):
                    mf.load_manifest(write_manifest(tmp, data))

    def test_probe_emits_runtimes_and_devices(self) -> None:
        text = mf.PROBE.read_text()
        self.assertIn("ios_runtime", text)
        parsed = mf.parse_probe("ios_runtime\t27.0|27.0|24A5390f|com.apple.CoreSimulator.SimRuntime.iOS-27-0\n"
                                "ios_device\t27.0|iPad Pro 13-inch (M5)\n")
        self.assertEqual(parsed["ios_runtimes"][0]["build"], "24A5390f")
        self.assertEqual(parsed["ios_devices"], [{"runtime": "27.0", "name": "iPad Pro 13-inch (M5)"}])


class ApplyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def run_apply(self, hosts: list[str], yes: bool, texts: dict[str, str]) -> tuple[int, str, list]:
        calls: list = []
        with mock.patch.object(mf, "observe", lambda manifest, names: observed(**{n: texts[n] for n in names})), \
                mock.patch.object(mf, "remote", lambda *a: calls.append(a) or (0, "done")), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            code = mf.apply(self.manifest, hosts, yes)
        return code, out.getvalue(), calls

    def test_dry_run_runs_nothing(self) -> None:
        code, out, calls = self.run_apply(["build-mini-1"], False, {"build-mini-1": probe_text(keys=("operator",), clone=False)})
        self.assertEqual((code, calls), (0, []))
        self.assertIn("would add key coordinator", out)
        self.assertIn("would clone", out)

    def test_apply_false_host_is_skipped(self) -> None:
        code, out, calls = self.run_apply(["small-mini"], True, {"small-mini": probe_text(hostname="small-mini", clone=False)})
        self.assertEqual(calls, [])
        self.assertIn("skipped", out)

    def test_add_key_sends_public_key_and_fingerprint(self) -> None:
        text = probe_text(keys=("operator",))
        _code, _out, calls = self.run_apply(["build-mini-1"], True, {"build-mini-1": text})
        (_name, _user, script, args, stdin), = calls
        self.assertIs(script, mf.ADD_KEY)
        self.assertEqual(args, [FP["coordinator"]])
        self.assertTrue(stdin.startswith("ssh-ed25519 "))

    def test_add_key_writes_declared_options_first(self) -> None:
        self.manifest["keys"]["coordinator"]["options"] = 'from="10.0.0.0/8",no-pty'
        _code, _out, calls = self.run_apply(["build-mini-1"], True, {"build-mini-1": probe_text(keys=("operator",))})
        (_name, _user, _script, _args, stdin), = calls
        self.assertTrue(stdin.startswith('from="10.0.0.0/8",no-pty ssh-ed25519 '))

    def test_key_without_public_key_is_not_added(self) -> None:
        self.manifest["keys"]["coordinator"].pop("public_key")
        code, out, calls = self.run_apply(["build-mini-1"], True, {"build-mini-1": probe_text(keys=("operator",))})
        self.assertEqual(calls, [])
        self.assertEqual(code, 1)
        self.assertIn("no public_key", out)

    def test_unreachable_host_fails_apply(self) -> None:
        with mock.patch.object(mf, "observe", lambda m, n: {"hosts": {x: {"reachable": False, "error": "down"} for x in n}}), \
                mock.patch.object(mf, "remote") as remote, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(mf.apply(self.manifest, ["build-mini-1"], True), 1)
        remote.assert_not_called()

    def test_unverified_effect_fails_apply(self) -> None:
        texts = iter([observed(**{"build-mini-1": probe_text(keys=("operator",))}),
                      {"hosts": {"build-mini-1": {"reachable": False, "error": "gone"}}}])
        with mock.patch.object(mf, "observe", lambda m, n: next(texts)), \
                mock.patch.object(mf, "remote", return_value=(0, "added")), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(mf.apply(self.manifest, ["build-mini-1"], True), 1)

    def test_stray_keys_are_never_removed(self) -> None:
        _code, _out, calls = self.run_apply(["build-mini-1"], True, {"build-mini-1": probe_text(extra_keys=(STRAY,))})
        self.assertEqual(calls, [])


@unittest.skipUnless(shutil.which("ssh-keygen") and shutil.which("bash"), "needs ssh-keygen and bash")
class AddKeyScriptTests(unittest.TestCase):
    def test_append_is_idempotent_backed_up_and_fingerprint_bound(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / ".ssh").mkdir()
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "old", "-f", os.fspath(home / "old")], check=True)
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "new", "-f", os.fspath(home / "new")], check=True)
            ak = home / ".ssh" / "authorized_keys"
            ak.write_text((home / "old.pub").read_text().rstrip("\n"))  # no trailing newline
            ak.chmod(0o600)
            new = (home / "new.pub").read_text()
            fp = subprocess.run(["ssh-keygen", "-lf", os.fspath(home / "new.pub")], capture_output=True, text=True).stdout.split()[1]

            def run(fingerprint: str) -> subprocess.CompletedProcess:
                return subprocess.run(["bash", "-c", mf.ADD_KEY, "glaeda", fingerprint], input=new, text=True,
                                      capture_output=True, env={"HOME": tmp, "PATH": os.environ["PATH"]})

            self.assertEqual(run("SHA256:wrong").returncode, 3)
            first = run(fp)
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertIn("added", first.stdout)
            self.assertIn("unchanged", run(fp).stdout)
            self.assertEqual(len(ak.read_text().splitlines()), 2)
            self.assertEqual(ak.stat().st_mode & 0o777, 0o600)
            self.assertEqual(sorted(p.name for p in (home / ".ssh").iterdir()), ["authorized_keys"])
            self.assertEqual(len(list((home / ".local/state/glaeda/mini-fleet").glob("authorized_keys.*"))), 1)

    def test_missing_file_is_created_private(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "new", "-f", os.fspath(home / "new")], check=True)
            fp = subprocess.run(["ssh-keygen", "-lf", os.fspath(home / "new.pub")], capture_output=True, text=True).stdout.split()[1]
            proc = subprocess.run(["bash", "-c", mf.ADD_KEY, "glaeda", fp], input=(home / "new.pub").read_text(), text=True,
                                  capture_output=True, env={"HOME": tmp, "PATH": os.environ["PATH"]})
            self.assertEqual(proc.returncode, 0, proc.stderr)
            ak = home / ".ssh" / "authorized_keys"
            self.assertEqual(len(ak.read_text().splitlines()), 1)
            self.assertEqual((ak.stat().st_mode & 0o777, (home / ".ssh").stat().st_mode & 0o777), (0o600, 0o700))


@unittest.skipUnless(sys.platform == "darwin", "cp -c, plutil and stat -f are macOS")
class CloneScriptTests(unittest.TestCase):
    def fake_app(self, root: Path, version: str, build: str) -> Path:
        app = root / "X.app" / "Contents"
        app.mkdir(parents=True)
        subprocess.run(["plutil", "-create", "xml1", os.fspath(app / "Info.plist")], check=True)
        subprocess.run(["plutil", "-insert", "CFBundleShortVersionString", "-string", version, os.fspath(app / "Info.plist")], check=True)
        subprocess.run(["plutil", "-create", "xml1", os.fspath(app / "version.plist")], check=True)
        subprocess.run(["plutil", "-insert", "ProductBuildVersion", "-string", build, os.fspath(app / "version.plist")], check=True)
        return root / "X.app"

    def run_clone(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(["bash", "-c", mf.CLONE_XCODE, "glaeda", *args], capture_output=True, text=True)

    def test_clone_matches_identity_and_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            src = self.fake_app(Path(tmp), "26.3", "17C529")
            dst = os.fspath(Path(tmp) / "Y.app")
            self.assertEqual(self.run_clone(os.fspath(src), dst, "26.6", "17F113").returncode, 3)
            first = self.run_clone(os.fspath(src), dst, "26.3", "17C529")
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertTrue(Path(dst).is_dir() and not Path(dst).is_symlink())
            self.assertIn("unchanged", self.run_clone(os.fspath(src), dst, "26.3", "17C529").stdout)

    def test_dangling_symlink_destination_is_left_alone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            src = self.fake_app(Path(tmp), "26.3", "17C529")
            dst = Path(tmp) / "Y.app"
            dst.symlink_to(Path(tmp) / "gone")
            self.assertIn("unchanged", self.run_clone(os.fspath(src), os.fspath(dst), "26.3", "17C529").stdout)
            self.assertFalse((Path(tmp) / "Y.app.glaeda-partial").exists())


class ProbeScriptTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("ssh-keygen"), "needs ssh-keygen")
    def test_probe_reports_every_live_key_including_an_unterminated_last_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / ".ssh").mkdir()
            pubs = []
            for name in ("a", "b", "c"):
                subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", name, "-f", os.fspath(home / name)], check=True)
                pubs.append((home / f"{name}.pub").read_text().strip())
            (home / ".ssh" / "authorized_keys").write_text(pubs[0] + "\n" + pubs[1])  # no trailing newline
            (home / ".ssh" / "authorized_keys2").write_text(pubs[2] + "\n")
            out = subprocess.run(["bash", os.fspath(ROOT / "scripts" / "cmux_mini_probe.sh")], capture_output=True,
                                 text=True, env={"HOME": tmp, "PATH": "/usr/bin:/bin"}).stdout
            parsed = mf.parse_probe(out)
            self.assertEqual(sorted((k["file"], k["comment"]) for k in parsed["authorized_keys"]),
                             [("authorized_keys", "a"), ("authorized_keys", "b"), ("authorized_keys2", "c")])
            self.assertNotIn("AAAA", out)  # fingerprints only, never key material

    def test_probe_reports_an_options_digest_never_the_options(self) -> None:
        options = 'from="172.20.21.0/24",command="/usr/local/bin/probe",no-pty,no-port-forwarding'
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / ".ssh").mkdir()
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "agent", "-f", os.fspath(home / "a")], check=True)
            (home / ".ssh" / "authorized_keys").write_text(f"{options} {(home / 'a.pub').read_text().strip()}\n")
            out = subprocess.run(["bash", os.fspath(ROOT / "scripts" / "cmux_mini_probe.sh")], capture_output=True,
                                 text=True, env={"HOME": tmp, "PATH": "/usr/bin:/bin"}).stdout
            (key,) = mf.parse_probe(out)["authorized_keys"]
            self.assertEqual(key["options"], mf.options_digest(options))
            self.assertNotIn("172.20.21", out)
            self.assertNotIn("AAAA", out)

    def test_probe_parses_as_bash(self) -> None:
        subprocess.run(["bash", "-n", os.fspath(ROOT / "scripts" / "cmux_mini_probe.sh")], check=True)

    def test_probe_never_reads_tokens_or_runs_privileged_commands(self) -> None:
        text = (ROOT / "scripts" / "cmux_mini_probe.sh").read_text()
        secret_lines = [line.strip() for line in text.splitlines() if "secrets/" in line and not line.strip().startswith("#")]
        self.assertEqual(secret_lines, ['[ -f "$F/secrets/controller.token" ] && e controller_token present || e controller_token missing'])
        sudo_lines = [line.strip() for line in text.splitlines() if "sudo" in line and not line.strip().startswith("#")]
        self.assertEqual(sudo_lines, ["if sudo -n -l >/dev/null 2>&1; then e sudo nopasswd; else e sudo password; fi"])



READY_XCODE = ("xcode_ready\t/Applications/Xcode.app|accepted|done\n"
               "xcode_ready\t/Applications/Xcode_26.3.app|accepted|done\n")


def preflight_text(user: str = "builder", brew_owner: str | None = "builder", zig: str = "0.16.0",
                   zig_path: str = "/opt/homebrew/bin/zig", rust: bool = True, metal: bool = True,
                   sdks: str = "ok", python: str = "/opt/homebrew/bin/python3.13|3.13",
                   submodules: tuple[str, ...] = (" a1 ghostty (heads/main)",), artifacts: bool = True,
                   dirty: int = 0, candidate: str | None = "59ca9c9bd1bb|yes|yes", enroll_state: str | None = "eligible",
                   acceptance: str | None = "accepted", update_running: str | None = None, prepared: str | None = None,
                   sleep: int = 0, runners: tuple[str, ...] = ("actions-runner-cmux-persistent-compile|mini-1",),
                   candidate_generation: str | None = None, enroll_generation: str | None = None,
                   enroll_reason: str | None = None, python3: str | None = "/opt/homebrew/bin/python3|3.13",
                   brew_python3: bool = True, glaeda_lacking: str | None = "", glaeda_dirty: int = 0,
                   pins: tuple[str, ...] = ("rustup|pinned", "zig|pinned", "python@3.13|pinned"),
                   brew_dir: str | None = None, gh: str | None = "/opt/homebrew/bin/gh", secrets: tuple[str, ...] = (),
                   **probe: object) -> str:
    """A probe plus preflight section for a host that is ready unless told otherwise."""
    lines = [probe_text(**probe).rstrip("\n").replace("user\tbuilder", f"user\t{user}"), READY_XCODE.rstrip("\n")]
    lines.append(f"pf_sdks\t{sdks}")
    lines.append("pf_metal\t" + ("ok|Apple metal version 32023.883" if metal else
                                 "fail|error: cannot execute tool 'metal' due to missing Metal Toolchain"))
    tools = {"git": ("/usr/bin/git", "git version 2.50.1 (Apple Git-155)"),
             "xcodebuild": ("/usr/bin/xcodebuild", "Xcode 26.3"), "xcrun": ("/usr/bin/xcrun", "xcrun version 72."),
             "zig": (zig_path, zig)}
    if rust:
        tools.update({"cargo": ("/opt/homebrew/bin/cargo", "cargo 1.88.0 (abc 2025-06-23)"),
                      "rustc": ("/opt/homebrew/bin/rustc", "rustc 1.88.0 (abc 2025-06-23)"),
                      "rustup": ("/opt/homebrew/bin/rustup", "rustup 1.29.1 (2026-08-13)")})
    for tool in mf.bootstrap.MACOS_WORKLOAD_TOOLS:
        path, version = tools.get(tool, ("", ""))
        lines.append(f"pf_tool\t{tool}|{path}|{version if path else ''}")
    if rust:
        lines.append("pf_rust_channel\t1.88.0|rustc 1.88.0 (abc 2025-06-23)|cargo 1.88.0 (abc 2025-06-23)")
    lines.append(f"pf_python\t{python}")
    if python3 is not None:
        lines.append(f"pf_python3\t{python3}")
    if brew_python3:
        lines.append("pf_brew_python3\t/opt/homebrew/bin/python3.13")
    if gh is not None:
        lines.append(f"pf_gh\t{gh}")
    lines += ["pf_cmux\tpresent", "pf_cmux_pin\t26", "pf_zig_min\t0.16.0" if submodules[0][0] == " " else "pf_zig_min\t"]
    lines += [f"pf_submodule\t{s}" for s in submodules]
    lines += [f"pf_setup_artifacts\t{'yes' if artifacts else 'no'}", f"pf_cmux_dirty\t{dirty}"]
    if candidate:
        lines.append(f"pf_candidate\t{candidate}")
    if candidate_generation:
        lines.append(f"pf_candidate_generation\t{candidate_generation}")
    lines += ["pf_glaeda\tpresent", "pf_cache_root\tpresent", "pf_glaeda_head\t" + "c" * 40]
    if glaeda_lacking is not None:
        lines += [f"pf_glaeda_lacking\t{glaeda_lacking}", f"pf_glaeda_dirty\t{glaeda_dirty}"]
    if enroll_state:
        lines.append(f"pf_enroll_state\t{enroll_state}")
    if enroll_generation:
        lines.append(f"pf_enroll_generation\t{enroll_generation}")
    if enroll_reason:
        lines.append(f"pf_enroll_reason\t{enroll_reason}")
    if acceptance:
        lines.append(f"pf_acceptance\t{acceptance}")
    if brew_owner:
        lines.append(f"pf_brew_owner\t{brew_owner}")
        lines += [f"pf_brew_formula\t{pin}" for pin in pins]
    elif brew_dir:
        lines.append(f"pf_brew_dir\t{brew_dir}")
    if update_running:
        lines.append(f"pf_update_running\t{update_running}")
    if prepared:
        lines.append(f"pf_update_prepared\t{prepared}")
    lines += ["pf_pmset\tAC Power:", f"pf_pmset\t sleep                {sleep}", "pf_autoupdate\t0|0"]
    lines += [f"pf_runner\t{r}" for r in runners]
    lines += [f"pf_secret\t{p}" for p in secrets]
    return "\n".join(lines) + "\n"


class PreflightTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def result(self, text: str, host: str = "build-mini-1", zig_fallback: str | None = None) -> dict:
        obs = observed(**{host: text})["hosts"][host]
        return mf.preflight_host(self.manifest, host, obs, zig_fallback)

    def states(self, text: str, **kwargs: object) -> dict[str, str]:
        return {k: v["state"] for k, v in self.result(text, **kwargs)["checks"].items()}

    def test_ready_host_passes_every_check(self) -> None:
        result = self.result(preflight_text())
        self.assertTrue(result["ready"], result)
        self.assertEqual({k for k, v in result["checks"].items() if v["state"] not in {"ok", "info"}}, set())
        self.assertEqual(list(result["checks"]), list(mf.PREFLIGHT_CHECKS))

    def test_the_austin_mini_morning_is_found_in_one_pass(self) -> None:
        # 2026-09-24, cmux-austin-mini-1: every one of these showed up only after the previous fix.
        text = preflight_text(user="cmux", brew_owner="admin", zig="0.15.2", zig_path="/usr/local/bin/zig",
                              rust=False, metal=False, python="|3.9", submodules=("-a1 ghostty",),
                              artifacts=False, candidate="59ca9c9bd1bb|no|no", enroll_state=None,
                              acceptance=None, node_id=None)
        text = text.replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                            "xcode_select\t/Applications/Xcode_26.3.app/Contents/Developer")
        result = self.result(text, zig_fallback="0.16.0")
        checks = result["checks"]
        self.assertFalse(result["ready"])
        self.assertEqual({k for k, v in checks.items() if v["state"] == "fail"},
                         {"select", "metal", "rust", "zig", "python", "cmux", "setup"})
        self.assertEqual({k for k, v in checks.items() if v["state"] == "todo"}, {"candidate", "enroll"})
        fixes = mf.grouped_fixes(result)
        self.assertIn("select: sudo xcode-select -s /Applications/Xcode.app", fixes["password"])
        self.assertTrue(any(f.startswith("zig: sudo -u admin brew install zig") for f in fixes["password"]))
        self.assertTrue(any(f.startswith("rust: sudo -u admin brew install rustup") for f in fixes["password"]))
        self.assertTrue(any("-downloadComponent MetalToolchain" in f for f in fixes["self"]))
        self.assertTrue(any("git submodule update --init" in f for f in fixes["self"]))
        self.assertTrue(any("~/.local/bin/python3" in f for f in fixes["self"]))
        self.assertIn("0.15.2 at /usr/local/bin/zig, Ghostty needs 0.16.0", checks["zig"]["detail"])
        self.assertIn("admin", checks["brew"]["detail"])
        # Blockers come before the steps onboarding performs itself.
        self.assertTrue(fixes["self"][-1].startswith("enroll: glaeda-mini-enroll"))
        self.assertIn("--node-id cmux-mac-001", fixes["self"][-1])

    def test_first_launch_is_caught_from_plugin_errors_too(self) -> None:
        checks = self.result(preflight_text(sdks="plugin_error|DVTPlugInLoading: symbol not found"))["checks"]
        self.assertEqual(checks["launch"]["state"], "fail")
        self.assertIn("-runFirstLaunch", checks["launch"]["fix"])
        self.assertEqual(checks["launch"]["group"], "password")
        text = preflight_text().replace("Xcode.app|accepted|done", "Xcode.app|needed|needed")
        checks = self.result(text)["checks"]
        self.assertEqual((checks["licence"]["state"], checks["launch"]["state"]), ("fail", "fail"))

    def test_pending_macos_update_blocks(self) -> None:
        running = self.result(preflight_text(update_running="softwareupdate --install macOS 26.7 --restart"))
        self.assertFalse(running["ready"])
        self.assertEqual(running["checks"]["update"]["group"], "person")
        prepared = self.result(preflight_text(prepared="pending|26.7"))
        self.assertEqual(prepared["checks"]["update"]["state"], "fail")
        self.assertIn("macOS 26.7 is prepared and waits for a restart", prepared["checks"]["update"]["detail"])
        # cmux14 and cmux15 carry a 26.6.2 prepared before a reboot that never applied; that is not pending.
        suspended = self.result(preflight_text(prepared="suspended|26.6.2"))
        self.assertEqual(suspended["checks"]["update"]["state"], "ok")

    def test_onboarding_steps_left_do_not_block(self) -> None:
        text = preflight_text(candidate="59ca9c9bd1bb|no|yes", enroll_state=None, acceptance=None, runners=(),
                              node_id=None)
        result = self.result(text)
        self.assertTrue(result["ready"], result)
        self.assertEqual({k for k, v in result["checks"].items() if v["state"] == "todo"},
                         {"candidate", "enroll", "runner"})
        self.assertIn("archive downloaded, not staged", result["checks"]["candidate"]["detail"])

    def test_readable_secrets_fail_where_pr_jobs_run(self) -> None:
        paths = ("/Users/Shared/cmux-build-fleet/secrets/r2.token", "/Users/builder/.ssh/id_ed25519")
        # build-mini-1: a registered runner and the ci-runner role.
        checks = self.result(preflight_text(secrets=paths))["checks"]
        self.assertEqual((checks["secrets"]["state"], checks["secrets"]["group"]), ("fail", "person"))
        self.assertIn("readable by builder, the user PR jobs run as: " + ", ".join(paths), checks["secrets"]["detail"])
        self.assertIn("cmuxterm-hq#595", checks["secrets"]["fix"])
        self.assertFalse(self.result(preflight_text(secrets=paths))["ready"])
        # A registered runner is enough, whatever the roles say (build-mini-2 has no ci-runner role).
        text = preflight_text(secrets=paths[:1], hostname="build-mini-2", node_id="cmux-mac-002")
        self.assertEqual(self.result(text, host="build-mini-2")["checks"]["secrets"]["state"], "fail")

    def test_readable_secrets_on_a_builder_are_info(self) -> None:
        # A dev-build worker host (cmux15 then): no runner, no ci-runner role; its worker reads its own
        # tokens as the login user.
        text = preflight_text(secrets=("/Users/Shared/cmux-build-fleet/secrets/r2.token",), runners=(),
                              hostname="build-mini-2", node_id="cmux-mac-002")
        result = self.result(text, host="build-mini-2")
        self.assertEqual(result["checks"]["secrets"]["state"], "info")
        self.assertIn("r2.token", result["checks"]["secrets"]["detail"])
        self.assertTrue(result["ready"], result)

    def test_accepted_secrets_are_info_and_only_the_rest_fail(self) -> None:
        key, token = "/Users/builder/.ssh/id_ed25519", "/Users/Shared/cmux-build-fleet/secrets/r2.token"
        self.manifest["hosts"]["build-mini-1"]["accepted_secrets"] = ["~/.ssh/id_ed25519"]
        text = preflight_text(secrets=(key,)) + "pf_home\t/Users/builder\n"
        result = self.result(text)
        self.assertEqual(result["checks"]["secrets"]["state"], "info")
        self.assertEqual(result["checks"]["secrets"]["detail"], f"accepted: {key}")
        self.assertTrue(result["ready"], result)
        # Both: fail, naming only the path nobody accepted.
        checks = self.result(preflight_text(secrets=(key, token)) + "pf_home\t/Users/builder\n")["checks"]
        self.assertEqual((checks["secrets"]["state"], checks["secrets"]["group"]), ("fail", "person"))
        self.assertTrue(checks["secrets"]["detail"].endswith(f"PR jobs run as: {token}"), checks["secrets"]["detail"])
        self.assertNotIn(key, checks["secrets"]["detail"])
        # An absolute path matches as written; without pf_home, ~ is the login user's /Users home.
        self.manifest["hosts"]["build-mini-1"]["accepted_secrets"] = [token, "~/.ssh/id_ed25519"]
        self.assertEqual(self.result(preflight_text(secrets=(key, token)))["checks"]["secrets"]["state"], "info")

    def test_accepted_secrets_are_validated(self) -> None:
        for bad in ("~/.ssh/id_ed25519", ["relative/key"], ["~root/.ssh/id_rsa"], [3], ["/a\nb"]):
            data = json.loads(EXAMPLE.read_text())
            data["hosts"]["build-mini-1"]["accepted_secrets"] = bad
            with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(mf.Failure, "accepted_secrets"):
                mf.load_manifest(write_manifest(tmp, data))
        data = json.loads(EXAMPLE.read_text())
        data["hosts"]["build-mini-1"]["accepted_secrets"] = ["~/.ssh/id_ed25519", "/Users/Shared/x.token"]
        with tempfile.TemporaryDirectory() as tmp:
            mf.load_manifest(write_manifest(tmp, data))

    def test_no_readable_secrets_is_ok(self) -> None:
        checks = self.result(preflight_text())["checks"]
        self.assertEqual(checks["secrets"], {"state": "ok", "detail": "none readable by builder", "fix": "", "group": ""})
        self.assertEqual(list(checks)[-1], "secrets")

    def test_node_id_is_required_before_enrollment(self) -> None:
        checks = self.result(preflight_text(enroll_state=None, acceptance=None, node_id=None), host="small-mini")["checks"]
        self.assertEqual(checks["node"]["state"], "fail")
        self.assertIn("hosts.small-mini.node_id", checks["node"]["fix"])
        checks = self.result(preflight_text(node_id="cmux-mac-009"))["checks"]
        self.assertIn("enrolled as cmux-mac-009", checks["node"]["detail"])

    def test_automatic_updates_are_a_nonblocking_password_todo(self) -> None:
        on = self.result(preflight_text().replace("pf_autoupdate\t0|0", "pf_autoupdate\t1|"))["checks"]["autoupdate"]
        self.assertEqual((on["state"], on["group"]), ("todo", "password"))
        self.assertIn("install and download", on["detail"])  # unset download means on
        self.assertEqual(self.states(preflight_text())["autoupdate"], "ok")
        older = preflight_text().replace("pf_autoupdate\t0|0\n", "")
        self.assertNotIn("autoupdate", self.states(older))  # an older probe says nothing

    def test_power_disk_and_dirty_checkout(self) -> None:
        states = self.states(preflight_text(sleep=10, dirty=3))
        self.assertEqual((states["power"], states["cmux"]), ("fail", "fail"))
        low = preflight_text().replace("disk\t/System/Volumes/Data|460|220", "disk\t/System/Volumes/Data|460|20")
        self.assertEqual(self.states(low)["disk"], "fail")

    def test_bootstrap_refusals_are_predicted(self) -> None:
        # Each of these passes every tool check yet the fleet bootstrap would refuse the host.
        pin16 = preflight_text().replace("pf_cmux_pin\t26", "pf_cmux_pin\t16")
        self.assertEqual(self.states(pin16)["xcode"], "fail")
        small = preflight_text().replace("cpus\t14", "cpus\t4")
        self.assertEqual(self.states(small)["os"], "fail")
        bare = preflight_text().replace("pf_cache_root\tpresent", "pf_cache_root\tmissing")
        self.assertIn("native cache root", self.result(bare)["checks"]["glaeda"]["detail"])
        no_cargo = preflight_text().replace("|cargo 1.88.0 (abc 2025-06-23)", "|error: toolchain '1.88.0' is not installed")
        self.assertEqual(self.states(no_cargo)["rust"], "fail")
        dubious = preflight_text(submodules=("fatal: detected dubious ownership in repository",))
        checks = self.result(dubious)["checks"]
        self.assertIn("dubious ownership", checks["cmux"]["detail"])
        self.assertEqual(checks["cmux"]["group"], "person")

    def test_unknowns_that_hide_a_refusal_block(self) -> None:
        del self.manifest["defaults"]["xcode"]
        del self.manifest["defaults"]["toolchain"]["xcode"]
        result = self.result(preflight_text())
        self.assertEqual(result["checks"]["xcode"]["state"], "unknown")
        self.assertFalse(result["ready"])

    def test_metal_is_not_blamed_for_an_unaccepted_licence(self) -> None:
        text = preflight_text(metal=False).replace("Xcode.app|accepted|done", "Xcode.app|needed|done")
        checks = self.result(text)["checks"]
        self.assertEqual((checks["licence"]["state"], checks["metal"]["state"]), ("fail", "unknown"))

    def test_zig_minimum_falls_back_to_the_operator_checkout(self) -> None:
        del self.manifest["defaults"]["toolchain"]["zig_min"]
        text = preflight_text(zig="0.15.2", submodules=("-a1 ghostty",))
        self.assertEqual(self.states(text)["zig"], "unknown")
        self.assertEqual(self.states(text, zig_fallback="0.16.0")["zig"], "fail")
        self.assertEqual(self.states(preflight_text(zig="0.16.1"))["zig"], "ok")

    def test_missing_homebrew_is_installed_by_sudo_plan_then_used_by_fix(self) -> None:
        # Most Manaflow minis had no Homebrew at all on 2026-09-24.
        result = self.result(preflight_text(user="cmux", brew_owner=None, rust=False, zig="0.15.2"), zig_fallback="0.16.0")
        checks = result["checks"]
        self.assertEqual((checks["rust"]["group"], checks["brew"]["state"], checks["brew"]["group"]),
                         ("self", "fail", "password"))
        self.assertIn("once fix puts Homebrew in /opt/homebrew for cmux", checks["rust"]["fix"])
        mine, root = mf.planned_actions(result)
        self.assertEqual([a["kind"] for a in root], ["homebrew"])
        self.assertEqual([(a["kind"], a.get("formulas") or a.get("toolchain")) for a in mine],
                         [("brew", ["rustup", "zig"]), ("rust_default", "stable")])
        self.assertEqual(mine[0]["owner"], "cmux")
        out = mf.fix_host(self.manifest, "build-mini-1", result, False, None)
        self.assertEqual(len(out["waiting"]), 2)
        self.assertIn("waits for brew (sudo-plan)", out["waiting"][0])
        self.assertIn("waits for rust", out["waiting"][1])
        script = mf.sudo_plan_script(self.manifest, "build-mini-1", result)
        # Root only creates the directory; the brew.sh installer stalled on a Command Line Tools install.
        self.assertIn("  [ -d /opt/homebrew ] || sudo mkdir /opt/homebrew\n", script)
        self.assertIn("  sudo chown -R cmux:admin /opt/homebrew\n", script)
        self.assertIn('"$(sudo find /opt/homebrew -mindepth 1 -not -type d | head -1)"', script)  # root looks
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        for word in ("brew install", "install.sh", "curl"):
            self.assertNotIn(word, script)
        # Once Homebrew exists and the login user owns it, fix installs the formulas without root.
        checks["brew"] = {"state": "info", "detail": "cmux", "fix": "", "group": ""}
        out = mf.fix_host(self.manifest, "build-mini-1", result, False, None)
        self.assertEqual([p.split(":")[0] for p in out["planned"]], ["rust/zig", "rust"])

    def test_an_empty_homebrew_prefix_is_filled_by_fix_without_root(self) -> None:
        # The brew.sh installer stalled on the Command Line Tools and left /opt/homebrew empty, owned by cmux.
        text = preflight_text(user="cmux", brew_owner=None, brew_dir="cmux|empty", rust=False, zig="0.15.2")
        result = self.result(text, zig_fallback="0.16.0")
        brew = result["checks"]["brew"]
        self.assertEqual((brew["state"], brew["group"], brew["action"]), ("fail", "self", {"kind": "homebrew_fetch"}))
        mine, root = mf.planned_actions(result)
        self.assertEqual(root, [])
        self.assertEqual([a["kind"] for a in mine], ["homebrew_fetch", "brew", "rust_default"])
        out = mf.fix_host(self.manifest, "build-mini-1", result, False, None, seed="build-mini-2")
        self.assertEqual(out["waiting"], [])
        self.assertTrue(out["planned"][0].startswith("brew: git fetch Homebrew/brew into /opt/homebrew"), out["planned"])
        self.assertIn("[from build-mini-2 over the LAN]", out["planned"][0])
        # Someone else's empty prefix is handed over by sudo-plan; one with files needs a person.
        self.assertEqual(result["enrollment"]["state"], "eligible")  # the brew check leaves the enrollment alone
        self.assertEqual(mf.describe({"kind": "glaeda_sync"}),
                         "move ~/glaeda to the tip of Glaeda main (clean fetch, detached checkout)")
        other = self.result(preflight_text(user="cmux", brew_owner=None, brew_dir="root|empty", rust=False))
        self.assertEqual((other["checks"]["brew"]["group"], other["checks"]["brew"]["action"]["kind"]), ("password", "homebrew"))
        # An interrupted fetch (a .git, no brew) resumes in fix instead of going to a person.
        partial = self.result(preflight_text(user="cmux", brew_owner=None, brew_dir="cmux|partial", rust=False))
        self.assertEqual(partial["checks"]["brew"]["action"], {"kind": "homebrew_fetch"})
        files = self.result(preflight_text(user="cmux", brew_owner=None, brew_dir="cmux|files", rust=False))
        self.assertEqual(files["checks"]["brew"]["group"], "person")
        self.assertIn("move /opt/homebrew aside", files["checks"]["brew"]["fix"])

    def test_workload_python3_must_be_313(self) -> None:
        # Brew's python@3.13 installs only python3.13, so /usr/bin/python3 (3.9) is what the workload PATH finds.
        result = self.result(preflight_text(python3="/usr/bin/python3|3.9"))
        check = result["checks"]["python3"]
        self.assertFalse(result["ready"])
        self.assertIn("python3 on the workload PATH is 3.9 at /usr/bin/python3", check["detail"])
        self.assertEqual((check["group"], check["action"]), ("self", {"kind": "python_link", "owner": "builder"}))
        mine, _ = mf.planned_actions(result)
        self.assertEqual([a["kind"] for a in mine], ["python_link"])
        self.assertEqual(mf.fix_host(self.manifest, "build-mini-1", result, False, None)["planned"],
                         ["python3: link /opt/homebrew/bin/python3 to python3.13"])
        # No python3.13 yet: brew installs python@3.13 and links it, as the Homebrew owner when that is not us.
        bare = self.result(preflight_text(python3="/usr/bin/python3|3.9", brew_python3=False, brew_owner="admin"))
        _, root = mf.planned_actions(bare)
        self.assertEqual([(a["kind"], a.get("formulas")) for a in root], [("brew", ["python@3.13"])])
        script = mf.sudo_plan_script(self.manifest, "build-mini-1", bare)
        self.assertIn('sudo -H -u admin ln -s python3.13 "$b/python3"', script)
        self.assertIn("brew_as pin python@3.13", script)
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        linked = self.result(preflight_text(python3="/usr/bin/python3|3.9", brew_owner="admin"))
        self.assertIn("sudo -H -u admin ln -s python3.13", mf.sudo_plan_script(self.manifest, "build-mini-1", linked))
        # A python3 in /opt/homebrew/bin that is not 3.13 was put there by someone; a person decides.
        foreign = self.result(preflight_text(python3="/opt/homebrew/bin/python3|3.12"))
        self.assertEqual(foreign["checks"]["python3"]["group"], "person")

    def test_unpinned_formulas_warn_and_fix_pins_what_it_installs(self) -> None:
        result = self.result(preflight_text(pins=("rustup|pinned", "zig|unpinned", "python@3.13|unpinned")))
        pins = result["checks"]["pins"]
        self.assertTrue(result["ready"])
        self.assertEqual(pins["state"], "todo")
        self.assertEqual(pins["fix"], "brew pin zig python@3.13")
        lines = mf.brew_commands("builder", ["zig", "python@3.13"], False)
        self.assertEqual(lines[-1], "brew_as pin zig python@3.13")
        self.assertIn("brew unpin $f to upgrade it", "\n".join(lines))
        subprocess.run(["bash", "-n"], input="\n".join(lines), text=True, check=True)

    def test_missing_gh_is_brew_installed_and_left_unpinned(self) -> None:
        # cmux CI jobs call gh with GH_TOKEN and failed with FileNotFoundError on minis without it.
        result = self.result(preflight_text(gh=""))
        check = result["checks"]["gh"]
        self.assertFalse(result["ready"])
        self.assertEqual((check["state"], check["group"], check["fix"]), ("fail", "self", "brew install gh"))
        self.assertEqual(check["action"], {"kind": "brew", "formula": "gh", "owner": "builder"})
        self.assertEqual(result["checks"]["pins"]["state"], "ok")  # no class receipt names gh
        mine, root = mf.planned_actions(result)
        self.assertEqual(([(a["kind"], a["formulas"]) for a in mine], root), ([("brew", ["gh"])], []))
        self.assertEqual(mf.fix_host(self.manifest, "build-mini-1", result, False, None)["planned"],
                         ["gh: brew install gh"])
        lines = mf.brew_commands("builder", ["gh"], False)
        self.assertIn('[ -e "/opt/homebrew/bin/gh" ] || brew_as link gh', lines)
        self.assertFalse([line for line in lines if "brew_as pin" in line], lines)
        subprocess.run(["bash", "-n"], input="\n".join(lines), text=True, check=True)
        # Merged with a pinned formula, only that one is pinned; no gh auth step anywhere (jobs use GH_TOKEN).
        both = mf.brew_commands("builder", ["zig", "gh"], False)
        self.assertEqual(both[-1], "brew_as pin zig")
        self.assertFalse([line for line in both if "auth" in line], both)
        self.assertEqual(mf.describe({"kind": "brew", "formulas": ["zig", "gh"]}),
                         "brew install zig gh, then brew pin zig")
        # Someone else's Homebrew: sudo-plan installs it as that owner, still unpinned.
        other = self.result(preflight_text(gh="", brew_owner="admin"))
        self.assertEqual(other["checks"]["gh"]["fix"], "sudo -u admin brew install gh")
        script = mf.sudo_plan_script(self.manifest, "build-mini-1", other)
        self.assertIn("step 'Homebrew as admin: gh'", script)
        self.assertIn('[ -e "/opt/homebrew/bin/gh" ] || brew_as link gh', script)
        self.assertNotIn("brew_as pin", script)
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        # No Homebrew yet: gh makes it a blocker, like any formula.
        bare = self.result(preflight_text(user="cmux", brew_owner=None, gh=""))
        self.assertEqual((bare["checks"]["brew"]["state"], bare["checks"]["gh"]["group"]), ("fail", "self"))
        # A gh elsewhere on the workload PATH counts; an older probe is unknown and does not block.
        self.assertEqual(self.result(preflight_text(gh="/usr/local/bin/gh"))["checks"]["gh"]["state"], "ok")
        old = self.result(preflight_text(gh=None))
        self.assertEqual((old["checks"]["gh"]["state"], old["ready"]), ("unknown", True))

    def test_an_old_glaeda_checkout_is_moved_before_enrolling(self) -> None:
        # An old ~/glaeda failed onboard with "unrecognized arguments: --class-receipt ...".
        lacking = "--class-receipt --class-receipt-sha256 --fleet-class"
        result = self.result(preflight_text(glaeda_lacking=lacking))
        check = result["checks"]["glaeda"]
        self.assertFalse(result["ready"])
        self.assertEqual((check["state"], check["group"], check["action"]), ("fail", "self", {"kind": "glaeda_sync"}))
        self.assertIn(f"lacks {lacking}", check["detail"])
        dirty = self.result(preflight_text(glaeda_lacking=lacking, glaeda_dirty=2))["checks"]["glaeda"]
        self.assertEqual((dirty["group"], "action" in dirty), ("person", False))
        # --glaeda-ref pins the commit: any other head is moved, even one that has the options.
        with mock.patch.object(mf, "GLAEDA_REF", "e" * 40):
            pinned = self.result(preflight_text())["checks"]["glaeda"]
            self.assertEqual((pinned["state"], pinned["action"]["kind"]), ("fail", "glaeda_sync"))
            self.assertIn("the operator pins eeeeeeeeeeee", pinned["detail"])
        calls = []
        with mock.patch.object(mf, "GLAEDA_MAIN", {}), mock.patch.object(mf, "resolve_glaeda_main", return_value="d" * 40), \
                mock.patch.object(mf, "fix_call", side_effect=lambda n, u, f, a, log, t: (calls.append((f, a)), 0)[1]), \
                mock.patch.object(mf, "operator_candidate", return_value={}):
            for _ in range(2):  # main's tip is read once per run
                self.assertEqual(mf.run_repair(self.manifest, "build-mini-1", {"kind": "glaeda_sync"}, None),
                                 (True, "~/glaeda at dddddddddddd"))
            self.assertEqual(mf.resolve_glaeda_main.call_count, 1)
        self.assertEqual(calls, [("glaeda_sync", ["d" * 40, *mf.ENROLL_FLAGS])] * 2)

    def test_unreachable_and_unprobed_hosts_are_not_ready(self) -> None:
        result = mf.preflight_host(self.manifest, "build-mini-1", {"reachable": False, "error": "timeout"})
        self.assertEqual((result["ready"], result["checks"]["reach"]["detail"]), (False, "timeout"))
        plain = observed(**{"build-mini-1": probe_text()})["hosts"]["build-mini-1"]
        self.assertFalse(mf.preflight_host(self.manifest, "build-mini-1", plain)["ready"])

    def run_main(self, texts: dict[str, str], *extra: str) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as tmp:
            obs = Path(tmp) / "obs.json"
            obs.write_text(json.dumps(observed(**texts)))
            with contextlib.redirect_stdout(io.StringIO()) as out, \
                    mock.patch.object(mf, "operator_zig_minimum", return_value=None):
                code = mf.main(["preflight", *texts, "--manifest", os.fspath(EXAMPLE), "--observed", os.fspath(obs), *extra])
        return code, out.getvalue()

    def test_table_groups_fixes_and_exits_nonzero(self) -> None:
        code, out = self.run_main({"build-mini-1": preflight_text(),
                                   "build-mini-2": preflight_text(hostname="Build-Mini-2", metal=False, sleep=1,
                                                                  node_id="cmux-mac-002")})
        self.assertEqual(code, 1, out)
        header, first, second = out.splitlines()[:3]
        self.assertEqual(header.split()[1:], [*mf.PREFLIGHT_CHECKS, "ready"])
        self.assertTrue(first.startswith("build-mini-1") and first.endswith("yes"))
        self.assertTrue(second.endswith("NO"))
        self.assertIn("can do itself (no root):", out)
        self.assertIn("needs a password:\n    power: sudo pmset -c sleep 0", out)
        self.assertIn("2 hosts, 1 ready, 1 not ready", out)
        self.assertNotIn("\u2014", out)  # no em dashes in operator output

    def test_json_output_and_ready_exit(self) -> None:
        code, out = self.run_main({"build-mini-1": preflight_text()}, "--output", "json")
        self.assertEqual(code, 0, out)
        doc = json.loads(out)
        self.assertEqual(doc["schema"], "glaeda-mini-fleet/v1/preflight")
        host = doc["hosts"]["build-mini-1"]
        self.assertTrue(host["ready"])
        self.assertEqual(set(host["fixes"]), {"self", "password", "person"})

    def test_never_touch_host_is_refused_before_any_connection(self) -> None:
        with mock.patch.object(mf, "observe_host") as probe, contextlib.redirect_stderr(io.StringIO()) as err:
            code = mf.main(["preflight", "coordinator-mini", "--manifest", os.fspath(EXAMPLE)])
        self.assertEqual(code, 2)
        self.assertIn("never_touch", err.getvalue())
        probe.assert_not_called()

    def test_preflight_ships_the_owning_scripts_rules(self) -> None:
        script = mf.preflight_script(self.manifest, "build-mini-1").decode()
        self.assertTrue(script.startswith(mf.PROBE.read_text()))
        self.assertIn(f"WORKLOAD_PATH={mf.bootstrap.CMUX_WORKLOAD_TOOL_PATH}\n", script)
        self.assertIn("WORKLOAD_TOOLS='" + " ".join(mf.bootstrap.MACOS_WORKLOAD_TOOLS) + "'", script)
        self.assertIn("XCODE_PIN=/Applications/Xcode.app\n", script)
        self.assertIn(mf.enroll.PYTHON_CANDIDATES[0], script)
        self.assertIn("~/.local/bin/python3", script)
        self.assertEqual(mf.xcode_pin(self.manifest, "build-mini-1"), "/Applications/Xcode.app")


class PreflightScriptTests(unittest.TestCase):
    def test_parses_as_bash(self) -> None:
        subprocess.run(["bash", "-n", os.fspath(mf.PREFLIGHT_PROBE)], check=True)

    def test_never_installs_writes_or_escalates(self) -> None:
        body = [line.strip() for line in mf.PREFLIGHT_PROBE.read_text().splitlines()
                if line.strip() and not line.strip().startswith("#")]
        # The update check reports the softwareupdate command line, not the sudo wrapper around it.
        body = [line.replace("grep -v '^[0-9]* sudo '", "") for line in body if not line.startswith("applying='")]
        for word in ("sudo", "rm ", "mv ", "cp ", "brew ", "install", "downloadComponent", "curl", "> ", ">>"):
            self.assertFalse([line for line in body if word in line.replace("2>", "").replace(">/dev/null", "")],
                             word)
        self.assertIn("export RUSTUP_AUTO_INSTALL=0 GIT_OPTIONAL_LOCKS=0", body)

    @unittest.skipUnless(shutil.which("git"), "needs git")
    def test_runs_against_a_sandbox_home(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            tools = home / "workload-bin"
            tools.mkdir()
            for name, text in (("zig", "0.15.2"), ("cargo", "cargo 1.88.0 (x)"), ("gh", "gh version 2.80.0"),
                               ("git", None)):
                if text is None:
                    (tools / name).symlink_to(shutil.which("git"))
                    continue
                (tools / name).write_text(f"#!/bin/sh\necho '{text}'\n")
                (tools / name).chmod(0o755)
            cmux = home / "cmux"
            subprocess.run(["git", "init", "-q", os.fspath(cmux)], check=True)
            generation = home / "Projects/glaeda-generations" / ("ab" * 6)
            (generation / "bin").mkdir(parents=True)
            (generation / "stage-receipt.json").write_text("{}")
            (generation / "bin/glaeda").write_bytes(b"glaeda")
            fleet = home / ".config/glaeda/cmux-fleet"
            fleet.mkdir(parents=True)
            (fleet / "enrollment.json").write_text('{"state": "eligible", "glaedaGeneration": "sha256:old"}')
            (home / "glaeda/scripts").mkdir(parents=True)
            (home / "glaeda/scripts/glaeda-mini-enroll").write_text('p.add_argument("--renew")\n')
            (tools / "python3").write_text("#!/bin/sh\necho 3.9\n")
            (tools / "python3").chmod(0o755)
            runner = home / "actions-runner-cmux-persistent-compile"
            runner.mkdir()
            (runner / ".runner").write_text('﻿{\n  "agentName": "mini-1",\n  "serverUrl": "https://secret.example/"\n}\n')
            header = (f"CMUX_ROOT='~/cmux'\nXCODE_PIN=''\nWORKLOAD_PATH={tools}\n"
                      f"WORKLOAD_TOOLS='cargo git zig rustup'\nPYTHONS=''\nCANDIDATE_PIN={'ab' * 20}\n"
                      "ENROLL_FLAGS='--renew --class-receipt --fleet-class'\nPIN_FORMULAS='zig'\n")
            # The whole SSH payload, probe first, as observe_host sends it.
            script = mf.PROBE.read_text() + "\n" + header + mf.PREFLIGHT_PROBE.read_text()
            out = subprocess.run(["bash", "-s"], input=script, capture_output=True,
                                 text=True, env={"HOME": tmp, "PATH": "/usr/bin:/bin"}, timeout=60).stdout
            pf = mf.parse_probe(out)["preflight"]
        self.assertEqual(pf["tools"]["zig"], {"path": f"{tools}/zig", "version": "0.15.2"})
        self.assertEqual(pf["tools"]["cargo"]["version"], "cargo 1.88.0 (x)")
        self.assertIsNone(pf["tools"]["rustup"]["path"])
        self.assertTrue(pf["cmux"])
        import hashlib
        self.assertEqual(pf["candidate"], {"source12": "ab" * 6, "staged": True, "archive": False,
                                           "generation": "sha256:" + hashlib.sha256(b"glaeda").hexdigest()})
        # No launchd agent recorded: loaded is unknown, and nothing listens in the sandbox.
        self.assertEqual(pf["runners"], [{"dir": "actions-runner-cmux-persistent-compile", "name": "mini-1",
                                          "loaded": "unknown", "listening": "no", "held": "no"}])
        self.assertNotIn("secret.example", out)
        self.assertEqual(pf["python"], {"path": None, "version": None})
        self.assertEqual(pf["python3"], {"path": f"{tools}/python3", "version": "3.9"})
        self.assertEqual(pf["glaeda_lacking"], ["--class-receipt", "--fleet-class"])
        self.assertEqual(pf["gh"], f"{tools}/gh")


class PreflightSecretsScriptTests(unittest.TestCase):
    def test_lists_readable_secret_paths_never_contents(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            files = {".secrets/r2.env": "R2_SECRET=sekrit-env-value\n",
                     ".ssh/id_ed25519": "-----BEGIN sekrit-key-body\n",
                     ".ssh/id_ed25519.pub": "ssh-ed25519 AAAA public\n",
                     ".config/glaeda/fleet.key": "sekrit-glaeda-key\n",
                     ".config/glaeda/empty.key": "",
                     ".config/gh/hosts.yml": "github.com:\n    oauth_token: gho_sekritgh\n",
                     "Library/Application Support/cmux-build-fleet/secrets/runner.token": "sekrit-runner-token\n",
                     ".secrets/locked.env": "sekrit-locked\n",
                     ".secrets/nested/deep/.hidden.token": "sekrit-nested\n",
                     ".secrets/bad\nname": "sekrit-newline\n",
                     ".secrets/bad\tname": "sekrit-tab\n",
                     ".secrets/bad\rname": "sekrit-cr\n",
                     ".netrc": "machine x password sekrit-netrc\n",
                     ".git-credentials": "https://u:sekrit-gitcred@example.com\n",
                     ".docker/config.json": '{"auths": {"x": {"auth": "sekrit-docker"}}}\n',
                     ".aws/credentials": "aws_secret_access_key = sekrit-aws\n",
                     ".config/rclone/rclone.conf": "secret_access_key = sekrit-rclone\n"}
            for rel, text in files.items():
                (home / rel).parent.mkdir(parents=True, exist_ok=True)
                (home / rel).write_text(text)
            (home / ".secrets/locked.env").chmod(0)
            (home / ".secrets/dir").mkdir()
            header = ("CMUX_ROOT='~/cmux'\nXCODE_PIN=''\nWORKLOAD_PATH=/usr/bin\nWORKLOAD_TOOLS=''\nPYTHONS=''\n"
                      "CANDIDATE_PIN=''\nENROLL_FLAGS=''\nPIN_FORMULAS=''\n")
            script = mf.PROBE.read_text() + "\n" + header + mf.PREFLIGHT_PROBE.read_text()
            out = subprocess.run(["bash", "-s"], input=script, capture_output=True, text=True,
                                 env={"HOME": tmp, "PATH": "/usr/bin:/bin"}, timeout=60).stdout
            found = [p for p in mf.parse_probe(out)["preflight"]["secrets"] if p.startswith(tmp)]
            # A token-less gh config, or a docker config with no auth, is not a secret.
            (home / ".config/gh/hosts.yml").write_text("github.com:\n    user: someone\n")
            (home / ".docker/config.json").write_text('{"auths": {}, "credsStore": "osxkeychain"}\n')
            again = subprocess.run(["bash", "-s"], input=script, capture_output=True, text=True,
                                   env={"HOME": tmp, "PATH": "/usr/bin:/bin"}, timeout=60).stdout
            (home / ".secrets/locked.env").chmod(0o600)
        want = [f"{tmp}/.config/glaeda/fleet.key", f"{tmp}/.secrets/r2.env", f"{tmp}/.secrets/nested/deep/.hidden.token",
                f"{tmp}/Library/Application Support/cmux-build-fleet/secrets/runner.token",
                f"{tmp}/.config/gh/hosts.yml", f"{tmp}/.ssh/id_ed25519", f"{tmp}/.netrc", f"{tmp}/.git-credentials",
                f"{tmp}/.docker/config.json", f"{tmp}/.aws/credentials", f"{tmp}/.config/rclone/rclone.conf"]
        if os.geteuid() == 0:  # root reads a mode-0 file
            want.append(f"{tmp}/.secrets/locked.env")
        self.assertEqual(sorted(found), sorted(want))
        self.assertEqual(mf.parse_probe(out)["preflight"]["home"], tmp)
        # A newline, tab or carriage return in a name could forge or disguise probe lines, so those paths are skipped whole.
        self.assertNotIn("bad", out)
        later = mf.parse_probe(again)["preflight"]["secrets"]
        self.assertNotIn(f"{tmp}/.config/gh/hosts.yml", later)
        self.assertNotIn(f"{tmp}/.docker/config.json", later)
        self.assertNotIn("sekrit", out + again)


def morning_text() -> str:
    """cmux-austin-mini-1 on 2026-09-24 (see PreflightTests), as a preflight probe."""
    text = preflight_text(user="cmux", brew_owner="admin", zig="0.15.2", zig_path="/usr/local/bin/zig",
                          rust=False, metal=False, python="|3.9", submodules=("-a1 ghostty",),
                          artifacts=False, candidate="59ca9c9bd1bb|no|no", enroll_state=None,
                          acceptance=None, node_id=None)
    return text.replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                        "xcode_select\t/Applications/Xcode_26.3.app/Contents/Developer")


def write_manifest(tmp: str, data: dict) -> Path:
    path = Path(tmp) / "m.json"
    path.write_text(json.dumps(data))
    return path


class ToolchainTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def result(self, text: str, host: str = "build-mini-1") -> dict:
        return mf.preflight_host(self.manifest, host, observed(**{host: text})["hosts"][host])

    def test_bad_declarations_are_refused(self) -> None:
        cases = (({"python_min": "3.12"}, "below 3.13"),
                 ({"xcode": {"app": "/Applications/My Xcode.app", "version": "26.3", "build": "x"}}, "without spaces"),
                 ({"xcode": {"app": "/Applications/Xcode.app"}}, "needs app, version and build"),
                 ({"zig_min": "latest"}, "not a version"),
                 ({"rustup_default": "stable; rm -rf ~"}, "not a toolchain name"),
                 ({"cmux": {"ref": "-o ProxyCommand"}}, "branch or tag"),
                 ({"cmux": {"submodule_depth": -1}}, "submodule_depth"),
                 ({"ruby": "3"}, "unknown keys"))
        for override, error in cases:
            data = copy.deepcopy(self.manifest)
            data["hosts"]["build-mini-1"]["overrides"] = {"toolchain": override}
            with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(mf.Failure, error):
                mf.load_manifest(write_manifest(tmp, data))

    def test_declared_xcode_is_the_pin(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"]["app"] = "/Applications/Xcode_26.3.app"
        self.assertEqual(mf.xcode_pin(self.manifest, "build-mini-1"), "/Applications/Xcode_26.3.app")
        self.manifest["defaults"]["toolchain"]["xcode"]["build"] = "17C999"
        checks = self.result(preflight_text())["checks"]
        self.assertEqual(checks["xcode"]["state"], "fail")
        self.assertIn("want 26.3 (17C999)", checks["xcode"]["detail"])

    def test_selection_and_metal_can_be_declared_optional(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"]["select"] = False
        self.manifest["defaults"]["toolchain"]["metal"] = False
        text = preflight_text(metal=False).replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                                                   "xcode_select\t/Library/Developer/CommandLineTools")
        result = self.result(text)
        self.assertEqual((result["checks"]["select"]["state"], result["checks"]["metal"]["state"]), ("info", "info"))
        self.assertTrue(result["ready"], result)

    def test_declared_minimums_raise_the_bar(self) -> None:
        self.manifest["defaults"]["toolchain"]["zig_min"] = "0.17.0"
        self.manifest["defaults"]["toolchain"]["python_min"] = "3.14"
        checks = self.result(preflight_text(zig="0.16.0"))["checks"]
        self.assertIn("Ghostty needs 0.17.0", checks["zig"]["detail"])
        self.assertEqual(checks["python"]["state"], "fail")
        self.assertIn("enroll would use 3.13", checks["python"]["detail"])
        self.assertEqual(checks["python"]["action"], {"kind": "python", "min": "3.14"})

    def test_rustup_default_must_match(self) -> None:
        text = preflight_text() + "pf_rust_default\tnightly-aarch64-apple-darwin (default)\n"
        checks = self.result(text)["checks"]
        self.assertEqual(checks["rust"]["state"], "fail")
        self.assertEqual(checks["rust"]["action"], {"kind": "rust_default", "toolchain": "stable"})
        ok = preflight_text() + "pf_rust_default\tstable-aarch64-apple-darwin (default)\n"
        self.assertEqual(self.result(ok)["checks"]["rust"]["state"], "ok")
        self.manifest["defaults"]["toolchain"]["rustup_default"] = "1.8"
        near = preflight_text() + "pf_rust_default\t1.80.0-aarch64-apple-darwin (default)\n"
        self.assertEqual(self.result(near)["checks"]["rust"]["state"], "fail")

    def test_cmux_checkout_comes_from_the_toolchain(self) -> None:
        self.manifest["defaults"]["toolchain"]["cmux"] = {"repo": "example/cmux", "ref": "release", "root": "~/src/cmux",
                                                         "submodule_depth": 0}
        text = preflight_text().replace("pf_cmux\tpresent", "pf_cmux\tmissing")
        check = self.result(text)["checks"]["cmux"]
        self.assertEqual(check["action"], {"kind": "cmux_clone", "repo": "example/cmux", "ref": "release",
                                           "root": "~/src/cmux", "depth": 0})
        self.assertIn("--branch release https://github.com/example/cmux.git ~/src/cmux", check["fix"])
        self.assertIn("CMUX_ROOT='~/src/cmux'", mf.preflight_script(self.manifest, "build-mini-1").decode())

    def test_check_reports_toolchain_drift(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"] = {"app": "/Applications/Xcode_26.6.app", "version": "26.6",
                                                           "build": "17F113"}
        obs = observed(**{"build-mini-1": preflight_text(zig="0.15.2", metal=False)})
        issues = [i for i in mf.check(self.manifest, obs, ["build-mini-1"]) if i["area"] == "toolchain"]
        details = [i["detail"] for i in issues]
        self.assertIn("Xcode /Applications/Xcode_26.6.app (26.6 17F113) missing", details)
        self.assertTrue(any(d.startswith("zig: 0.15.2") for d in details), details)
        # Without a preflight section only the Xcode part can be judged.
        plain = observed(**{"build-mini-1": probe_text()})
        self.assertEqual([i["detail"] for i in mf.check(self.manifest, plain, ["build-mini-1"]) if i["area"] == "toolchain"],
                         ["Xcode /Applications/Xcode_26.6.app (26.6 17F113) missing"])

    def test_check_probes_preflight_when_a_toolchain_is_declared(self) -> None:
        with mock.patch.object(mf, "observe", return_value=observed(**{"build-mini-1": preflight_text()})) as probe, \
                contextlib.redirect_stdout(io.StringIO()):
            mf.main(["check", "build-mini-1", "--manifest", os.fspath(EXAMPLE)])
        self.assertTrue(probe.call_args.kwargs["preflight"])


class FixPlanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        obs = observed(**{"build-mini-1": morning_text()})["hosts"]["build-mini-1"]
        self.result = mf.preflight_host(self.manifest, "build-mini-1", obs, "0.16.0")

    def test_the_morning_splits_into_fix_and_sudo_plan(self) -> None:
        mine, root = mf.planned_actions(self.result)
        self.assertEqual([a["kind"] for a in mine], ["python", "submodules", "metal", "setup", "candidate"])
        self.assertEqual([a["kind"] for a in root], ["select", "brew"])
        self.assertEqual((root[1]["formulas"], root[1]["checks"], root[1]["owner"]), (["rustup", "zig"], ["rust", "zig"], "admin"))

    def test_dry_run_plans_waits_and_runs_nothing(self) -> None:
        with mock.patch.object(mf, "run_repair") as run:
            out = mf.fix_host(self.manifest, "build-mini-1", self.result, False, None)
        run.assert_not_called()
        self.assertEqual(len(out["planned"]), 4)
        self.assertEqual(out["waiting"], ["setup: cmux scripts/setup.sh in ~/cmux (builds GhosttyKit) (waits for zig (sudo-plan))"])
        self.assertEqual(out["sudo"], ["select", "rust", "zig"])
        text = mf.render_fix([out], False)
        self.assertIn("dry run; pass --yes", text)
        self.assertNotIn("\u2014", text)

    def test_runs_in_order_logs_and_a_failure_holds_back_its_dependents(self) -> None:
        self.result["checks"]["zig"] = {"state": "ok", "detail": "", "fix": "", "group": ""}
        calls = []

        def repair(manifest, name, action, log, seed=None):
            calls.append(action["kind"])
            return action["kind"] != "metal", "boom" if action["kind"] == "metal" else ""

        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(mf, "run_repair", side_effect=repair), \
                contextlib.redirect_stdout(io.StringIO()):
            out = mf.fix_host(self.manifest, "build-mini-1", self.result, True, Path(tmp))
            log = Path(out["log"]).read_text()
        self.assertEqual(calls, ["python", "submodules", "metal", "candidate"])
        self.assertEqual(len(out["failed"]), 1)
        self.assertIn("waits for metal (fix)", out["waiting"][0])
        self.assertIn("FAILED boom", log)

    def test_one_host_raising_is_a_failure_not_a_crash(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "run_repair", side_effect=OSError("disk full")):
            out = mf.fix_host(self.manifest, "build-mini-1", self.result, True, Path(tmp))
        self.assertTrue(out["failed"])
        self.assertIn("OSError: disk full", out["failed"][0])

    def test_unknown_prerequisites_name_their_cause(self) -> None:
        self.result["checks"]["licence"] = {"state": "unknown", "detail": "no runnable xcodebuild in the pinned Xcode",
                                            "fix": "", "group": ""}
        waiting = mf.fix_host(self.manifest, "build-mini-1", self.result, False, None)["waiting"]
        self.assertIn("waits for licence (unknown: no runnable xcodebuild in the pinned Xcode)", waiting[0])

    def test_glaeda_runs_with_the_python_enroll_will_use_and_waits_for_cargo(self) -> None:
        self.result["checks"]["glaeda"] = {"state": "fail", "detail": "", "fix": "", "group": "self", "action": {"kind": "glaeda"}}
        waiting = mf.fix_host(self.manifest, "build-mini-1", self.result, False, None)["waiting"]
        self.assertTrue(any(w.startswith("glaeda:") and "waits for rust (sudo-plan)" in w for w in waiting), waiting)
        self.result["checks"]["glaeda"] = {"state": "fail", "detail": "", "fix": "", "group": "self", "action": {"kind": "glaeda"}}
        self.result["checks"]["rust"] = {"state": "ok", "detail": "", "fix": "", "group": ""}
        seen = {}
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "run_repair", side_effect=lambda m, n, a, log, seed=None: (seen.setdefault(a["kind"], a), (True, ""))[1]):
            mf.fix_host(self.manifest, "build-mini-1", self.result, True, Path(tmp))
        self.assertEqual(seen["glaeda"]["python"], "~/.local/bin/python3")

    def test_fix_command_skips_record_only_hosts_and_probes_once(self) -> None:
        obs = observed(**{"build-mini-1": morning_text()})
        with mock.patch.object(mf, "observe", return_value=obs) as probe, mock.patch.object(mf, "run_repair") as run, \
                contextlib.redirect_stdout(io.StringIO()) as out, mock.patch.object(mf, "host_check", return_value=None), \
                mock.patch.object(mf, "operator_zig_minimum", return_value="0.16.0"):
            code = mf.main(["fix", "build-mini-1", "small-mini", "--manifest", os.fspath(EXAMPLE)])
        self.assertEqual(code, 0)
        run.assert_not_called()
        self.assertEqual(probe.call_args.args[1], ["build-mini-1"])
        self.assertIn("small-mini: skipped (apply: false)", out.getvalue())
        self.assertIn("would run", out.getvalue())


class SudoPlanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def plan(self, text: str, sudoers: bool = False) -> str | None:
        obs = observed(**{"build-mini-1": text})["hosts"]["build-mini-1"]
        return mf.sudo_plan_script(self.manifest, "build-mini-1", mf.preflight_host(self.manifest, "build-mini-1", obs, "0.16.0"),
                                   sudoers)

    def test_every_root_step_once_in_order(self) -> None:
        text = morning_text().replace("Xcode.app|accepted|done", "Xcode.app|needed|needed").replace(
            "pf_pmset\t sleep                0", "pf_pmset\t sleep                1\npf_pmset\t autorestart          0")
        script = self.plan(text)
        order = [script.index(s) for s in ("sudo -v", "sudo xcode-select -s /Applications/Xcode.app",
                                           "xcodebuild -license accept", "xcodebuild -runFirstLaunch",
                                           "sudo pmset -c sleep 0", "sudo pmset -a autorestart 1",
                                           "sudo -H -u admin env HOMEBREW_NO_ASK=1 /opt/homebrew/bin/brew", "for f in rustup zig",
                                           'sudo -H -u admin ln -s "/opt/homebrew/opt/rustup/bin/$t"')]
        self.assertEqual(order, sorted(order))
        self.assertEqual(script.count("sudo -v\n"), 1)
        self.assertNotIn(mf.SUDOERS_FILE, script)
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        self.assertNotIn("\u2014", script)

    def test_ready_host_has_no_plan_and_autorestart_alone_does_not_block(self) -> None:
        self.assertIsNone(self.plan(preflight_text()))
        text = preflight_text().replace("pf_pmset\t sleep                0",
                                        "pf_pmset\t sleep                0\npf_pmset\t autorestart          0")
        obs = observed(**{"build-mini-1": text})["hosts"]["build-mini-1"]
        result = mf.preflight_host(self.manifest, "build-mini-1", obs)
        self.assertTrue(result["ready"])
        self.assertIn("sudo pmset -a autorestart 1", mf.sudo_plan_script(self.manifest, "build-mini-1", result))

    def test_sudoers_rule_is_narrow_and_checked_before_install(self) -> None:
        script = self.plan(preflight_text(), sudoers=True)
        rule = next(line for line in script.splitlines() if line.startswith("builder ALL="))
        # Never a path rule for Xcode tools: /Applications is admin-writable, so that would be root.
        self.assertEqual(rule, "builder ALL=(root) NOPASSWD: /bin/launchctl kickstart -k system/com.example.build-worker")
        del self.manifest["defaults"]["launchd"]
        self.assertIn("no rule to install", self.plan(preflight_text(), sudoers=True))
        self.assertLess(script.index('sudo visudo -cf "$rule"'), script.index(f"sudo install -m 0440"))
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)

    def test_pending_restart_is_noted_not_run(self) -> None:
        script = self.plan(morning_text() + "pf_update_prepared\tpending|26.7\n")
        self.assertIn("# Not run here: macOS 26.7 is prepared", script)
        self.assertNotIn("shutdown", script.replace("# Not run here", ""))

    def test_automatic_updates_are_planned_off(self) -> None:
        for value in ("1|1", "|", "0|1"):
            script = self.plan(preflight_text().replace("pf_autoupdate\t0|0", f"pf_autoupdate\t{value}"))
            self.assertIn("defaults write /Library/Preferences/com.apple.SoftwareUpdate "
                          "AutomaticallyInstallMacOSUpdates -bool false", script)
            self.assertIn("AutomaticDownload -bool false", script)
            subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        self.assertNotIn("SoftwareUpdate", self.plan(preflight_text()) or "")  # already off: nothing to do

    def test_homebrew_owner_is_validated(self) -> None:
        with self.assertRaises(mf.Failure):
            mf.brew_commands("admin; rm -rf /", ["zig"], True)
        with self.assertRaises(mf.Failure):
            mf.brew_commands("admin", ["zig", "openssl"], True)
        self.assertIn("brew_as pin zig", mf.brew_commands("admin", ["zig", "gh"], True))

    def test_ask_once_plan_runs_as_root_throughout(self) -> None:
        text = morning_text().replace("Xcode.app|accepted|done", "Xcode.app|needed|needed").replace(
            "pf_pmset\t sleep                0", "pf_pmset\t sleep                1")
        obs = observed(**{"build-mini-1": text})["hosts"]["build-mini-1"]
        result = mf.preflight_host(self.manifest, "build-mini-1", obs, "0.16.0")
        script = mf.sudo_plan_script(self.manifest, "build-mini-1", result, sudoers=True, as_root=True)
        body = [line.strip() for line in script.splitlines() if not line.lstrip().startswith("#")]
        self.assertNotIn("sudo -v", script)
        self.assertNotIn("keepalive", script)
        # The only sudo left drops to the Homebrew owner.
        self.assertEqual([line for line in body if re.search(r"(^|[\s;|&(])sudo (?!-H -u admin )", line)], [])
        for step in ("xcode-select -s /Applications/Xcode.app", "env DEVELOPER_DIR=", "pmset -c sleep 0",
                     "sudo -H -u admin env HOMEBREW_NO_ASK=1 /opt/homebrew/bin/brew",
                     'sudo -H -u admin ln -s "/opt/homebrew/opt/rustup/bin/$t"',
                     'cp -nP "$staged"/* "$cache"/', 'chown -h admin "$cache"/*', 'visudo -cf "$rule"',
                     "install -m 0440 -o root"):
            self.assertIn(step, script)
        # Bottles fix staged are in the login user's home, not root's.
        self.assertIn("login_home=~builder", body)
        self.assertIn("exec </dev/null", body)
        self.assertIn('staged="${login_home:-$HOME}/', script)
        self.assertIn('[ "$(id -u)" = 0 ]', script)
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        self.assertNotIn("\u2014", script)
        # The default plan is unchanged: sudo -v, the keepalive, and sudo before each root step.
        default = mf.sudo_plan_script(self.manifest, "build-mini-1", result)
        for step in ("sudo -v\n", "keepalive=$!", "sudo xcode-select -s", "sudo pmset -c sleep 0",
                     'sudo chown -h admin "$cache"/*'):
            self.assertIn(step, default)
        self.assertNotIn("login_home=", default)

    def test_run_needs_a_terminal_and_plans_are_saved(self) -> None:
        obs = observed(**{"build-mini-1": morning_text()})
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(mf, "observe", return_value=obs), \
                contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err, \
                mock.patch.object(mf, "operator_zig_minimum", return_value="0.16.0"):
            with mock.patch.object(sys.stdin, "isatty", return_value=False):
                self.assertEqual(mf.main(["sudo-plan", "build-mini-1", "--run", "--manifest", os.fspath(EXAMPLE)]), 2)
            self.assertIn("from a terminal", err.getvalue())
            code = mf.main(["sudo-plan", "build-mini-1", "--manifest", os.fspath(EXAMPLE), "--log-dir", tmp])
            saved = (Path(tmp) / "build-mini-1.sudo-plan.sh")
            self.assertEqual(code, 0)
            self.assertEqual(saved.stat().st_mode & 0o777, 0o600)
            self.assertIn("sudo-plan build-mini-1 --run", out.getvalue())


FAKE_SSH = """#!/usr/bin/env python3
import json, os, sys
argv = sys.argv[1:]
stdin = "" if "-t" in argv else sys.stdin.read()
with open(os.environ["FAKE_SSH_LOG"], "a") as log:
    log.write(json.dumps({"argv": argv, "stdin": stdin}) + "\\n")
host = argv[argv.index("-l") + 2]
if argv[-1] == "sudo -k -S -p '' true":
    sys.exit(0 if stdin == os.environ["FAKE_PASSWORD"] + "\\n" and host != os.environ.get("FAKE_REJECT") else 1)
sys.exit(0)
"""


class SudoPlanAskOnceTests(unittest.TestCase):
    PASSWORD = "correct horse battery 42"

    def run_plan(self, *flags: str, reject: str = "") -> tuple[int, list[dict], str, Path, mock.Mock]:
        hosts = {"build-mini-1": morning_text(),
                 "build-mini-2": morning_text().replace("hostname\tbuild-mini-1", "hostname\tbuild-mini-2")}
        obs = observed(**hosts)
        with tempfile.TemporaryDirectory() as tmp:
            bindir, logs, calls = Path(tmp) / "bin", Path(tmp) / "logs", Path(tmp) / "ssh-calls.jsonl"
            bindir.mkdir()
            ssh = bindir / "ssh"
            ssh.write_text(FAKE_SSH)
            ssh.chmod(0o755)
            env = {"PATH": f"{bindir}:{os.environ['PATH']}", "FAKE_SSH_LOG": os.fspath(calls),
                   "FAKE_PASSWORD": self.PASSWORD, "FAKE_REJECT": reject}
            with mock.patch.dict(os.environ, env), mock.patch.object(mf, "SSH", os.fspath(ssh)), \
                    mock.patch.object(mf, "observe", return_value=obs), \
                    mock.patch.object(mf, "gate_hosts", return_value={}), \
                    mock.patch.object(mf, "operator_zig_minimum", return_value="0.16.0"), \
                    mock.patch.object(sys.stdin, "isatty", return_value=True), \
                    mock.patch.object(mf.getpass, "getpass", return_value=self.PASSWORD) as asked, \
                    contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
                code = mf.main(["sudo-plan", "build-mini-1", "build-mini-2", "--run", *flags,
                                "--manifest", os.fspath(EXAMPLE), "--log-dir", os.fspath(logs)])
            recorded = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            written = "".join(p.read_text() for p in logs.rglob("*") if p.is_file())
        return code, recorded, out.getvalue() + err.getvalue() + written, logs, asked

    def test_one_password_on_stdin_never_in_argv_or_logs(self) -> None:
        code, calls, text, _, asked = self.run_plan("--ask-once")
        self.assertEqual(code, 0, text)
        self.assertEqual(asked.call_count, 1)
        hosts = [c["argv"][c["argv"].index("-l") + 2] for c in calls]
        # Every host's password is checked before any plan runs; then each plan runs once, per host.
        self.assertEqual(hosts, ["build-mini-1", "build-mini-2", "build-mini-1", "build-mini-2"])
        self.assertEqual([c["argv"][-1] for c in calls[:2]], ["sudo -k -S -p '' true"] * 2)
        for call in calls[2:]:
            self.assertTrue(call["argv"][-1].startswith("sudo -k -S -p '' /bin/bash -c "), call["argv"][-1])
            self.assertIn("xcode-select -s /Applications/Xcode.app", call["argv"][-1])
            self.assertNotIn("sudo -v", call["argv"][-1])
        for call in calls:
            self.assertNotIn("-t", call["argv"])
            self.assertEqual(call["argv"][0], "-T")  # no tty even under a RequestTTY config
            self.assertEqual(call["stdin"], self.PASSWORD + "\n")
            self.assertFalse(any(self.PASSWORD in a for a in call["argv"]))
        self.assertNotIn(self.PASSWORD, text)

    def test_a_refused_password_stops_before_any_plan(self) -> None:
        code, calls, text, _, _ = self.run_plan("--ask-once", reject="build-mini-2")
        self.assertEqual(code, 2)
        self.assertEqual([c["argv"][-1] for c in calls], ["sudo -k -S -p '' true"] * 2)
        self.assertIn("build-mini-2: sudo refused the password", text)
        self.assertIn("no plan ran", text)
        self.assertNotIn(self.PASSWORD, text)

    def test_default_run_still_types_each_password_over_ssh_t(self) -> None:
        code, calls, text, _, asked = self.run_plan()
        self.assertEqual(code, 0, text)
        self.assertEqual(asked.call_count, 0)
        self.assertEqual(len(calls), 2)
        for call in calls:
            self.assertEqual(call["argv"][0], "-t")
            self.assertTrue(call["argv"][-1].startswith("/bin/bash -c "))
            self.assertIn("sudo -v", call["argv"][-1])

    def test_ask_once_needs_run(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(mf.main(["sudo-plan", "build-mini-1", "--ask-once", "--manifest", os.fspath(EXAMPLE)]), 2)
        self.assertIn("--ask-once goes with sudo-plan --run", err.getvalue())


class FixLibraryTests(unittest.TestCase):
    """cmux_mini_fix.sh functions, run locally the way fix_call sends them, against a sandbox HOME."""

    def call(self, home: Path, func: str, *args: str) -> subprocess.CompletedProcess:
        with mock.patch.object(mf, "ssh_stream", return_value=0) as ssh:
            mf.fix_call("h", "u", func, list(args), None, 10)
        command = ssh.call_args.args[2]
        self.assertTrue(command.startswith("/bin/bash -c "))
        return subprocess.run(["bash", "-c", command], capture_output=True, text=True, timeout=60,
                              env={"HOME": os.fspath(home), "PATH": "/usr/bin:/bin",
                                   "GLAEDA_FLEET_DIR": os.fspath(home / "fleet")})

    def test_parses_as_bash_and_never_escalates(self) -> None:
        subprocess.run(["bash", "-n", os.fspath(mf.FIX_LIBRARY)], check=True)
        body = [line for line in mf.FIX_LIBRARY.read_text().splitlines() if not line.lstrip().startswith("#")]
        for word in ("curl", "--force"):
            self.assertFalse([line for line in body if word in line], word)
        # Recursive removal only of this file's own staging directories.
        self.assertEqual([line.strip() for line in body if "rm -r" in line],
                         ['case "$stage" in */.glaeda-share.partial) rm -rf "$stage" ;; esac',
                          'rm -rf "$stage"  # what is left are copies of toolchains this host already had'])
        self.assertFalse([line for line in body if re.search(r"(^|[;&|(]\s*)sudo\b", line.strip())])
        # The only removals are the step lock's own pid file and directory, and the runner held marks it writes.
        self.assertEqual([line.strip() for line in body if "rm " in line and "rm -r" not in line],
                         ["""trap 'rm -f "$HOME/.local/state/glaeda/mini-fleet/step.lock/pid"; rmdir "$HOME/.local/state/glaeda/mini-fleet/step.lock" 2>/dev/null || true' EXIT""",
                          'rm -f "$(held_dir)/$(basename "$dir")"'])

    @unittest.skipUnless(shutil.which("shasum") or shutil.which("sha256sum"), "needs shasum")
    def test_candidate_is_placed_only_when_its_digest_matches(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            bin_dir = home / "bin"
            bin_dir.mkdir()
            if not shutil.which("shasum"):  # Linux CI: shasum -a 256 as sha256sum
                (bin_dir / "shasum").write_text('#!/bin/sh\nshift 2\nexec sha256sum "$@"\n')
                (bin_dir / "shasum").chmod(0o755)
            with mock.patch.object(mf.bootstrap, "CMUX_WORKLOAD_TOOL_PATH", f"{bin_dir}:/usr/bin:/bin"):
                self.assertEqual(self.call(home, "candidate_check", "abc", "a.tar.gz", "0" * 64).returncode, 10)
                self.assertEqual(self.call(home, "candidate_dir", "abc").returncode, 0)
                directory = home / "Library/Caches/cmux-fleet/glaeda-candidate-abc"
                (directory / "a.tar.gz.partial").write_bytes(b"candidate")
                import hashlib
                good = hashlib.sha256(b"candidate").hexdigest()
                bad = self.call(home, "candidate_place", "abc", "a.tar.gz", "f" * 64)
                self.assertEqual(bad.returncode, 3)
                self.assertFalse((directory / "a.tar.gz").exists())
                self.assertEqual(self.call(home, "candidate_place", "abc", "a.tar.gz", good).returncode, 0)
                self.assertEqual((directory / "a.tar.gz").read_bytes(), b"candidate")
                self.assertEqual(self.call(home, "candidate_check", "abc", "a.tar.gz", good).returncode, 0)

    def test_python_link_refuses_a_foreign_python3(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            dist = home / ".local/python-3.14/bin"
            dist.mkdir(parents=True)
            (dist / "python3").write_text('#!/bin/sh\n[ "$1" = --version ] && echo "Python 3.14.0"\nexit 0\n')
            (dist / "python3").chmod(0o755)
            (home / ".local/bin").mkdir()
            (home / ".local/bin/python3").symlink_to("/usr/bin/python3")
            refused = self.call(home, "python_link", "~/.local/python-3.14", "3.13")
            self.assertEqual(refused.returncode, 3, refused.stderr)
            (home / ".local/bin/python3").unlink()
            linked = self.call(home, "python_link", "~/.local/python-3.14", "3.13")
            self.assertEqual(linked.returncode, 0, linked.stderr)
            self.assertEqual(os.readlink(home / ".local/bin/python3"), f"{dist}/python3")
            # Idempotent: its own link is replaced, not refused.
            self.assertEqual(self.call(home, "python_link", "~/.local/python-3.14", "3.13").returncode, 0)

    def test_a_running_step_holds_the_host_lock(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            lock = home / ".local/state/glaeda/mini-fleet/step.lock"
            lock.mkdir(parents=True)
            (lock / "pid").write_text(str(os.getpid()))  # a live process holds it
            held = self.call(home, "candidate_dir", "abc")
            self.assertEqual(held.returncode, 3)
            self.assertIn("still running", held.stderr)
            finished = subprocess.Popen(["true"])
            finished.wait()
            (lock / "pid").write_text(str(finished.pid))  # its process is gone: taken over, then released
            self.assertEqual(self.call(home, "candidate_dir", "abc").returncode, 0)
            self.assertFalse(lock.exists())

    @unittest.skipUnless(shutil.which("git"), "needs git")
    def test_glaeda_sync_moves_only_a_clean_checkout_to_a_commit_that_can_renew(self) -> None:
        env = {"HOME": "/nonexistent", "PATH": "/usr/bin:/bin", "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@e",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@e", "GIT_CONFIG_GLOBAL": "/dev/null"}

        def git(*args: str, cwd: Path) -> str:
            return subprocess.run(["git", *args], cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout.strip()

        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            origin = home / "origin"
            (origin / "scripts").mkdir(parents=True)
            git("init", "-q", "-b", "main", cwd=origin)
            (origin / "scripts/cmux_fleet.py").write_text("")
            (origin / "scripts/glaeda-mini-enroll").write_text('p.add_argument("--no-accept")\n')
            git("add", ".", cwd=origin)
            git("commit", "-qm", "old", cwd=origin)
            old = git("rev-parse", "HEAD", cwd=origin)
            (origin / "scripts/glaeda-mini-enroll").write_text('p.add_argument("--renew")\n')
            git("commit", "-qam", "renew", cwd=origin)
            new = git("rev-parse", "HEAD", cwd=origin)
            git("clone", "-q", os.fspath(origin), os.fspath(home / "glaeda"), cwd=home)
            git("checkout", "-q", "--detach", old, cwd=home / "glaeda")
            (origin / "scripts/cmux_fleet.py").write_text("# later\n")
            git("commit", "-qam", "later", cwd=origin)
            later = git("rev-parse", "HEAD", cwd=origin)

            refused = self.call(home, "glaeda_sync", "f" * 40)
            self.assertNotEqual(refused.returncode, 0)
            self.assertEqual(git("rev-parse", "HEAD", cwd=home / "glaeda"), old)
            moved = self.call(home, "glaeda_sync", new)
            self.assertEqual(moved.returncode, 0, moved.stderr)
            self.assertEqual(git("rev-parse", "HEAD", cwd=home / "glaeda"), new)
            self.assertIn("unchanged", self.call(home, "glaeda_sync", new).stdout)
            fetched = self.call(home, "glaeda_sync", later)  # not in the clone yet: fetched from origin
            self.assertEqual(fetched.returncode, 0, fetched.stderr)
            self.assertEqual(git("rev-parse", "HEAD", cwd=home / "glaeda"), later)
            (home / "glaeda/scripts/cmux_fleet.py").write_text("local edit\n")
            dirty = self.call(home, "glaeda_sync", new)
            self.assertEqual(dirty.returncode, 3)
            self.assertIn("local changes", dirty.stderr)
            (home / "glaeda/scripts/cmux_fleet.py").write_text("# later\n")
            predates = self.call(home, "glaeda_sync", old)
            self.assertEqual(predates.returncode, 3)
            self.assertIn("predates glaeda-mini-enroll --renew", predates.stderr)
            # Every option onboarding passes is asked for, not just --renew.
            lacks = self.call(home, "glaeda_sync", new, *mf.ENROLL_FLAGS)
            self.assertEqual(lacks.returncode, 3)
            self.assertIn("predates glaeda-mini-enroll --class-receipt", lacks.stderr)
            # Already at a commit that lacks an option: refused, never reported unchanged (fix would loop).
            same = self.call(home, "glaeda_sync", later, *mf.ENROLL_FLAGS)
            self.assertEqual(same.returncode, 3)
            self.assertNotIn("unchanged", same.stdout)
            self.assertEqual(git("rev-parse", "HEAD", cwd=home / "glaeda"), later)

    @unittest.skipUnless(shutil.which("git"), "needs git")
    def test_homebrew_fetch_fills_an_empty_prefix_from_a_peer_else_upstream(self) -> None:
        env = {"HOME": "/nonexistent", "PATH": "/usr/bin:/bin", "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@e",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@e", "GIT_CONFIG_GLOBAL": "/dev/null"}

        def git(*args: str, cwd: Path) -> str:
            return subprocess.run(["git", *args], cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout.strip()

        def repo(path: Path, tag: str) -> Path:
            (path / "bin").mkdir(parents=True)
            (path / "bin/brew").write_text(f"#!/bin/sh\necho {tag}\n")
            (path / "bin/brew").chmod(0o755)
            git("init", "-q", "-b", "main", cwd=path)
            git("add", ".", cwd=path)
            git("commit", "-qm", tag, cwd=path)
            return path

        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            upstream, peer = repo(home / "upstream", "upstream"), repo(home / "peer", "peer")
            git("tag", "4.6.0", cwd=upstream)  # the newest release; GitHub's fallback takes it, not HEAD
            (upstream / "bin/brew").write_text("#!/bin/sh\necho unreleased\n")
            git("commit", "-qam", "unreleased", cwd=upstream)

            def fetch(prefix: Path, *args: str) -> subprocess.CompletedProcess:
                with mock.patch.object(mf, "ssh_stream", return_value=0) as ssh:
                    mf.fix_call("h", "u", "homebrew_fetch", [os.fspath(upstream), *args], None, 10)
                return subprocess.run(["bash", "-c", ssh.call_args.args[2]], capture_output=True, text=True, timeout=60,
                                      env={**env, "HOME": tmp, "GLAEDA_HOMEBREW_DIR": os.fspath(prefix),
                                           "GLAEDA_FLEET_DIR": os.fspath(home / "fleet")})

            prefix = home / "homebrew"
            (prefix / "Cellar").mkdir(parents=True)  # a stalled installer leaves empty directories
            done = fetch(prefix, os.fspath(peer))
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual((prefix / "bin/brew").read_text(), "#!/bin/sh\necho peer\n")
            self.assertEqual(git("remote", "get-url", "origin", cwd=prefix), os.fspath(upstream))
            self.assertIn("unchanged", fetch(prefix, os.fspath(peer)).stdout)
            fallback = home / "homebrew2"
            fallback.mkdir()
            done = fetch(fallback, os.fspath(home / "no-such-peer"))
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual((fallback / "bin/brew").read_text(), "#!/bin/sh\necho upstream\n")
            resumed = home / "homebrew4"  # an interrupted fetch: git init done, nothing checked out
            resumed.mkdir()
            git("init", "-q", cwd=resumed)
            done = fetch(resumed)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual((resumed / "bin/brew").read_text(), "#!/bin/sh\necho upstream\n")
            busy = home / "homebrew3"
            busy.mkdir()
            (busy / "notes").write_text("mine")
            refused = fetch(busy)
            self.assertEqual(refused.returncode, 3)
            self.assertIn("has files but no brew", refused.stderr)
            self.assertEqual(fetch(home / "absent").returncode, 3)

    @unittest.skipUnless(shutil.which("perl"), "needs perl")
    def test_a_held_host_refuses_every_changing_step(self) -> None:
        import fcntl
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            fleet = home / "fleet"
            fleet.mkdir()
            self.assertEqual(self.call(home, "host_check").returncode, 0)
            marker = {"schema": "glaeda-reservation/v1", "owner": "cache-bench", "purpose": "measurement",
                      "since": 1, "until": 4102444800}
            (fleet / "reservation.json").write_text(json.dumps(marker))
            checked = self.call(home, "host_check")
            self.assertEqual(checked.returncode, 20)
            self.assertIn("held: reserved by cache-bench for measurement until 2100-01-01T00:00:00Z", checked.stdout)
            refused = self.call(home, "candidate_dir", "abc")  # a changing step: lock() asks first
            self.assertEqual(refused.returncode, 20)
            self.assertIn("reserved by cache-bench", refused.stderr)
            self.assertFalse((home / "Library").exists())
            for bad in ({**marker, "until": "4102444800"}, {**marker, "until": 4102444800.5},
                        {**marker, "schema": "v2"}, "not json"):
                (fleet / "reservation.json").write_text(bad if isinstance(bad, str) else json.dumps(bad))
                self.assertEqual(self.call(home, "host_check").returncode, 20, bad)  # invalid counts as held
            # A far-future until overflowed gmtime and crashed perl, which once read as free.
            (fleet / "reservation.json").write_text(json.dumps({**marker, "until": 9223372036854775807}))
            self.assertEqual(self.call(home, "host_check").returncode, 20)
            (fleet / "reservation.json").write_text(json.dumps(marker).replace("4102444800", "2e3"))  # Python: a float
            self.assertEqual(self.call(home, "host_check").returncode, 20)
            (fleet / "reservation.json").write_text(json.dumps({**marker, "until": 2}))  # expired
            (fleet / "host.lock").write_text("")
            self.assertEqual(self.call(home, "host_check").returncode, 0)
            fd = os.open(fleet / "host.lock", os.O_RDONLY)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX)
                locked = self.call(home, "host_check")
                self.assertEqual(locked.returncode, 20)
                self.assertIn("held by another build (fleet host lock", locked.stdout)
                # A drain honors only reservations: it waits out the job that holds the lock.
                gate = subprocess.run(["bash", "-c", 'say() { :; }; eval "$(sed -n "/^# >>> host gate/,/^# <<< host gate/p" '
                                       f'{mf.FIX_LIBRARY})"; host_held reservation || echo free'],
                                      capture_output=True, text=True, env={"PATH": "/usr/bin:/bin",
                                                                           "GLAEDA_FLEET_DIR": os.fspath(fleet)})
                self.assertEqual(gate.stdout.strip(), "free")
            finally:
                os.close(fd)
            fd = os.open(fleet / "host.lock", os.O_RDONLY)
            try:  # a PR job on a mini with several runners holds it shared: still busy
                fcntl.flock(fd, fcntl.LOCK_SH)
                self.assertEqual(self.call(home, "host_check").returncode, 20)
                fcntl.flock(fd, fcntl.LOCK_UN)
            finally:
                os.close(fd)
            self.assertEqual(self.call(home, "candidate_dir", "abc").returncode, 0)

    def test_runner_plist_knows_every_glaeda_instance(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            agents = home / "Library/LaunchAgents"
            agents.mkdir(parents=True)
            for suffix, label in (("", "com.teamleaderleo.glaeda.cmux-runner"), ("-3", "com.teamleaderleo.glaeda.cmux-runner.3")):
                (home / f"actions-runner-glaeda{suffix}").mkdir()
                (agents / f"{label}.plist").write_text("")
                got = self.call(home, "runner_plist", os.fspath(home / f"actions-runner-glaeda{suffix}"))
                self.assertEqual(got.stdout.strip(), os.fspath(agents / f"{label}.plist"), got.stderr)
            (home / "actions-runner-glaeda-x").mkdir()
            self.assertNotEqual(self.call(home, "runner_plist", os.fspath(home / "actions-runner-glaeda-x")).returncode, 0)

    def test_runner_hold_release_and_kick(self) -> None:
        # launchctl and pgrep are shims ahead of the real ones on the workload PATH, so this runs anywhere.
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            shims = home / "shims"
            shims.mkdir()
            (shims / "launchctl").write_text("""#!/bin/bash
echo "$*" >> "$HOME/launchctl.log"
state="$HOME/loaded"; mkdir -p "$state"
case "$1" in
  print) [ -e "$state/$(basename "$2")" ] ;;
  bootout) rm -f "$state/$(basename "$2")" ;;
  bootstrap) : > "$state/$(basename "$3" .plist)" ;;
  *) exit 0 ;;
esac
""")
            (shims / "pgrep").write_text("""#!/bin/bash
case "$*" in
  *Runner.Worker*) [ -e "$HOME/worker-running" ] ;;
  *Runner.Listener*) [ -e "$HOME/listener-running" ] ;;
  *) exit 1 ;;
esac
""")
            for shim in shims.iterdir():
                shim.chmod(0o755)
            runner = home / "actions-runner-x"
            runner.mkdir()
            (runner / ".runner").write_text('{"agentName": "mini-1"}')
            plist = home / "Library/LaunchAgents/actions.runner.x.plist"
            plist.parent.mkdir(parents=True)
            plist.write_text("")
            (runner / ".service").write_text(f"{plist}\n")
            (home / "loaded").mkdir()
            (home / "loaded/actions.runner.x").write_text("")
            held = home / ".local/state/glaeda/mini-fleet/runner-held/actions-runner-x"
            with mock.patch.object(mf.bootstrap, "CMUX_WORKLOAD_TOOL_PATH", f"{shims}:/usr/bin:/bin"):
                (home / "worker-running").write_text("")
                busy = self.call(home, "runner_hold", "0")
                self.assertEqual(busy.returncode, 3)
                self.assertIn("still running a job", busy.stderr)
                self.assertFalse(held.exists())
                (home / "worker-running").unlink()
                stopped = self.call(home, "runner_hold", "60")
                self.assertEqual(stopped.returncode, 0, stopped.stderr)
                self.assertTrue(held.exists())
                self.assertFalse((home / "loaded/actions.runner.x").exists())
                self.assertIn("unchanged", self.call(home, "runner_hold", "60").stdout)  # a rerun changes nothing
                started = self.call(home, "runner_release")
                self.assertEqual(started.returncode, 0, started.stderr)
                self.assertFalse(held.exists())
                self.assertTrue((home / "loaded/actions.runner.x").exists())
                self.assertEqual(self.call(home, "runner_kick").returncode, 0)  # loaded, no listener: restarted
                (home / "listener-running").write_text("")
                self.assertEqual(self.call(home, "runner_kick").returncode, 0)  # listening: left alone
            log = (home / "launchctl.log").read_text()
            uid = os.getuid()
            self.assertIn(f"disable gui/{uid}/actions.runner.x", log)
            self.assertIn(f"bootstrap gui/{uid} {plist}", log)
            self.assertEqual(log.count("kickstart -k"), 1)

    @unittest.skipUnless(shutil.which("git"), "needs git")
    def test_an_unfinished_clone_is_not_built_on(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            subprocess.run(["git", "init", "-q", os.fspath(home / "cmux")], check=True)
            out = self.call(home, "cmux_clone", "example/cmux", "main", "~/cmux", "1")
            self.assertEqual(out.returncode, 3)
            self.assertIn("unfinished clone", out.stderr)

    def test_clone_refuses_a_directory_that_is_not_a_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / "cmux").mkdir()
            (home / "cmux/notes.txt").write_text("mine")
            out = self.call(home, "cmux_clone", "example/cmux", "main", "~/cmux", "1")
            self.assertEqual(out.returncode, 3)
            self.assertIn("not a git checkout", out.stderr)
            self.assertEqual((home / "cmux/notes.txt").read_text(), "mine")


SOURCE = "59ca9c9bd1bb" + "0" * 28


class OperatorSideTests(unittest.TestCase):
    def test_candidate_downloads_once_and_is_digest_checked(self) -> None:
        import hashlib
        payload = b"archive"
        sha = hashlib.sha256(payload).hexdigest()
        pin = {"source": SOURCE, "sha256": sha, "run": "35991372390", "artifact": "glaeda-candidate-aarch64-apple-darwin",
               "repo": "teamleaderleo/glaeda"}
        with tempfile.TemporaryDirectory() as tmp:
            downloads = []

            def gh(argv, log, stdin=b"", timeout=None):
                downloads.append(argv)
                target = Path(argv[argv.index("--dir") + 1])
                (target / f"glaeda-{SOURCE}-aarch64-apple-darwin.tar.gz").write_bytes(payload)
                return 0

            with mock.patch.object(mf, "PINNED_CANDIDATE", pin), \
                    mock.patch.object(mf, "OPERATOR_CACHE", Path(tmp) / "cache"), \
                    mock.patch.object(mf, "run_logged", side_effect=gh):
                candidate = mf.operator_candidate()
                self.assertEqual(candidate["run"], "35991372390")
                self.assertEqual(mf.operator_candidate_source(), candidate["source"])
                first = mf.fetch_candidate(candidate, None)
                second = mf.fetch_candidate(candidate, None)
                self.assertEqual(first, second)
                self.assertEqual(len(downloads), 1)
                self.assertEqual(downloads[0][:3], ["gh", "run", "download"])
                with self.assertRaisesRegex(mf.Failure, "sha256"):
                    mf.fetch_candidate({**candidate, "sha256": "f" * 64}, None)
        # Nothing pinned: no candidate, and the reason says how to pin one.
        with mock.patch.object(mf, "PINNED_CANDIDATE", None):
            self.assertEqual(mf.operator_candidate(), {})
            with self.assertRaisesRegex(mf.Failure, "upgrade --candidate-run"):
                mf.fetch_candidate({}, None)

    def test_python_dist_is_the_newest_that_meets_the_minimum(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / ".local/share/uv/python"
            for name in ("cpython-3.12.9-macos-aarch64-none", "cpython-3.13.5-macos-aarch64-none",
                         "cpython-3.14.0-macos-aarch64-none"):
                (base / name / "bin").mkdir(parents=True)
                (base / name / "bin/python3").write_text("")
            with mock.patch.dict(os.environ, {"HOME": tmp}):
                os.environ.pop("GLAEDA_MINI_FLEET_PYTHON", None)
                self.assertEqual(mf.operator_python_dist("3.13")[1], "3.14.0")
                self.assertIsNone(mf.operator_python_dist("3.15"))


SSH_SHIM = """#!/bin/bash
# Test stand-in for ssh: each host is a directory under $SHIM_ROOT used as that host's HOME.
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-i|-l) shift 2 ;;
    -A|-t|-T) shift ;;
    *) break ;;
  esac
done
host=$1; shift
exec env HOME="$SHIM_ROOT/$host" bash -c "$*"
"""


class ShareTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        self.candidate = {"source": SOURCE, "sha256": "a" * 64, "run": "1", "artifact": "x", "repo": "a/b"}

    def test_share_arguments_come_from_the_manifest_and_this_mac(self) -> None:
        with mock.patch.object(mf, "operator_candidate", return_value=self.candidate):
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "xcode"), ["/Applications/Xcode.app", "26.3", "17C529"])
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "metal"), ["/Applications/Xcode.app/Contents/Developer"])
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "candidate"),
                             [SOURCE[:12], f"glaeda-{SOURCE}-aarch64-apple-darwin.tar.gz", "a" * 64])
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "python"), ["3.13"])

    def test_lan_address_must_be_on_the_fleet_lan(self) -> None:
        self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "10.0.8.7"
        self.assertEqual(mf.lan_address(self.manifest, "build-mini-2", None), ("10.0.8.7", "10.0.8.7"))
        self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "100.64.1.2"  # a tailnet address is not the LAN
        address, why = mf.lan_address(self.manifest, "build-mini-2", None)
        self.assertIsNone(address)
        self.assertIn("outside the fleet LAN 10.0.8.", why)

    def test_dry_run_checks_the_peer_and_copies_nothing(self) -> None:
        self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "10.0.8.7"
        with mock.patch.object(mf, "ssh_stream", return_value=1) as ssh:
            ok, detail = mf.share_one(self.manifest, "xcode", "build-mini-1", "build-mini-2",
                                      ["/Applications/Xcode.app", "26.3", "17C529"], None, False)
        self.assertTrue(ok)
        self.assertEqual(detail, "would copy over build-mini-1 -> 10.0.8.7 (ssh -A from this Mac)")
        self.assertEqual(ssh.call_count, 1)
        self.assertIn("share_have xcode", ssh.call_args.args[2])

    @unittest.skipUnless(shutil.which("shasum") or shutil.which("sha256sum"), "needs shasum")
    def test_candidate_goes_from_seed_to_peer_over_the_lan_and_verifies(self) -> None:
        import hashlib
        payload = b"candidate bytes"
        sha = hashlib.sha256(payload).hexdigest()
        name = f"glaeda-{SOURCE}-aarch64-apple-darwin.tar.gz"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shim = root / "ssh"
            shim.write_text(SSH_SHIM)
            shim.chmod(0o755)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            if not shutil.which("shasum"):
                (bin_dir / "shasum").write_text('#!/bin/sh\nshift 2\nexec sha256sum "$@"\n')
                (bin_dir / "shasum").chmod(0o755)
            seed = root / "build-mini-1/Library/Caches/cmux-fleet" / f"glaeda-candidate-{SOURCE[:12]}"
            seed.mkdir(parents=True)
            (seed / name).write_bytes(payload)
            (root / "build-mini-2").mkdir()
            (root / "10.0.8.7").symlink_to(root / "build-mini-2")
            self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "10.0.8.7"
            args = [SOURCE[:12], name, sha]
            with mock.patch.object(mf, "SSH", os.fspath(shim)), \
                    mock.patch.object(mf.bootstrap, "CMUX_WORKLOAD_TOOL_PATH", f"{bin_dir}:/usr/bin:/bin"), \
                    mock.patch.dict(os.environ, {"SHIM_ROOT": tmp}), \
                    mock.patch.object(mf, "operator_candidate", return_value={"source": SOURCE, "sha256": sha}):
                self.assertIsNone(mf.prepare_seed(self.manifest, "build-mini-1", "candidate", None, True))
                ok, detail = mf.share_one(self.manifest, "candidate", "build-mini-1", "build-mini-2", args, None, True)
                self.assertTrue(ok, detail)
                placed = root / "build-mini-2/Library/Caches/cmux-fleet" / f"glaeda-candidate-{SOURCE[:12]}" / name
                self.assertEqual(placed.read_bytes(), payload)
                self.assertFalse((placed.parent / ".glaeda-share.partial").exists())
                # A rerun finds it in place and copies nothing.
                self.assertEqual(mf.share_one(self.manifest, "candidate", "build-mini-1", "build-mini-2", args, None, True),
                                 (True, "already there"))
                # A peer holding a different archive under that name is refused, not overwritten.
                placed.write_bytes(b"something else")
                ok, _ = mf.share_one(self.manifest, "candidate", "build-mini-1", "build-mini-2", args, None, True)
                self.assertFalse(ok)
                self.assertEqual(placed.read_bytes(), b"something else")

    def test_share_command_needs_a_source_and_refuses_never_touch(self) -> None:
        for argv, error in ((["share", "xcode", "build-mini-2"], "--from HOST"),
                            (["share", "xcode", "build-mini-2", "--from", "coordinator-mini"], "never_touch"),
                            (["share", "xcode", "--from", "build-mini-1"], "destination"),
                            (["share", "teapot", "build-mini-2", "--from", "build-mini-1"], "share what")):
            with contextlib.redirect_stderr(io.StringIO()) as err, mock.patch.object(mf, "ssh_stream") as ssh:
                self.assertEqual(mf.main([*argv, "--manifest", os.fspath(EXAMPLE)]), 2, argv)
            self.assertIn(error, err.getvalue())
            ssh.assert_not_called()

    def test_fix_from_a_seed_copies_over_the_lan(self) -> None:
        obs = observed(**{"build-mini-1": morning_text()})["hosts"]["build-mini-1"]
        result = mf.preflight_host(self.manifest, "build-mini-1", obs, "0.16.0")
        out = mf.fix_host(self.manifest, "build-mini-1", result, False, None, seed="build-mini-2")
        self.assertIn("python: copy a relocatable CPython 3.13+ from this Mac into ~/.local [from build-mini-2 over the LAN]",
                      out["planned"])
        calls = []
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "prepare_seed", return_value=None), \
                mock.patch.object(mf, "share_one", side_effect=lambda m, thing, src, dst, a, log, yes: (calls.append((thing, src)), (True, ""))[1]), \
                mock.patch.object(mf, "fix_call", return_value=0), mock.patch.object(mf, "operator_candidate", return_value=self.candidate):
            mf.fix_host(self.manifest, "build-mini-1", result, True, Path(tmp), seed="build-mini-2")
        self.assertEqual(calls, [("python", "build-mini-2"), ("metal", "build-mini-2"), ("candidate", "build-mini-2")])
        # A seed that cannot provide Python (a Homebrew one, say) falls back to this Mac's copy.
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "prepare_seed", return_value="share_source python exit 3"), \
                mock.patch.object(mf, "share_one") as share, mock.patch.object(mf, "ship_python", return_value=(True, "")) as ship, \
                mock.patch.object(mf, "fix_call", return_value=0), mock.patch.object(mf, "stage_candidate", return_value=(True, "")):
            out = mf.fix_host(self.manifest, "build-mini-1", result, True, Path(tmp), seed="build-mini-2")
        share.assert_not_called()
        ship.assert_called_once()
        self.assertEqual(out["failed"], [])

    def test_homebrew_comes_from_the_seed_over_the_lan_with_the_operator_agent(self) -> None:
        calls = []
        with mock.patch.object(mf, "lan_address", return_value=("10.0.8.5", "10.0.8.5")), \
                mock.patch.object(mf, "ssh_stream", side_effect=lambda n, u, c, log, stdin=b"", timeout=None, options=():
                                  (calls.append((n, c, options)), 0)[1]):
            ok, detail = mf.fetch_homebrew(self.manifest, "build-mini-1", None, "build-mini-2")
            self.assertEqual((ok, detail), (True, "Homebrew in /opt/homebrew (from build-mini-2)"))
            mf.fetch_homebrew(self.manifest, "build-mini-1", None, None)
        (host, command, options), (_, plain, plain_options) = calls
        self.assertEqual((host, options, plain_options), ("build-mini-1", ("-A",), ()))
        self.assertTrue(remote_call(command).endswith(
            f"homebrew_fetch {mf.HOMEBREW_REPO} ssh://builder@10.0.8.5/opt/homebrew"), remote_call(command))
        self.assertTrue(remote_call(plain).endswith(f"homebrew_fetch {mf.HOMEBREW_REPO}"))

    def test_a_missing_xcode_waits_for_a_seed(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"] = {"app": "/Applications/Xcode_26.6.app", "version": "26.6",
                                                           "build": "17F113"}
        obs = observed(**{"build-mini-1": preflight_text()})["hosts"]["build-mini-1"]
        result = mf.preflight_host(self.manifest, "build-mini-1", obs)
        self.assertEqual(result["checks"]["xcode"]["action"]["kind"], "xcode_share")
        waiting = mf.fix_host(self.manifest, "build-mini-1", result, False, None)["waiting"]
        self.assertIn("waits for --from HOST, a host that has it", waiting[0])
        planned = mf.fix_host(self.manifest, "build-mini-1", result, False, None, seed="build-mini-2")["planned"]
        self.assertTrue(planned[0].startswith("xcode: copy /Applications/Xcode_26.6.app"))


def run_command(texts: dict[str, str], *extra: str, tokens: list[str] | None = None,
                manifest: Path = EXAMPLE, command: str = "onboard", after: dict[str, str] | None = None,
                ssh_codes: dict[str, int] | None = None, ssh_output: dict[str, bytes] | None = None,
                held: dict[str, str] | None = None) -> tuple[int, str, list]:
    """Run a command with SSH stubbed. Probes return `texts` until something enrolls, then `after`
    (default: each host eligible, no runner). ssh_codes and ssh_output: the exit code and log output
    for commands containing a needle."""
    calls: list = []
    loaded = mf.load_manifest(manifest)
    if after is None:
        after = {h: preflight_text(runners=(), node_id=loaded["hosts"][h].get("node_id")) for h in texts}

    def ssh(name, user, command, log, stdin=b"", timeout=None, options=()):
        calls.append((name, command, stdin))
        for needle, output in (ssh_output or {}).items():
            if needle in command and log is not None:
                log.write(output)
        for needle, code in (ssh_codes or {}).items():
            if needle in command:
                return code
        return 0

    def probe(manifest, names, preflight=False):
        enrolled = any(ENROLL in c[1] for c in calls)
        return observed(**{h: (after if enrolled else texts)[h] for h in names})

    with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()) as out, \
            mock.patch.object(mf, "observe", side_effect=probe), mock.patch.object(mf, "ssh_stream", side_effect=ssh), \
            mock.patch.object(mf, "mint_runner_token", return_value="AAAATOKENTOKENTOKENTOKEN") as mint, \
            mock.patch.object(mf, "resolve_glaeda_main", return_value="d" * 40), \
            mock.patch.object(mf, "host_check", side_effect=lambda m, h: (held or {}).get(h)), \
            mock.patch.object(mf, "operator_zig_minimum", return_value="0.16.0"):
        code = mf.main([command, *texts, "--manifest", os.fspath(manifest), "--log-dir", tmp, *extra])
    if tokens is not None:
        tokens.append(mint.call_count)
    return code, out.getvalue(), calls


def remote_call(command: str) -> str:
    """The function call a fix-library command ends with (its last line), or the command itself."""
    if not command.startswith("/bin/bash -c "):
        return command
    return [line for line in shlex.split(command)[2].splitlines() if line.strip()][-1]


def calls_to(calls: list, needle: str) -> list:
    return [c for c in calls if needle in remote_call(c[1])]


# glaeda-mini-enroll as enroll_command quotes it; the fix library also names the script, never this way.
ENROLL = "~/'glaeda/scripts/glaeda-mini-enroll'"


class OnboardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        self.candidate = {"source": "59ca9c9bd1bb" + "0" * 28, "sha256": "a" * 64, "run": "1", "artifact": "x", "repo": "a/b"}
        patcher = mock.patch.object(mf, "operator_candidate", return_value=self.candidate)
        patcher.start()
        self.addCleanup(patcher.stop)

    def fresh(self, **kwargs: object) -> str:
        """A host that passes every check and is not enrolled or registered yet."""
        return preflight_text(enroll_state=None, acceptance=None, runners=(), node_id=None, **kwargs)

    def test_default_hosts_are_node_ids_on_compile_lane_classes(self) -> None:
        # By default, hosts that could never join (no node id, a light class) are left out silently.
        self.assertEqual(mf.onboard_hosts(self.manifest, None), (["build-mini-1", "build-mini-2"], {}))
        targets, skipped = mf.onboard_hosts(self.manifest, ["small-mini"])
        self.assertEqual((targets, skipped), ([], {"small-mini": "no node_id in the manifest"}))
        self.manifest["hosts"]["build-mini-2"]["hardware"] = "m4-16"  # 16 GB hardware never takes the app compile
        targets, skipped = mf.onboard_hosts(self.manifest, ["build-mini-1", "build-mini-2"])
        self.assertEqual(targets, ["build-mini-1"])
        self.assertIn("compile_lane: false", skipped["build-mini-2"])
        with self.assertRaisesRegex(mf.Failure, "never_touch"):
            mf.onboard_hosts(self.manifest, ["coordinator-mini"])

    def test_class_acceptance_must_match_xcode_build_and_candidate(self) -> None:
        recorded, why = mf.class_acceptance(self.manifest, "build-mini-1")
        self.assertEqual(recorded["node"], "cmux-mac-001")
        self.manifest["defaults"]["toolchain"]["xcode"]["build"] = "17C999"
        recorded, why = mf.class_acceptance(self.manifest, "build-mini-1")
        self.assertIsNone(recorded)
        self.assertIn("not 17C999", why)

    def run_onboard(self, texts: dict[str, str], *extra: str, **kwargs: object) -> tuple[int, str, list]:
        return run_command(texts, *extra, **kwargs)

    def test_dry_run_prints_the_plan_and_touches_nothing(self) -> None:
        code, out, calls = self.run_onboard({"build-mini-1": self.fresh(), "build-mini-2": morning_text()})
        self.assertEqual((code, calls), (0, []))
        self.assertIn("would: enroll without acceptance (class)", out)
        self.assertIn("sudo-plan select, rust, zig", out)
        self.assertIn("dry run; pass --yes", out)
        self.assertEqual(out.splitlines()[0].split(), ["host", "class", "node", "id", "generation", "step", "result", "log"])
        self.assertNotIn("—", out)

    def test_class_mode_enrolls_without_acceptance_and_does_not_register(self) -> None:
        minted: list = []
        code, out, calls = self.run_onboard({"build-mini-1": self.fresh()}, "--yes", tokens=minted)
        enroll = calls_to(calls, ENROLL)
        self.assertEqual(len(enroll), 1)
        self.assertIn("--no-accept", enroll[0][1])
        self.assertIn("--node-id cmux-mac-001", enroll[0][1])
        self.assertIn("~/'glaeda/scripts/glaeda-mini-enroll'", enroll[0][1])
        self.assertEqual(minted, [0])
        self.assertIn("enrolled without node acceptance", out)
        self.assertEqual(code, 1)

    def test_class_receipt_goes_over_stdin_and_the_host_registers(self) -> None:
        receipt = {"schema": "glaeda-cmux-fleet-class-acceptance/v1", "fleetClass": "m4pro-48",
                   "receiptSha256": "sha256:" + "c" * 64}
        with tempfile.TemporaryDirectory() as tmp:
            receipt_path = Path(tmp) / "m4pro-48.json"
            receipt_path.write_text(json.dumps(receipt))
            data = copy.deepcopy(self.manifest)
            data["hardware"]["m4pro-48"]["acceptance"].update(receipt=os.fspath(receipt_path),
                                                             receipt_sha256=receipt["receiptSha256"])
            path = write_manifest(tmp, data)
            code, out, calls = self.run_onboard({"build-mini-1": self.fresh()}, "--yes", manifest=path)
            self.assertEqual(code, 0, out)
            enroll = calls_to(calls, ENROLL)
            self.assertEqual(len(enroll), 1)
            self.assertNotIn("--no-accept", enroll[0][1])
            self.assertIn(f"--class-receipt - --class-receipt-sha256 {receipt['receiptSha256']} --fleet-class m4pro-48",
                          enroll[0][1])
            self.assertEqual(enroll[0][2], receipt_path.read_bytes())
            # Enrollment is where onboard ends: nothing registers a runner until the hook is wired.
            self.assertIn("no runner: runner registration is not wired", out)
            # A receipt that is not the recorded one blocks the host before anything runs on it.
            data["hardware"]["m4pro-48"]["acceptance"]["receipt_sha256"] = "sha256:" + "d" * 64
            path = write_manifest(tmp, data)
            code, out, calls = self.run_onboard({"build-mini-1": self.fresh()}, "--yes", manifest=path)
        self.assertEqual(code, 1)
        self.assertIn("is not the recorded", out)
        self.assertFalse(calls_to(calls, ENROLL))

    def test_node_mode_accepts_and_the_registration_hook_gets_the_token_on_stdin(self) -> None:
        minted: list = []
        hook = mock.patch.object(mf, "runner_register_command", return_value="IFS= read -r TOKEN; register")
        with hook:
            code, out, calls = self.run_onboard(
                {"build-mini-1": self.fresh(), "build-mini-2": self.fresh(hostname="Build-Mini-2")},
                "--yes", "--acceptance", "node", tokens=minted)
        self.assertEqual(code, 0, out)
        self.assertEqual(minted, [1])  # one token for the whole run
        enroll = calls_to(calls, ENROLL)
        self.assertTrue(enroll and all("--no-accept" not in c[1] for c in enroll))
        register = [c for c in calls if c[1] == "IFS= read -r TOKEN; register"]
        self.assertEqual(sorted(c[0] for c in register), ["build-mini-1", "build-mini-2"])
        for _, command, stdin in register:
            self.assertEqual(stdin, b"AAAATOKENTOKENTOKENTOKEN\n")
        self.assertNotIn("AAAATOKEN", out)
        rows = [line for line in out.splitlines() if line.startswith("build-mini-")]
        self.assertTrue(all(" register " in line and "registered" in line for line in rows), rows)
        # Without the hook, no token is minted and the table says where registration lives.
        minted = []
        code, out, calls = self.run_onboard({"build-mini-1": self.fresh()}, "--yes", "--acceptance", "node", tokens=minted)
        self.assertEqual((code, minted), (0, [0]))
        self.assertIn("glaeda#1174", out)

    def test_a_host_needing_a_person_stops_at_preflight(self) -> None:
        text = self.fresh(update_running="softwareupdate --install macOS 26.7 --restart")
        code, out, calls = self.run_onboard({"build-mini-1": text}, "--yes")
        self.assertEqual(code, 1)
        self.assertEqual(calls, [])
        self.assertIn("needs a person: update", out)

    def test_sudo_plans_stop_the_host_without_a_terminal(self) -> None:
        text = self.fresh().replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                                    "xcode_select\t/Library/Developer/CommandLineTools")
        with mock.patch.object(sys.stdin, "isatty", return_value=False):
            code, out, _ = self.run_onboard({"build-mini-1": text}, "--yes")
        self.assertEqual(code, 1)
        self.assertIn("waiting: glaeda-mini-fleet sudo-plan build-mini-1 --run", out)
        with mock.patch.object(sys.stdin, "isatty", return_value=False), contextlib.redirect_stderr(io.StringIO()) as err:
            code, _, calls = self.run_onboard({"build-mini-1": text}, "--yes", "--sudo", "run")
        self.assertEqual((code, calls), (2, []))
        self.assertIn("from a terminal", err.getvalue())

    def test_heartbeat_reports_each_running_host(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()) as out:
            log = Path(tmp) / "a.log"
            log.write_text("== enroll\naccept-local: cold cmux build\n")
            results = mf.run_with_heartbeat({"a": lambda: (time.sleep(0.3), 0)[1]}, {"a": log}, "enroll", every=0.1)
        self.assertEqual(results, {"a": 0})
        self.assertIn("heartbeat enroll 0m00s: a: accept-local: cold cmux build", out.getvalue())

    def test_onboarding_fields_are_validated(self) -> None:
        cases = ((("lan",), {"auth": "password"}, "lan.auth"),
                 (("runner",), {"org": "a b"}, "runner.org"),
                 (("hardware", "m4-16", "compile_lane"), "no", "compile_lane"),
                 (("hardware", "m4pro-48", "acceptance"), {"xcode_build": "17C529", "candidate": "x", "node": "cmux-mac-001"},
                  "acceptance needs"),
                 (("hosts", "build-mini-1", "lan_ip"), "mini-1.lan", "lan_ip"))
        for keys, value, error in cases:
            data = copy.deepcopy(self.manifest)
            target = data
            for key in keys[:-1]:
                target = target[key]
            target[keys[-1]] = value
            with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(mf.Failure, error):
                mf.load_manifest(write_manifest(tmp, data))



NEW = "3809eed51fdd" + "1" * 28
OLD_GEN, NEW_GEN = "sha256:" + "1" * 64, "sha256:" + "2" * 64
RECEIPT_SHA = "sha256:" + "c" * 64


def pinned_manifest(tmp: str, recorded_for: str | None = NEW, receipt: bool = True) -> Path:
    """The example manifest pinning candidate NEW, with class m4pro-48 accepted for `recorded_for`."""
    data = json.loads(EXAMPLE.read_text())
    data["candidate"] = {"source": NEW, "sha256": "a" * 64, "run": "36018123850",
                         "artifact": "glaeda-candidate-aarch64-apple-darwin", "repo": "teamleaderleo/glaeda"}
    acceptance = data["hardware"]["m4pro-48"]["acceptance"]
    if recorded_for:
        acceptance["candidate"] = recorded_for[:12]
    if receipt:
        path = Path(tmp) / "m4pro-48.json"
        path.write_text(json.dumps({"fleetClass": "m4pro-48", "receiptSha256": RECEIPT_SHA}))
        acceptance.update(receipt=os.fspath(path), receipt_sha256=RECEIPT_SHA)
    path = Path(tmp) / "m.json"
    path.write_text(json.dumps(data, indent=2) + "\n")
    return path


def on_candidate(staged: bool = True, generation: str = OLD_GEN, state: str = "eligible", **kwargs: object) -> str:
    """An enrolled host probed for candidate NEW: staged or not, its enrollment on `generation`."""
    return preflight_text(candidate=f"{NEW[:12]}|{'yes' if staged else 'no'}|{'yes' if staged else 'no'}",
                          candidate_generation=NEW_GEN if staged else None, enroll_state=state,
                          enroll_generation=generation, **kwargs)


class PinnedRustTests(unittest.TestCase):
    def test_a_pinned_rust_version_is_held_like_stable(self) -> None:
        # Class adoption compares rustc byte for byte, so the manifest may pin rustup_default to 1.98.1.
        manifest = mf.load_manifest(EXAMPLE)
        manifest["defaults"]["toolchain"]["rustup_default"] = "1.98.1"
        for default, state in (("1.98.1-aarch64-apple-darwin (default)", "ok"), ("stable-aarch64-apple-darwin", "fail")):
            text = preflight_text().replace("pf_python", f"pf_rust_default\t{default}\npf_python", 1)
            result = mf.preflight_host(manifest, "build-mini-1", observed(h=text)["hosts"]["h"])
            self.assertEqual(result["checks"]["rust"]["state"], state, result["checks"]["rust"])
        mine, _ = mf.planned_actions(result)
        self.assertEqual([(a["kind"], a.get("toolchain")) for a in mine], [("rust_default", "1.98.1")])


class HostGateTests(unittest.TestCase):
    def test_held_hosts_are_skipped_and_the_run_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = pinned_manifest(tmp)
            texts = {"build-mini-1": on_candidate(), "build-mini-2": on_candidate(node_id="cmux-mac-002")}
            why = "reserved by cache-bench for measurement until 2100-01-01T00:00:00Z"
            code, out, calls = run_command(texts, "--yes", command="repair", manifest=manifest,
                                           after={h: on_candidate(generation=NEW_GEN) for h in texts},
                                           held={"build-mini-2": why})
        self.assertEqual(code, 1)
        self.assertIn(f"held: {why}, skipped", out)
        self.assertEqual({c[0] for c in calls}, {"build-mini-1"})

    def test_wait_polls_until_the_host_is_free(self) -> None:
        answers = iter(["held by another build", "held by another build", None])
        with mock.patch.object(mf, "host_check", side_effect=lambda m, h: next(answers)), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(mf.gate_hosts({}, ["a"], wait=True, poll=0), {})
        self.assertEqual(out.getvalue().count("waiting: a held"), 2)
        with mock.patch.object(mf, "host_check", return_value="held by another build"), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(mf.gate_hosts({}, ["a"], wait=True, poll=0, limit=0), {"a": "held by another build"})
            self.assertEqual(mf.gate_hosts({}, ["a"]), {"a": "held by another build"})
        self.assertIn("a: held: held by another build, skipped (--wait waits for it)", out.getvalue())

    @unittest.skipUnless(shutil.which("perl"), "needs perl")
    def test_a_sudo_plan_checks_the_host_before_it_asks_for_a_password(self) -> None:
        manifest = mf.load_manifest(EXAMPLE)
        text = preflight_text().replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                                        "xcode_select\t/Library/Developer/CommandLineTools")
        script = mf.sudo_plan_script(manifest, "build-mini-1",
                                     mf.preflight_host(manifest, "build-mini-1", observed(h=text)["hosts"]["h"], "0.16.0"))
        self.assertLess(script.index("host_held"), script.index("sudo -v"))
        with tempfile.TemporaryDirectory() as tmp:
            fleet = Path(tmp) / "fleet"
            fleet.mkdir()
            (fleet / "reservation.json").write_text(json.dumps(
                {"schema": "glaeda-reservation/v1", "owner": "o", "purpose": "p", "since": 1, "until": 4102444800}))
            shims = Path(tmp) / "bin"
            shims.mkdir()
            (shims / "sudo").write_text(f"#!/bin/sh\ntouch {tmp}/sudo-ran\n")
            (shims / "sudo").chmod(0o755)
            run = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=60,
                                 env={"PATH": f"{shims}:/usr/bin:/bin", "GLAEDA_FLEET_DIR": os.fspath(fleet)})
            self.assertEqual(run.returncode, 20, run.stderr)
            self.assertIn("held: reserved by o for p", run.stdout)
            self.assertFalse((Path(tmp) / "sudo-ran").exists())


class RepairTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.manifest = pinned_manifest(self.tmp.name)

    def test_staleness_and_quarantine_are_steps_not_blockers(self) -> None:
        manifest = mf.load_manifest(self.manifest)
        with mock.patch.object(mf, "PINNED_CANDIDATE", manifest["candidate"]):
            for text, need, detail in ((on_candidate(), "renew", "runs Glaeda 111111111111"),
                                       (on_candidate(staged=False), "renew", "not candidate 3809eed51fdd"),
                                       (on_candidate(generation=NEW_GEN), "ok", "eligible, accepted"),
                                       (on_candidate(state="quarantined", enroll_reason="failed_acceptance"), "renew",
                                        "quarantined (failed_acceptance)"),
                                       (on_candidate(state="draining"), "leave", "draining")):
                result = mf.preflight_host(manifest, "build-mini-1", observed(h=text)["hosts"]["h"])
                self.assertEqual(mf.enrollment_need(result), need, detail)
                self.assertIn(detail, result["checks"]["enroll"]["detail"])
                self.assertTrue(result["ready"], result["checks"]["enroll"])
            held = on_candidate(generation=NEW_GEN, runners=("actions-runner-x|mini-1|no|no|yes",))
            down = on_candidate(generation=NEW_GEN, runners=("actions-runner-x|mini-1|yes|no|no",))
            for text, want in ((held, "stopped by glaeda-mini-fleet"), (down, "listener not running")):
                check = mf.preflight_host(manifest, "build-mini-1", observed(h=text)["hosts"]["h"])["checks"]["runner"]
                self.assertEqual(check["state"], "todo")
                self.assertIn(want, check["detail"])

    def test_dry_run_names_what_it_would_do_and_touches_nothing(self) -> None:
        code, out, calls = run_command({"build-mini-1": on_candidate(), "build-mini-2": on_candidate(
            state=None, generation=None, node_id=None)}, command="repair", manifest=self.manifest)
        self.assertEqual((code, calls), (0, []))
        self.assertIn("would: renew and adopt the class receipt", out)
        self.assertIn("not enrolled: glaeda-mini-fleet onboard build-mini-2 --yes", out)
        self.assertIn("dry run; pass --yes to repair", out)

    def test_a_stale_host_is_renewed_in_order_and_its_runner_comes_back(self) -> None:
        after = {"build-mini-1": on_candidate(generation=NEW_GEN)}
        code, out, calls = run_command({"build-mini-1": on_candidate()}, "--yes", command="repair",
                                       manifest=self.manifest, after=after)
        self.assertEqual(code, 0, out)
        order = ("lock; glaeda_sync " + "d" * 40, "candidate_check", "lock reservation; runner_hold 2400", "host_check",
                 ENROLL, "lock none; runner_release")
        self.assertEqual([next(k for k in order if k in remote_call(c[1])) for c in calls], list(order))
        enroll = calls_to(calls, ENROLL)[0]
        self.assertIn(f"--renew --class-receipt - --class-receipt-sha256 {RECEIPT_SHA} --fleet-class m4pro-48",
                      enroll[1])
        self.assertEqual(json.loads(enroll[2])["receiptSha256"], RECEIPT_SHA)
        row = next(line for line in out.splitlines() if line.startswith("build-mini-1"))
        self.assertIn("111111111111->222222222222", row)
        self.assertIn("eligible", row)

    def test_an_adoption_that_differs_is_named_and_left_quarantined(self) -> None:
        error = {"error": "this node is not covered by the class m4pro-48 acceptance; run accept-local on it instead. "
                          'Differs in: toolchain.rustc (here "rustc 1.90.0", class "rustc 1.89.0"); '
                          "hardware.memoryGiB (here 64, class 48)"}
        code, out, calls = run_command({"build-mini-1": on_candidate()}, "--yes", command="repair",
                                       manifest=self.manifest, ssh_codes={ENROLL: 1},
                                       ssh_output={ENROLL: (json.dumps(error) + "\n").encode()})
        self.assertEqual(code, 1)
        self.assertIn("quarantined: differs from class m4pro-48 in toolchain.rustc, hardware.memoryGiB", out)
        self.assertIn("glaeda-mini-fleet repair build-mini-1 --acceptance node --yes", out)
        # After a failed renewal the runners come back only if the node still serves; the host decides.
        self.assertEqual([remote_call(c[1]) for c in calls_to(calls, "runner_release")], ["lock none; runner_release if-eligible"])

    def test_no_receipt_for_the_candidate_blocks_before_anything_runs(self) -> None:
        manifest = pinned_manifest(self.tmp.name, recorded_for="59ca9c9bd1bb")
        code, out, calls = run_command({"build-mini-1": on_candidate(state="quarantined",
                                                                     enroll_reason="stale_glaeda_generation")},
                                       "--yes", command="repair", manifest=manifest)
        self.assertEqual((code, calls), (1, []))
        self.assertIn("glaeda-mini-fleet upgrade records it", out)

    def test_an_operator_quarantine_is_left_alone(self) -> None:
        text = on_candidate(state="quarantined", enroll_reason="hardware_failure")
        code, out, calls = run_command({"build-mini-1": text}, "--yes", command="repair", manifest=self.manifest)
        self.assertEqual((code, calls), (1, []))
        self.assertIn("needs a person: enroll", out)
        manifest = mf.load_manifest(self.manifest)
        with mock.patch.object(mf, "PINNED_CANDIDATE", manifest["candidate"]):
            result = mf.preflight_host(manifest, "build-mini-1", observed(h=text)["hosts"]["h"])
        self.assertEqual(mf.enrollment_need(result), "leave")
        self.assertIn("transition-apply ENROLLMENT --to enrolling", result["checks"]["enroll"]["fix"])

    def test_runners_are_restarted_or_released_and_drains_are_left_alone(self) -> None:
        texts = {"build-mini-1": on_candidate(generation=NEW_GEN, runners=("actions-runner-x|mini-1|yes|no|no",)),
                 "build-mini-2": on_candidate(generation=NEW_GEN, runners=("actions-runner-x|mini-2|no|no|yes",),
                                              node_id="cmux-mac-002")}
        code, out, calls = run_command(texts, "--yes", command="repair", manifest=self.manifest, after=texts)
        self.assertEqual(code, 0, out)
        self.assertEqual([c[0] for c in calls_to(calls, "lock none; runner_kick")], ["build-mini-1"])
        self.assertEqual([c[0] for c in calls_to(calls, "lock none; runner_release")], ["build-mini-2"])
        self.assertEqual(calls_to(calls, ENROLL), [])
        draining = {"build-mini-1": on_candidate(state="draining")}
        code, out, calls = run_command(draining, "--yes", command="repair", manifest=self.manifest, after=draining)
        self.assertEqual(calls, [])
        self.assertIn("draining", out)


class UpgradeTests(unittest.TestCase):
    RUN = {"source": NEW, "run": "36018123850", "artifact": "glaeda-candidate-aarch64-apple-darwin",
           "repo": "teamleaderleo/glaeda", "expires": "2026-10-24T15:15:27Z"}

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.manifest = Path(self.tmp.name) / "m.json"  # class m4pro-48 accepted for 59ca9c9bd1bb, no pin
        self.manifest.write_text(EXAMPLE.read_text())
        self.receipt = json.dumps({"schema": "glaeda-cmux-fleet-class-acceptance/v1", "fleetClass": "m4pro-48",
                                   "receiptSha256": RECEIPT_SHA}).encode()

    def upgrade(self, texts: dict[str, str], *extra: str, after: dict[str, str] | None = None,
                **kwargs: object) -> tuple[int, str, list, mock.Mock]:
        with mock.patch.object(mf, "resolve_candidate_run", return_value=dict(self.RUN)), \
                mock.patch.object(mf, "download_run_candidate", return_value="a" * 64), \
                mock.patch.object(mf, "export_class_receipt", return_value=self.receipt) as export:
            code, out, calls = run_command(texts, "--candidate-run", "36018123850", *extra, command="upgrade",
                                           manifest=self.manifest, after=after, **kwargs)
        return code, out, calls, export

    def fleet(self) -> dict[str, str]:
        return {"build-mini-1": on_candidate(staged=False),
                "build-mini-2": on_candidate(staged=False, node_id="cmux-mac-002")}

    def test_dry_run_plans_one_seed_per_class_and_shows_the_manifest_change(self) -> None:
        before = self.manifest.read_text()
        code, out, calls, export = self.upgrade(self.fleet())
        self.assertEqual((code, calls, export.call_count), (0, [], 0))
        self.assertEqual(self.manifest.read_text(), before)
        self.assertIn("class m4pro-48: seed build-mini-1", out)
        self.assertIn('+        "candidate": "3809eed51fdd"', out)
        self.assertIn('"receipt_sha256": "sha256:<from the export>"', out)
        rows = {line.split()[0]: line for line in out.splitlines() if line.startswith("build-mini-")}
        self.assertIn("renew + accept-local", rows["build-mini-1"])
        self.assertIn("record hardware.m4pro-48.acceptance", rows["build-mini-1"])
        self.assertIn("renew + adopt class m4pro-48 receipt", rows["build-mini-2"])
        self.assertNotIn("—", out)

    def test_the_seed_accepts_records_the_class_and_the_rest_adopt_it(self) -> None:
        after = {"build-mini-1": on_candidate(generation=NEW_GEN),
                 "build-mini-2": on_candidate(generation=NEW_GEN, node_id="cmux-mac-002")}
        code, out, calls, export = self.upgrade(self.fleet(), "--yes", after=after)
        self.assertEqual(code, 0, out)
        enrolls = calls_to(calls, ENROLL)
        self.assertEqual([c[0] for c in enrolls], ["build-mini-1", "build-mini-2"])  # the seed first
        self.assertIn("--renew", enrolls[0][1])
        self.assertNotIn("--class-receipt", enrolls[0][1])
        self.assertIn(f"--class-receipt - --class-receipt-sha256 {RECEIPT_SHA} --fleet-class m4pro-48", enrolls[1][1])
        self.assertEqual(enrolls[1][2], self.receipt)
        self.assertEqual(export.call_args.args[1], "build-mini-1")
        data = json.loads(self.manifest.read_text())
        acceptance = data["hardware"]["m4pro-48"]["acceptance"]
        self.assertEqual((acceptance["candidate"], acceptance["node"], acceptance["receipt_sha256"]),
                         ("3809eed51fdd", "cmux-mac-001", RECEIPT_SHA))
        self.assertEqual(Path(acceptance["receipt"]).read_bytes(), self.receipt)
        self.assertEqual(Path(acceptance["receipt"]).stat().st_mode & 0o777, 0o600)
        self.assertEqual(data["candidate"], {**self.RUN, "sha256": "a" * 64})
        self.assertIn(f"+  \"candidate\": {{", out)  # the diff is printed
        rows = {line.split()[0]: line for line in out.splitlines() if line.startswith("build-mini-")}
        self.assertIn("111111111111->222222222222", rows["build-mini-2"])
        # A rerun resumes: the class is recorded for this candidate, every host already runs it.
        code, out, calls, export = self.upgrade(after, "--yes", after=after)
        self.assertEqual((code, export.call_count), (0, 0), out)
        self.assertEqual(calls_to(calls, ENROLL), [])
        self.assertIn("ok: already on 3809eed51fdd", out)

    def test_an_old_glaeda_checkout_is_synced_by_the_renewal_not_refused(self) -> None:
        lacking = "--class-receipt --class-receipt-sha256 --fleet-class"
        fleet = {h: text.replace("pf_glaeda_lacking\t", f"pf_glaeda_lacking\t{lacking}") for h, text in self.fleet().items()}
        after = {"build-mini-1": on_candidate(generation=NEW_GEN),
                 "build-mini-2": on_candidate(generation=NEW_GEN, node_id="cmux-mac-002")}
        code, out, calls, _ = self.upgrade(fleet, "--yes", "--glaeda-ref", "e" * 40, after=after)
        self.assertEqual(code, 0, out)
        syncs = [remote_call(c[1]) for c in calls_to(calls, "glaeda_sync")]
        self.assertEqual(syncs, ["lock; glaeda_sync " + " ".join(["e" * 40, *mf.ENROLL_FLAGS])] * 2)
        # A blocking unknown still blocks when the old checkout is the only failure.
        unknown = {h: re.sub(r"pf_pmset\t.*\n", "", text) for h, text in fleet.items()}
        code, out, calls, _ = self.upgrade(unknown)
        self.assertIn("not ready: power; glaeda-mini-fleet repair build-mini-1", out)
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(mf.main(["preflight", "--glaeda-ref", "main", "--manifest", os.fspath(EXAMPLE)]), 2)
        self.assertIn("--glaeda-ref is a full commit id", err.getvalue())

    def test_a_named_seed_and_a_failed_seed_leave_the_class_untouched(self) -> None:
        code, out, calls, export = self.upgrade(self.fleet(), "--yes", "--seed-per-class", "build-mini-2",
                                                ssh_codes={ENROLL: 1})
        self.assertEqual(code, 1)
        self.assertEqual([c[0] for c in calls_to(calls, ENROLL)], ["build-mini-2"])
        self.assertIn("blocked: class m4pro-48 has no receipt for 3809eed51fdd (its seed build-mini-2 failed)", out)
        self.assertEqual(export.call_count, 0)
        self.assertNotIn("candidate", json.loads(self.manifest.read_text()))
        # No declared Xcode build: nothing to record the acceptance under, so the class is not seeded.
        data = json.loads(self.manifest.read_text())
        del data["defaults"]["toolchain"]["xcode"]
        self.manifest.write_text(json.dumps(data, indent=2) + "\n")
        code, out, calls, export = self.upgrade(self.fleet(), "--yes")
        self.assertEqual((code, calls_to(calls, ENROLL)), (1, []))
        self.assertIn("declares no toolchain.xcode", out)
        with self.assertRaisesRegex(mf.Failure, "two seeds for class m4pro-48"):
            with mock.patch.object(mf, "resolve_candidate_run", return_value=dict(self.RUN)):
                mf.cmd_upgrade(mf.load_manifest(self.manifest), self.manifest, None, "1", "a/b", None,
                               ["build-mini-1", "build-mini-2"], False, None, None)

    def test_a_host_that_adopted_its_class_cannot_seed_it(self) -> None:
        adopted = on_candidate(generation=NEW_GEN).replace("pf_acceptance\taccepted",
                                                           "pf_acceptance\taccepted\npf_acceptance_class\tglaeda-class-acceptance/v1")
        local = on_candidate(generation=NEW_GEN, node_id="cmux-mac-002").replace(
            "pf_acceptance\taccepted", "pf_acceptance\taccepted\npf_acceptance_class\tglaeda-local-acceptance/v1")
        code, out, calls, export = self.upgrade({"build-mini-1": adopted, "build-mini-2": local})
        self.assertIn("class m4pro-48: seed build-mini-2", out)
        code, out, calls, export = self.upgrade({"build-mini-1": adopted, "build-mini-2": adopted})
        self.assertIn("recorded receipt is gone", out)

    def test_the_run_must_be_a_reviewed_candidate(self) -> None:
        good = {"headSha": NEW, "event": "workflow_dispatch", "status": "completed", "conclusion": "success",
                "workflowName": "Fleet candidate bundles"}
        artifacts = [{"name": "glaeda-candidate-aarch64-apple-darwin", "expired": False,
                      "expires_at": "2026-10-24T15:15:27Z"}]

        def gh(view: dict, compare: str = "behind", arts: list = artifacts):
            return lambda args: view if args[0] == "run" else ({"status": compare} if "compare" in args[1] else arts)

        with mock.patch.object(mf, "gh_json", side_effect=gh(good)):
            got = mf.resolve_candidate_run("36018123850", "teamleaderleo/glaeda", None)
        self.assertEqual(got, {**self.RUN})
        for view, compare, arts, error in (({**good, "event": "pull_request"}, "behind", artifacts, "only a dispatched"),
                                           ({**good, "conclusion": "failure"}, "behind", artifacts, "completed/success"),
                                           (good, "diverged", artifacts, "not on teamleaderleo/glaeda main"),
                                           (good, "behind", [{**artifacts[0], "expired": True}], "no unexpired")):
            with self.subTest(error=error), mock.patch.object(mf, "gh_json", side_effect=gh(view, compare, arts)), \
                    self.assertRaisesRegex(mf.Failure, error):
                mf.resolve_candidate_run("36018123850", "teamleaderleo/glaeda", None)
        with self.assertRaisesRegex(mf.Failure, "64-hex"):
            mf.resolve_candidate_run("1", "a/b", "abc")

    def test_the_download_is_checked_against_the_build_receipt(self) -> None:
        import hashlib
        payload = b"candidate archive"
        sha = hashlib.sha256(payload).hexdigest()
        name = f"glaeda-{NEW}-aarch64-apple-darwin.tar.gz"

        def download(receipt: dict):
            def run(argv, log, stdin=b"", timeout=None):
                target = Path(argv[argv.index("--dir") + 1])
                (target / name).write_bytes(payload)
                (target / "receipt.json").write_text(json.dumps(receipt))
                return 0
            return run

        good = {"archive": name, "sha256": sha, "target": "aarch64-apple-darwin",
                "source": {"repository": "teamleaderleo/glaeda", "commit": NEW, "tree": "e" * 40}}
        for receipt, pinned, error in (({**good, "sha256": "f" * 64}, None, "receipt says"),
                                       ({**good, "source": {**good["source"], "commit": "0" * 40}}, None, "does not describe"),
                                       (good, "f" * 64, "differs from the run's receipt")):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as tmp, \
                    mock.patch.object(mf, "OPERATOR_CACHE", Path(tmp)), \
                    mock.patch.object(mf, "run_logged", side_effect=download(receipt)), \
                    self.assertRaisesRegex(mf.Failure, error):
                mf.download_run_candidate({**self.RUN, **({"sha256": pinned} if pinned else {})}, None)
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(mf, "OPERATOR_CACHE", Path(tmp)), \
                mock.patch.object(mf, "run_logged", side_effect=download(good)) as run:
            self.assertEqual(mf.download_run_candidate(dict(self.RUN), None), sha)
            self.assertEqual(mf.download_run_candidate({**self.RUN, "sha256": sha}, None), sha)  # cached
            self.assertEqual(run.call_count, 1)
            cached = mf.fetch_candidate({**self.RUN, "sha256": sha}, None)  # what stage_candidate then uses
            self.assertEqual(cached.read_bytes(), payload)
            self.assertEqual(run.call_count, 1)
            mf.download_run_candidate({**self.RUN, "run": "36018123851"}, None)  # another run of the source: fetched
            self.assertEqual(run.call_count, 2)

    def test_commands_quote_for_the_host(self) -> None:
        manifest = mf.load_manifest(EXAMPLE)
        result = mf.preflight_host(manifest, "build-mini-1", observed(h=preflight_text())["hosts"]["h"])
        candidate = {**self.RUN, "sha256": "a" * 64}
        command = mf.enroll_command(manifest, "build-mini-1", result, candidate, False, ("m4pro-48", RECEIPT_SHA), renew=True)
        self.assertIn(" --apply --renew --class-receipt - ", command)
        self.assertIn("~/'glaeda/scripts/glaeda-mini-enroll'", command)
        export = mf.export_command(manifest, "build-mini-1", result, candidate, "m4pro-48")
        self.assertIn('G="$HOME/Projects/glaeda-generations/3809eed51fdd"', export)
        self.assertIn('export-class-acceptance "$F/enrollment.json" --acceptance', export)
        self.assertIn('--cmux-root "$HOME/cmux"', export)

    def test_the_manifest_pin_is_validated(self) -> None:
        data = json.loads(EXAMPLE.read_text())
        data["candidate"] = {"source": "x", "sha256": "a" * 64, "run": "1", "artifact": "a", "repo": "a/b"}
        with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(mf.Failure, "candidate needs"):
            mf.load_manifest(write_manifest(tmp, data))
def reservation_line(owner: str = "leo@air", purpose: str = "chromium campaign", since: int | None = None,
                     until: int | None = None, raw: bytes | None = None) -> str:
    """The probe's line for a marker on the host: raw bytes, base64 on one line."""
    now = int(time.time())
    if raw is None:
        raw = json.dumps({"schema": "glaeda-reservation/v1", "owner": owner, "purpose": purpose,
                          "since": since if since is not None else now - 60,
                          "until": until if until is not None else now + 3600}).encode()
    return "reservation_raw\t" + base64.b64encode(raw).decode() + "\n"


class ReservationProbeParsingTests(unittest.TestCase):
    def test_parses_the_raw_marker_with_the_shared_rule(self) -> None:
        got = mf.parse_probe(reservation_line(purpose="build a | b\tc", since=100, until=200) + "host_lock\theld\n")
        self.assertEqual(got["reservation"], {"valid": True, "owner": "leo@air", "purpose": "build a | b\tc",
                                              "since": 100, "until": 200})
        self.assertEqual(got["host_lock"], "held")
        self.assertEqual(mf.parse_probe("runner_worker\trunning\n")["runner_worker"], "running")
        self.assertEqual(mf.parse_probe("runner_worker\tabsent\n")["runner_worker"], "absent")
        self.assertIs(mf.reservation_rules, sys.modules["glaeda_reservation"])

    def test_invalid_markers_say_why(self) -> None:
        good = {"schema": "glaeda-reservation/v1", "owner": "a", "purpose": "b", "since": 1, "until": 2}
        for raw, why in ((b"not json", "not JSON"), (json.dumps({**good, "until": 2.5}).encode(), "until"),
                         (json.dumps({**good, "until": True}).encode(), "until"),
                         (json.dumps({**good, "schema": "v0"}).encode(), "schema"),
                         (b"\xff", "UTF-8"), (b" " * 4097, "over 4096")):
            got = mf.parse_probe(reservation_line(raw=raw))["reservation"]
            self.assertFalse(got["valid"], raw[:30])
            self.assertIn(why, got["why"])
        self.assertFalse(mf.parse_probe("reservation_raw\t!!notbase64\n")["reservation"]["valid"])
        self.assertEqual(mf.parse_probe("reservation\tinvalid\n")["reservation"]["valid"], False)

    def test_no_marker_is_none(self) -> None:
        got = mf.parse_probe("user\tbuilder\n")
        self.assertEqual((got["reservation"], got["host_lock"]), (None, None))

    def test_state_is_active_only_before_until(self) -> None:
        r = {"valid": True, "owner": "a", "purpose": "b", "since": 0, "until": 100}
        self.assertEqual([mf.reservation_state(r, t) for t in (99, 100)], ["active", "expired"])
        self.assertEqual(mf.reservation_state({"valid": False}, 0), "invalid")
        self.assertIsNone(mf.reservation_state(None, 0))


@unittest.skipUnless(shutil.which("perl") and shutil.which("base64"), "needs perl and base64")
class ReservationProbeScriptTests(unittest.TestCase):
    """Runs the real probe against a temporary fleet root."""

    def run_probe(self, root: Path) -> dict:
        text = (ROOT / "scripts" / "cmux_mini_probe.sh").read_text().replace(mf.FLEET_ROOT, os.fspath(root))
        out = subprocess.run(["bash", "-s"], input=text, capture_output=True, text=True, timeout=60,
                             env={"HOME": os.fspath(root), "PATH": "/usr/bin:/bin"}).stdout
        return mf.parse_probe(out)

    @unittest.skipUnless(os.path.exists("/usr/bin/perl"), "the probe runs /usr/bin/perl")
    def test_marker_and_lock_states(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            got = self.run_probe(root)
            self.assertEqual((got["reservation"], got["host_lock"]), (None, "missing"))
            (root / "reservation.json").write_text(json.dumps(
                {"schema": mf.RESERVATION_SCHEMA, "owner": "leo@air", "purpose": "two\nlines", "since": 5, "until": 9}))
            lock = root / "host.lock"
            lock.touch()
            got = self.run_probe(root)
            self.assertEqual(got["reservation"], {"valid": True, "owner": "leo@air", "purpose": "two\nlines",
                                                  "since": 5, "until": 9})
            self.assertEqual(got["host_lock"], "free")
            holder = subprocess.Popen(["perl", "-MFcntl=:flock", "-e",
                                       'open(my $f, "+<", $ARGV[0]) or die; flock($f, LOCK_EX) or die; '
                                       '$| = 1; print "locked\n"; sleep 30', os.fspath(lock)], stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(holder.stdout.readline(), "locked\n")
                started = time.monotonic()
                self.assertEqual(self.run_probe(root)["host_lock"], "held")
                self.assertLess(time.monotonic() - started, 20)  # never waits for the holder
            finally:
                holder.kill()
                holder.wait()
                holder.stdout.close()
            self.assertEqual(self.run_probe(root)["host_lock"], "free")
            import fcntl
            shared = os.open(lock, os.O_RDONLY)
            try:  # a PR job's shared share of the host counts as held
                fcntl.flock(shared, fcntl.LOCK_SH)
                self.assertEqual(self.run_probe(root)["host_lock"], "held")
            finally:
                os.close(shared)
            lock.unlink()
            lock.symlink_to(root / "reservation.json")
            self.assertEqual(self.run_probe(root)["host_lock"], "unknown")  # never follows a symlinked lock

    def test_symlinked_oversized_and_bad_markers_are_invalid(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            marker = root / "reservation.json"
            marker.write_bytes(b"{" + b" " * 5000 + b"}")
            self.assertIn("over 4096", self.run_probe(root)["reservation"]["why"])
            marker.write_text("garbage")
            self.assertEqual(self.run_probe(root)["reservation"]["why"], "not JSON")
            marker.unlink()
            (root / "elsewhere.json").write_text(json.dumps(
                {"schema": mf.RESERVATION_SCHEMA, "owner": "a", "purpose": "b", "since": 1, "until": 2}))
            marker.symlink_to(root / "elsewhere.json")
            self.assertFalse(self.run_probe(root)["reservation"]["valid"])


class ReservationCheckAndPoolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def issues(self, extra: str) -> list[dict]:
        return mf.check(self.manifest, observed(**{"build-mini-1": probe_text() + extra}), ["build-mini-1"])

    def test_active_reservation_is_informational(self) -> None:
        issues = self.issues(reservation_line())
        self.assertEqual(sorted(i["area"] for i in issues), ["pending", "reserved"])
        self.assertIn("reserved by leo@air for chromium campaign until",
                      [i for i in issues if i["area"] == "reserved"][0]["detail"])

    def test_expired_and_invalid_markers_are_drift_with_release_fix(self) -> None:
        now = int(time.time())
        expired = [i for i in self.issues(reservation_line(since=now - 7200, until=now - 60)) if i["area"] == "reservation"]
        self.assertEqual(len(expired), 1)
        self.assertEqual(expired[0]["fix"], "glaeda-mini-fleet release build-mini-1 --yes")
        invalid = [i for i in self.issues(reservation_line(raw=b"{}")) if i["area"] == "reservation"]
        self.assertEqual(invalid[0]["fix"].split(", then ")[1], "glaeda-mini-fleet release build-mini-1 --force --yes")
        self.assertIn("schema", invalid[0]["detail"])

    def test_a_time_no_calendar_holds_is_an_invalid_marker(self) -> None:
        # glaeda_reservation.parse rejects times past year 9999, so check reports drift, never a crash.
        for until in (10**12, 10**20):
            issues = [i for i in self.issues(reservation_line(until=until)) if i["area"] == "reservation"]
            self.assertEqual(len(issues), 1)
            self.assertIn("253402300799", issues[0]["detail"])

    def test_reservation_beyond_the_cap_is_also_drift(self) -> None:
        now = int(time.time())
        areas = [i["area"] for i in self.issues(reservation_line(until=now + 100 * 3600))]
        self.assertIn("reserved", areas)
        self.assertIn("reservation", areas)
        self.assertNotIn("reservation", [i["area"] for i in self.issues(reservation_line(until=now + 71 * 3600))])

    def test_expiry_is_judged_at_observation_time(self) -> None:
        obs = observed(**{"build-mini-1": probe_text() + reservation_line(since=100, until=200)})
        obs["hosts"]["build-mini-1"]["observed_at"] = 150
        self.assertIn("reserved", [i["area"] for i in mf.check(self.manifest, obs, ["build-mini-1"])])

    def test_unknown_host_lock_is_drift(self) -> None:
        self.assertIn("host-lock", [i["area"] for i in self.issues("host_lock\tunknown\n")])
        self.assertNotIn("host-lock", [i["area"] for i in self.issues("host_lock\tfree\n")])

    def test_cli_exit_and_summary_ignore_reserved(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            obs = Path(tmp) / "obs.json"
            obs.write_text(json.dumps(observed(**{"build-mini-1": probe_text() + reservation_line()})))
            with contextlib.redirect_stdout(io.StringIO()) as out:
                code = mf.main(["check", "--manifest", os.fspath(EXAMPLE), "build-mini-1", "--observed", os.fspath(obs)])
        self.assertEqual(code, 0, out.getvalue())
        self.assertIn("ok (1 pending, 1 reserved)", out.getvalue())

    def test_pools_exclude_reserved_members_and_only_list_busy_ones(self) -> None:
        label = "glaeda-std-xcode-26.3"
        m = ["build-mini-1"]
        for extra, conforming, reserved, busy in ((reservation_line(), [], m, []),
                                                  (reservation_line(raw=b"garbage"), [], m, []),  # invalid refuses too
                                                  ("host_lock\theld\n", m, [], m),  # busy still conforms
                                                  (reservation_line() + "host_lock\theld\n", [], m, m)):
            got = mf.pools(self.manifest, observed(**{"build-mini-1": probe_text() + extra}), m)[label]
            self.assertEqual((got["conforming"], got["reserved"], got["busy"]), (conforming, reserved, busy), extra)
            self.assertEqual((got["conforming_count"], got["reserved_count"], got["busy_count"]),
                             (len(conforming), len(reserved), len(busy)))
        now = int(time.time())
        for extra in (reservation_line(since=now - 7200, until=now - 1), "host_lock\tfree\n", "host_lock\tunknown\n"):
            got = mf.pools(self.manifest, observed(**{"build-mini-1": probe_text() + extra}), ["build-mini-1"])[label]
            self.assertEqual((got["conforming"], got["reserved"], got["busy"]), (["build-mini-1"], [], []), extra)


@unittest.skipUnless(shutil.which("bash") and shutil.which("shasum") and os.path.exists("/usr/bin/perl"),
                     "needs bash, shasum and /usr/bin/perl")
class ReserveReleaseTests(unittest.TestCase):
    """SSH is stubbed by running each remote script locally against a temporary fleet root."""

    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp)
        self.root = Path(tmp)
        self.calls: list = []
        patches = (mock.patch.object(mf, "FLEET_ROOT", tmp), mock.patch.object(mf, "reservation_ssh", self.ssh),
                   mock.patch.object(mf, "default_owner", lambda: "leo@air-blue"))
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def ssh(self, name: str, user: str, script: str, args: list[str], stdin: str = "") -> tuple[int, bytes, str]:
        self.calls.append((name, user, script, args, stdin))
        proc = subprocess.run(["bash", "-c", script, "glaeda", *args], input=stdin.encode(), capture_output=True,
                              timeout=30)
        return proc.returncode, proc.stdout, proc.stderr.decode().strip()

    def main(self, *argv: str) -> tuple[int, str]:
        with contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
            code = mf.main([*argv, "--manifest", os.fspath(EXAMPLE)])
        return code, out.getvalue() + err.getvalue()

    def marker(self) -> dict | None:
        path = self.root / "reservation.json"
        return json.loads(path.read_text()) if path.exists() else None

    def put(self, owner: str, until_delta: int, since_delta: int = -600) -> dict:
        now = int(time.time())
        record = {"schema": mf.RESERVATION_SCHEMA, "owner": owner, "purpose": "theirs", "since": now + since_delta,
                  "until": now + until_delta}
        (self.root / "reservation.json").write_text(json.dumps(record))
        return record

    def writes(self) -> list:
        return [c for c in self.calls if c[2] in (mf.WRITE_RESERVATION, mf.REMOVE_RESERVATION)]

    def test_dry_run_reads_but_writes_nothing(self) -> None:
        code, out = self.main("reserve", "build-mini-1", "--for", "chromium campaign", "--hours", "6")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.writes(), [])
        self.assertIsNone(self.marker())
        self.assertIn("would reserve", out)
        self.assertIn('"owner": "leo@air-blue"', out)
        self.assertIn("dry run; pass --yes", out)
        (name, user, script, args, _stdin), = self.calls
        self.assertEqual((name, user, script, args), ("build-mini-1", self.manifest["ssh_user"], mf.READ_RESERVATION,
                                                      [os.fspath(self.root)]))

    def test_apply_writes_the_marker_atomically_and_reads_it_back(self) -> None:
        before = int(time.time())
        code, out = self.main("reserve", "build-mini-1", "--for", "chromium campaign", "--hours", "6", "--yes")
        self.assertEqual(code, 0, out)
        got = self.marker()
        self.assertEqual((got["schema"], got["owner"], got["purpose"]), (mf.RESERVATION_SCHEMA, "leo@air-blue", "chromium campaign"))
        self.assertAlmostEqual(got["until"] - got["since"], 6 * 3600, delta=2)
        self.assertGreaterEqual(got["since"], before)
        self.assertEqual(list(got), ["schema", "owner", "purpose", "since", "until"])
        self.assertEqual((self.root / "reservation.json").stat().st_mode & 0o777, 0o664)
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), [".reservation.lock", "reservation.json"])  # no temp left behind
        (_n, _u, script, args, stdin), = self.writes()
        self.assertEqual((script, args), (mf.WRITE_RESERVATION, [os.fspath(self.root), "absent"]))
        self.assertEqual(json.loads(stdin), got)
        self.assertEqual(self.calls[-1][2], mf.READ_RESERVATION)  # fresh read after the effect

    def test_limits_and_arguments(self) -> None:
        now = int(time.time())
        for argv in (["--hours", "73"], ["--until", str(now - 5)], ["--hours", "1", "--until", str(now + 60)], [],
                     ["--hours", "1", "--owner", "a|b"]):
            code, out = self.main("reserve", "build-mini-1", "--for", "x", *argv)
            self.assertEqual(code, 2, argv)
        self.assertEqual(self.main("reserve", "--for", "x", "--hours", "1")[0], 2)  # explicit hosts only
        self.assertEqual(self.main("reserve", "build-mini-1", "--hours", "1")[0], 2)  # --for required
        self.assertEqual(self.main("reserve", "coordinator-mini", "--for", "x", "--hours", "1")[0], 2)  # never_touch
        self.assertEqual(self.calls, [])
        code, out = self.main("reserve", "build-mini-1", "--for", "x", "--until", str(now + 72 * 3600 - 5), "--yes")
        self.assertEqual(code, 0, out)

    def test_foreign_active_reservation_is_refused_and_printed(self) -> None:
        theirs = self.put("lawrence@studio", 3600)
        code, out = self.main("reserve", "build-mini-1", "--for", "mine", "--hours", "2", "--yes")
        self.assertEqual(code, 1)
        self.assertIn("refused: reserved by lawrence@studio for theirs until", out)
        self.assertEqual(self.marker(), theirs)
        self.assertEqual(self.writes(), [])

    def test_force_takes_over(self) -> None:
        self.put("lawrence@studio", 3600)
        code, out = self.main("reserve", "build-mini-1", "--for", "mine", "--hours", "2", "--force", "--yes")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.marker()["owner"], "leo@air-blue")
        self.assertIn("take over from lawrence@studio", out)

    def test_extending_your_own_keeps_since(self) -> None:
        mine = self.put("leo@air-blue", 600, since_delta=-3600)
        code, out = self.main("reserve", "build-mini-1", "--for", "more", "--hours", "10", "--yes")
        self.assertEqual(code, 0, out)
        got = self.marker()
        self.assertEqual((got["since"], got["purpose"]), (mine["since"], "more"))
        self.assertGreater(got["until"], mine["until"])
        self.assertIn("extend", out)

    def test_expired_foreign_reservation_is_replaced(self) -> None:
        self.put("lawrence@studio", -60, since_delta=-7200)
        code, out = self.main("reserve", "build-mini-1", "--for", "mine", "--hours", "1", "--yes")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.marker()["owner"], "leo@air-blue")
        self.assertIn("replace expired", out)

    def test_invalid_marker_needs_force(self) -> None:
        (self.root / "reservation.json").write_text("garbage")
        self.assertEqual(self.main("reserve", "build-mini-1", "--for", "x", "--hours", "1", "--yes")[0], 1)
        self.assertEqual(self.main("release", "build-mini-1", "--yes")[0], 1)
        self.assertEqual((self.root / "reservation.json").read_text(), "garbage")
        self.assertEqual(self.main("release", "build-mini-1", "--force", "--yes")[0], 0)
        self.assertIsNone(self.marker())

    def test_a_marker_that_changes_after_the_read_is_not_overwritten(self) -> None:
        real = self.ssh

        def racing(name: str, user: str, script: str, args: list[str], stdin: str = "") -> tuple[int, bytes, str]:
            if script == mf.WRITE_RESERVATION:
                self.put("someone@else", 3600)
            return real(name, user, script, args, stdin)

        with mock.patch.object(mf, "reservation_ssh", racing):
            code, out = self.main("reserve", "build-mini-1", "--for", "x", "--hours", "1", "--yes")
        self.assertEqual(code, 1)
        self.assertIn("changed since it was read", out)
        self.assertEqual(self.marker()["owner"], "someone@else")
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), [".reservation.lock", "reservation.json"])

    def test_release_only_by_owner_unless_forced(self) -> None:
        theirs = self.put("lawrence@studio", 3600)
        code, out = self.main("release", "build-mini-1", "--yes")
        self.assertEqual(code, 1)
        self.assertIn("not leo@air-blue", out)
        self.assertEqual(self.marker(), theirs)
        code, out = self.main("release", "build-mini-1", "--owner", "lawrence@studio")
        self.assertEqual((code, self.marker()), (0, theirs))  # dry run
        self.assertIn("would release active reservation by lawrence@studio", out)
        code, out = self.main("release", "build-mini-1", "--owner", "lawrence@studio", "--yes")
        self.assertEqual(code, 0, out)
        self.assertIsNone(self.marker())
        self.put("lawrence@studio", 3600)
        self.assertEqual(self.main("release", "build-mini-1", "--force", "--yes")[0], 0)
        self.assertIsNone(self.marker())

    def test_expired_marker_is_released_by_anyone_and_absent_is_fine(self) -> None:
        self.put("lawrence@studio", -60, since_delta=-7200)
        code, out = self.main("release", "build-mini-1", "--yes")
        self.assertEqual(code, 0, out)
        self.assertIsNone(self.marker())
        self.assertIn("released expired reservation", out)
        code, out = self.main("release", "build-mini-1", "--yes")
        self.assertEqual(code, 0, out)
        self.assertIn("not reserved", out)

    def test_limits_reject_non_finite_hours(self) -> None:
        for value in ("nan", "inf"):
            self.assertEqual(self.main("reserve", "build-mini-1", "--for", "x", "--hours", value)[0], 2)
        self.assertEqual(self.calls, [])

    def test_writers_serialize_on_the_host(self) -> None:
        lock = self.root / ".reservation.lock"
        lock.touch()
        holder = subprocess.Popen(["perl", "-MFcntl=:flock", "-e",
                                   'open(my $f, ">>", $ARGV[0]) or die; flock($f, LOCK_EX) or die; '
                                   '$| = 1; print "locked\n"; sleep 60', os.fspath(lock)], stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline(), "locked\n")
            code, out = self.main("reserve", "build-mini-1", "--for", "x", "--hours", "1", "--yes")
        finally:
            holder.kill()
            holder.wait()
            holder.stdout.close()
        self.assertEqual(code, 1)
        self.assertIn("still running", out)
        self.assertIsNone(self.marker())

    def test_unwritable_fleet_root_is_refused_plainly(self) -> None:
        if os.geteuid() == 0:
            self.skipTest("root writes anywhere")
        self.root.chmod(0o555)
        self.addCleanup(self.root.chmod, 0o755)
        code, out = self.main("reserve", "build-mini-1", "--for", "x", "--hours", "1", "--yes")
        self.assertEqual(code, 1)
        self.assertIn("not writable by", out)

    def test_dry_run_leaves_no_file_anywhere(self) -> None:
        self.put("lawrence@studio", -60)
        before = sorted(p.name for p in self.root.iterdir())
        with mock.patch.dict(os.environ, {"TMPDIR": os.fspath(self.root)}):
            self.assertEqual(self.main("reserve", "build-mini-1", "--for", "x", "--hours", "1")[0], 0)
            self.assertEqual(self.main("release", "build-mini-1")[0], 0)
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), before)
        self.assertEqual({c[2] for c in self.calls}, {mf.READ_RESERVATION})

    def test_oversized_and_non_utf8_markers_are_invalid(self) -> None:
        for raw in (b"{" + b" " * 70000 + b"}", b"\xff\xfe"):
            (self.root / "reservation.json").write_bytes(raw)
            self.assertEqual(mf.read_reservation("build-mini-1", "cmux")["state"], "invalid")
        # Over the read limit nothing can be bound to a digest, so even --force leaves it for a person.
        (self.root / "reservation.json").write_bytes(b" " * 70000)
        self.assertEqual(self.main("release", "build-mini-1", "--force", "--yes")[0], 1)
        self.assertTrue((self.root / "reservation.json").exists())
        (self.root / "reservation.json").write_bytes(b"\xff\xfe")
        self.assertEqual(self.main("release", "build-mini-1", "--force", "--yes")[0], 0)
        self.assertIsNone(self.marker())

    def test_symlinked_marker_is_never_followed(self) -> None:
        target = self.root / "elsewhere"
        target.write_text("keep")
        (self.root / "reservation.json").symlink_to(target)
        for argv in (["reserve", "build-mini-1", "--for", "x", "--hours", "1", "--force", "--yes"],
                     ["release", "build-mini-1", "--force", "--yes"]):
            self.assertEqual(self.main(*argv)[0], 1, argv)
        self.assertTrue((self.root / "reservation.json").is_symlink())
        self.assertEqual(target.read_text(), "keep")


class ReservationScriptShapeTests(unittest.TestCase):
    def test_scripts_parse_as_bash(self) -> None:
        for script in (mf.READ_RESERVATION, mf.WRITE_RESERVATION, mf.REMOVE_RESERVATION):
            subprocess.run(["bash", "-n"], input=script, text=True, check=True)

    def test_ssh_command_is_batch_mode_and_quoted(self) -> None:
        with mock.patch.object(mf.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"absent\n", b"")) as run:
            self.assertEqual(mf.read_reservation("build-mini-1", "cmux")["state"], "absent")
        argv = run.call_args.args[0]
        self.assertEqual(argv[:8], [mf.SSH, "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-l", "cmux", "build-mini-1"])
        self.assertTrue(argv[8].startswith("/bin/bash -c "))
        self.assertTrue(argv[8].endswith(" glaeda " + mf.FLEET_ROOT))


if __name__ == "__main__":
    unittest.main()
