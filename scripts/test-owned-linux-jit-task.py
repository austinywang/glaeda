#!/usr/bin/env python3
"""Owned-Linux GitHub JIT task boundary tests; no network/systemd/bwrap side effects."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import stat
import tempfile
import time
import unittest
from unittest import mock

import owned_linux_admission as admission
import owned_linux_jit_task as jit
import owned_linux_task as task


class OwnedLinuxJitTaskTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.admission = self.root / "admission"
        self.admission.mkdir(mode=0o700)
        self.tasks = self.root / "tasks"
        self.tasks.mkdir(mode=0o700)
        self.unrelated = self.root / "unrelated"
        self.unrelated.mkdir(mode=0o700)
        self.payload = self.root / "payload"
        (self.payload / "bin").mkdir(parents=True)
        for name in ("_work", "_diag", "target"):
            (self.payload / name).mkdir()
        listener = self.payload / "bin" / "Runner.Listener"
        listener.write_bytes(b"fixed-runner-listener\n")
        listener.chmod(0o555)
        for path in (self.payload, self.payload / "bin", self.payload / "_work",
                     self.payload / "_diag", self.payload / "target"):
            path.chmod(0o555)
        self.launcher = Path(__file__).with_name("owned_linux_jit_launcher").resolve()
        self.egress_guard = self.root / "egress-authority.json"
        self.egress_guard.write_bytes(jit.canonical({
            "class": jit.NETWORK.value,
            "document_type": jit.EGRESS_GUARD_DOCUMENT_TYPE,
            "enforcement": jit.EGRESS_GUARD_ENFORCEMENT,
            "private_or_link_local_egress": False,
            "schema_version": jit.SCHEMA_VERSION,
        }))
        self.egress_guard.chmod(0o400)
        self.egress_guard_digest = "sha256:" + "7" * 64
        self.egress_patch = mock.patch.object(jit, "_verify_egress_guard", return_value=None)
        self.egress_patch.start()
        self.addCleanup(self.egress_patch.stop)
        self.unit = "glaeda-gha-" + "a" * 32 + ".service"
        self.task_root = self.tasks / ("a" * 32)
        self.fingerprint = "sha256:" + "b" * 64
        self.binding = "sha256:" + "c" * 64
        self.available_memory = 24 * 1024**3
        self.logical_cpus = 16
        self.policy = {
            "schema_version": 1,
            "generation": "d" * 64,
            "revision": 1,
            "node_control": "available",
            "memory_reserve_bytes": 4 * 1024**3,
            "host_executable": {"path": "/host", "sha256": "sha256:" + "e" * 64},
            "policy_executable": {"path": "/policy", "sha256": "sha256:" + "f" * 64},
        }
        (self.admission / "policy.json").write_bytes(admission.canonical(self.policy))
        (self.admission / "policy.json").chmod(0o600)
        self.query_patch = mock.patch.object(admission, "query", side_effect=self.query)
        self.query_patch.start()
        self.addCleanup(self.query_patch.stop)
        self.payload_digest = jit.payload_tree_digest(self.payload)

    def query(self, entry, arguments, raw=b""):
        if entry["path"] == "/host":
            return {
                "document_type": "glaeda-linux-host-observation",
                "schema_version": 3,
                "authority": "observation_only",
                "scope": "current_execution_context",
                "observed_at_unix_millis": time.time_ns() // 1_000_000,
                "memory": {"available_bytes": self.available_memory},
                "cpu": {"logical_cpus": self.logical_cpus},
                "pressure": {kind: {"avg10_micros": 0} for kind in ("cpu", "memory", "io")},
            }
        observation = json.loads(raw)["observation"]
        reason = {"held": "node_held", "draining": "node_draining"}.get(
            observation["node_control"]
        )
        reason = reason or "compatible"
        return {
            "document_type": "glaeda-local-admission-decision",
            "schema_version": 1,
            "input_sha256": admission.digest(raw),
            "grants_authority": False,
            "authorizes_execution": False,
            "decision": {
                "disposition": {"node_held": "refuse"}.get(reason, "wait" if reason != "compatible" else "admit_now"),
                "reason": reason,
                "active_quiet_lease_generation": None,
                "requires_new_quiet_lease": False,
                "requires_yieldable_drain": False,
                "grants_authority": False,
                "authorizes_preemption": False,
                "authorizes_execution": False,
            },
        }

    def args(self, command):
        return argparse.Namespace(
            command=command,
            admission_root=str(self.admission),
            task_root=str(self.task_root),
            payload_root=str(self.payload),
            payload_tree_sha256=self.payload_digest,
            launcher=str(self.launcher),
            egress_guard=str(self.egress_guard),
            egress_guard_sha256=self.egress_guard_digest,
            command_fingerprint=self.fingerprint,
            unit=self.unit,
            binding_sha256=self.binding,
            deadline_seconds=300 if command == "launch" else None,
        )

    def prepare(self):
        emitted = []
        with mock.patch.object(jit, "emit", side_effect=emitted.append):
            self.assertEqual(jit.prepare(self.args("prepare")), 0)
        self.assertEqual(emitted[0]["reservation_phase"], "preparing")
        return emitted[0]

    def fake_execute(self, command, **kwargs):
        self.assertTrue(kwargs["inherit_stdin"])
        self.assertFalse(kwargs["emit_failure_tail"])
        self.assertEqual(kwargs["unit"], self.unit)
        with kwargs["launch_guard"]():
            pass
        joined = "\0".join(command)
        self.assertIn("--ro-bind\0" + str(self.payload) + "\0/runner", joined)
        self.assertIn("--bind\0" + str(self.task_root / "work") + "\0/runner/_work", joined)
        self.assertIn("--bind\0" + str(self.task_root / "diag") + "\0/runner/_diag", joined)
        self.assertIn("--bind\0" + str(self.task_root / "home") + "\0/home/runner", joined)
        self.assertIn("github", jit.NETWORK.value)
        self.assertIn("--unshare-pid", command)
        self.assertIn("--unshare-ipc", command)
        self.assertIn("--unshare-uts", command)
        self.assertIn("--unshare-cgroup", command)
        self.assertNotIn("--unshare-net", command)
        self.assertNotIn("--unshare-all", command)
        self.assertNotIn(str(self.admission), joined)
        self.assertNotIn(str(self.unrelated), joined)
        self.assertNotIn("/run/docker.sock", joined)
        self.assertNotIn("/run/podman", joined)
        self.assertIn("/usr/bin/env", command)
        env_index = command.index("/usr/bin/env")
        self.assertEqual(command[env_index + 1], "-i")
        return ("succeeded", 0, 1.25, True, 731, "sha256:" + "1" * 64)

    def test_egress_authority_document_is_closed_and_canonical(self):
        expected = {
            "class": jit.NETWORK.value,
            "document_type": jit.EGRESS_GUARD_DOCUMENT_TYPE,
            "enforcement": jit.EGRESS_GUARD_ENFORCEMENT,
            "private_or_link_local_egress": False,
            "schema_version": jit.SCHEMA_VERSION,
        }
        raw = jit.canonical(expected)
        self.assertEqual(jit._validate_egress_guard_document(raw), expected)
        for changed in (
            {**expected, "private_or_link_local_egress": True},
            {**expected, "enforcement": "controller_assertion_only"},
            {**expected, "class": "host_network"},
            {**expected, "extra": True},
        ):
            with self.subTest(changed=changed):
                with self.assertRaises(task.Refusal):
                    jit._validate_egress_guard_document(jit.canonical(changed))
        with self.assertRaises(task.Refusal):
            jit._validate_egress_guard_document(json.dumps(expected).encode() + b"\n")

    def test_egress_authority_requires_root_owned_exact_file(self):
        self.egress_patch.stop()
        raw = jit.canonical({
            "class": jit.NETWORK.value,
            "document_type": jit.EGRESS_GUARD_DOCUMENT_TYPE,
            "enforcement": jit.EGRESS_GUARD_ENFORCEMENT,
            "private_or_link_local_egress": False,
            "schema_version": jit.SCHEMA_VERSION,
        })
        with mock.patch.object(
            jit,
            "_read_stable_regular",
            return_value=raw,
        ) as read:
            jit._verify_egress_guard(self.egress_guard, jit._digest(raw))
        self.assertEqual(read.call_args.kwargs["allowed_uids"], {0})
        self.assertEqual(read.call_args.kwargs["exact_modes"], {0o400, 0o444})
        self.assertTrue(read.call_args.kwargs["require_nonempty"])

        fields = dict(
            st_mode=stat.S_IFREG | 0o444,
            st_uid=1000,
            st_gid=1000,
            st_nlink=1,
            st_size=len(raw),
            st_dev=1,
            st_ino=2,
            st_mtime_ns=3,
            st_ctime_ns=4,
        )
        foreign_info = type("Info", (), fields)()
        with mock.patch.object(jit.os, "fstat", return_value=foreign_info):
            with self.assertRaisesRegex(task.Refusal, "unsafe"):
                jit._read_stable_regular(
                    self.egress_guard,
                    max_bytes=jit.MAX_EGRESS_GUARD_BYTES,
                    allowed_uids={0},
                    exact_modes={0o400, 0o444},
                    require_nonempty=True,
                    problem="owned runner egress authority is unsafe",
                )

    def test_stable_regular_read_refuses_symlink(self):
        target = self.root / "stable-target"
        target.write_bytes(b"stable\n")
        target.chmod(0o600)
        alias = self.root / "stable-alias"
        alias.symlink_to(target)
        with self.assertRaisesRegex(task.Refusal, "unsafe"):
            jit._read_stable_regular(
                alias,
                max_bytes=64,
                allowed_uids={os.getuid()},
                exact_modes={0o600},
                require_nonempty=True,
                problem="stable file is unsafe",
            )

    def test_stable_regular_read_detects_path_replacement_during_read(self):
        path = self.root / "stable-file"
        replacement = self.root / "replacement-file"
        path.write_bytes(b"original\n")
        replacement.write_bytes(b"replacement\n")
        path.chmod(0o600)
        replacement.chmod(0o600)
        real_fstat = jit.os.fstat
        calls = 0

        def swapping_fstat(descriptor):
            nonlocal calls
            calls += 1
            observed = real_fstat(descriptor)
            if calls == 1:
                os.replace(replacement, path)
            return observed

        with mock.patch.object(jit.os, "fstat", side_effect=swapping_fstat):
            with self.assertRaisesRegex(task.Refusal, "unsafe"):
                jit._read_stable_regular(
                    path,
                    max_bytes=64,
                    allowed_uids={os.getuid()},
                    exact_modes={0o600},
                    require_nonempty=True,
                    problem="stable file is unsafe",
                )

    def test_egress_revocation_blocks_launch_but_cleanup_remains_available(self):
        self.prepare()
        with (
            mock.patch.object(
                jit,
                "_verify_egress_guard",
                side_effect=task.Refusal("egress authority revoked"),
            ),
            mock.patch.object(task, "execute") as execute,
        ):
            with self.assertRaisesRegex(task.Refusal, "revoked"):
                jit.launch(self.args("launch"))
            execute.assert_not_called()
            with (
                mock.patch.object(task, "unit_absent", return_value=True),
                mock.patch.object(jit, "emit"),
            ):
                self.assertEqual(jit.cleanup(self.args("cleanup")), 0)
        self.assertFalse(self.task_root.exists())
        self.assertFalse((self.admission / "reservation.json").exists())

    def test_prepare_is_private_and_exact_restart_is_idempotent(self):
        receipt = self.prepare()
        self.assertRegex(receipt["task_identity_sha256"], r"^sha256:[0-9a-f]{64}$")
        self.assertEqual(receipt["profile"], jit.PROFILE)
        self.assertEqual(receipt["network"], "github_actions_trusted_egress")
        self.assertEqual((self.task_root / "task.json").stat().st_mode & 0o777, 0o600)
        for name in ("work", "diag", "home"):
            self.assertEqual((self.task_root / name).stat().st_mode & 0o777, 0o700)
        replay = []
        with mock.patch.object(jit, "emit", side_effect=replay.append):
            self.assertEqual(jit.prepare(self.args("prepare")), 0)
        self.assertEqual(replay[0]["task_identity_sha256"], receipt["task_identity_sha256"])

    def test_different_assignment_cannot_reuse_preparing_reservation(self):
        self.prepare()
        for field, value in (
            ("binding_sha256", "sha256:" + "9" * 64),
            ("command_fingerprint", "sha256:" + "8" * 64),
        ):
            arguments = self.args("prepare")
            setattr(arguments, field, value)
            with self.subTest(field=field):
                with self.assertRaises(task.Refusal):
                    jit.prepare(arguments)
        self.assertTrue((self.admission / "reservation.json").exists())
        self.assertTrue(self.task_root.exists())

    def test_direct_local_reservation_blocks_jit_prepare(self):
        direct = admission.Reservation(
            self.admission,
            "sha256:" + "6" * 64,
            "glaeda-verify-" + "6" * 32 + ".service",
            "sha256:" + "5" * 64,
            admission.VERIFY_FOCUSED_DEMAND,
        )
        with direct:
            with self.assertRaisesRegex(task.Refusal, "busy|recovery"):
                jit.prepare(self.args("prepare"))
            self.assertFalse(self.task_root.exists())
            reservation = json.loads(
                (self.admission / "reservation.json").read_bytes()
            )
            self.assertEqual(
                reservation["command_fingerprint"],
                "sha256:" + "6" * 64,
            )

    def test_controller_restart_repairs_bounded_partial_preparation(self):
        arguments = self.args("prepare")
        reservation = admission.Reservation(
            arguments.admission_root,
            arguments.command_fingerprint,
            arguments.unit,
            arguments.binding_sha256,
            admission.VERIFY_FOCUSED_DEMAND,
        )
        with reservation:
            pass
        task.prepare_task(self.task_root)
        (self.task_root / "work").mkdir(mode=0o700)
        receipt = []
        with mock.patch.object(jit, "emit", side_effect=receipt.append):
            self.assertEqual(jit.prepare(arguments), 0)
        self.assertEqual(receipt[0]["reservation_phase"], "preparing")
        for name in ("work", "diag", "home"):
            self.assertEqual((self.task_root / name).stat().st_mode & 0o777, 0o700)
        self.assertEqual((self.task_root / "task.json").stat().st_mode & 0o777, 0o600)

    def test_unknown_partial_state_refuses_repair_and_preserves_reservation(self):
        arguments = self.args("prepare")
        reservation = admission.Reservation(
            arguments.admission_root,
            arguments.command_fingerprint,
            arguments.unit,
            arguments.binding_sha256,
            admission.VERIFY_FOCUSED_DEMAND,
        )
        with reservation:
            pass
        task.prepare_task(self.task_root)
        (self.task_root / "unexpected").write_text("foreign")
        with self.assertRaisesRegex(task.Refusal, "unknown state"):
            jit.prepare(arguments)
        self.assertTrue((self.admission / "reservation.json").exists())
        self.assertTrue((self.task_root / "unexpected").exists())

    def test_controller_restart_resumes_exact_preparation_and_keeps_capacity(self):
        self.prepare()
        with (mock.patch.object(task, "execute", side_effect=self.fake_execute),
              mock.patch.object(jit, "emit") as emit):
            self.assertEqual(jit.launch(self.args("launch")), 0)
        emit.assert_not_called()
        reservation = json.loads((self.admission / "reservation.json").read_bytes())
        self.assertEqual(reservation["phase"], "launching")
        self.assertTrue(self.task_root.exists())

    def test_observation_binds_payload_generation_and_settled_unit(self):
        prepared = self.prepare()
        emitted = []
        with (mock.patch.object(task, "unit_absent", return_value=True),
              mock.patch.object(jit, "emit", side_effect=emitted.append)):
            self.assertEqual(jit.observe(self.args("observe")), 0)
        self.assertEqual(emitted[0]["task_identity_sha256"], prepared["task_identity_sha256"])
        listener = self.payload / "bin" / "Runner.Listener"
        listener.chmod(0o755)
        listener.write_bytes(b"changed\n")
        listener.chmod(0o555)
        with self.assertRaisesRegex(task.Refusal, "generation changed"):
            jit.observe(self.args("observe"))

    def test_launch_command_exposes_only_reviewed_writable_state(self):
        self.prepare()
        with (mock.patch.object(task, "execute", side_effect=self.fake_execute),
              mock.patch.object(jit, "emit")):
            jit.launch(self.args("launch"))

    def test_jit_secret_has_no_argv_environment_receipt_or_failure_tail_channel(self):
        self.prepare()
        secret = "JITSECRET-never-emit-7E88"
        seen = {}
        def execute(command, **kwargs):
            seen["command"] = command
            seen["kwargs"] = kwargs
            with kwargs["launch_guard"]():
                pass
            return ("failed", 3, 0.1, True, len(secret), "sha256:" + "2" * 64)
        with (mock.patch.object(task, "execute", side_effect=execute),
              mock.patch.object(jit, "emit") as emit):
            self.assertEqual(jit.launch(self.args("launch")), 70)
        serialized = task.closed_environment().copy()
        self.assertNotIn(secret, "\0".join(seen["command"]))
        self.assertNotIn(secret, json.dumps(serialized))
        emit.assert_not_called()
        self.assertTrue(seen["kwargs"]["inherit_stdin"])
        self.assertFalse(seen["kwargs"]["emit_failure_tail"])

    def test_deadline_outside_named_profile_refuses_before_spawn(self):
        self.prepare()
        for value in (0, jit.MAX_DEADLINE_SECONDS + 1):
            arguments = self.args("launch")
            arguments.deadline_seconds = value
            with (self.subTest(value=value),
                  mock.patch.object(task, "execute") as execute):
                with self.assertRaisesRegex(task.Refusal, "deadline"):
                    jit.launch(arguments)
                execute.assert_not_called()

    def test_insufficient_resource_admission_refuses_before_task_creation(self):
        self.available_memory = 8 * 1024**3
        with self.assertRaisesRegex(admission.Deferred, "capacity unavailable"):
            jit.prepare(self.args("prepare"))
        self.assertFalse(self.task_root.exists())
        self.assertFalse((self.admission / "reservation.json").exists())

    def test_probe_distinguishes_exact_preparing_and_absent(self):
        self.prepare()
        emitted = []
        with mock.patch.object(jit, "emit", side_effect=emitted.append):
            self.assertEqual(jit.probe(self.args("probe")), 0)
        self.assertEqual(emitted[0]["state"], "present")
        self.assertEqual(emitted[0]["reservation_phase"], "preparing")
        with mock.patch.object(task, "unit_absent", return_value=True), mock.patch.object(jit, "emit"):
            self.assertEqual(jit.cleanup(self.args("cleanup")), 0)
        emitted = []
        with mock.patch.object(jit, "emit", side_effect=emitted.append):
            self.assertEqual(jit.probe(self.args("probe")), 0)
        self.assertEqual(emitted[0]["state"], "absent")
        self.assertIsNone(emitted[0]["reservation_phase"])

    def test_hold_and_drain_refuse_prepare_before_task_creation(self):
        for state in ("held", "draining"):
            with self.subTest(state=state):
                self.policy["node_control"] = state
                self.policy["revision"] += 1
                (self.admission / "policy.json").write_bytes(admission.canonical(self.policy))
                (self.admission / "policy.json").chmod(0o600)
                with self.assertRaises(admission.Deferred):
                    jit.prepare(self.args("prepare"))
                self.assertFalse(self.task_root.exists())
                self.assertFalse((self.admission / "reservation.json").exists())
                self.policy["node_control"] = "available"

    def test_cleanup_settles_descendants_then_removes_private_state_and_capacity(self):
        self.prepare()
        with (mock.patch.object(task, "execute", side_effect=self.fake_execute),
              mock.patch.object(jit, "emit")):
            jit.launch(self.args("launch"))
        with (mock.patch.object(task, "unit_absent", return_value=True),
              mock.patch.object(jit, "emit")):
            self.assertEqual(jit.cleanup(self.args("cleanup")), 0)
        self.assertFalse(self.task_root.exists())
        self.assertFalse((self.admission / "reservation.json").exists())

    def test_cleanup_failure_retains_capacity_for_exact_recovery(self):
        self.prepare()
        with (mock.patch.object(task, "unit_absent", return_value=True),
              mock.patch.object(task, "remove_task", side_effect=task.Refusal("cleanup failed"))):
            with self.assertRaisesRegex(task.Refusal, "cleanup failed"):
                jit.cleanup(self.args("cleanup"))
        self.assertTrue((self.admission / "reservation.json").exists())

    def test_live_process_tree_refuses_release(self):
        self.prepare()
        with (mock.patch.object(task, "unit_absent", return_value=False),
              mock.patch.object(task, "stop_unit")):
            with self.assertRaisesRegex(task.Refusal, "process tree"):
                jit.cleanup(self.args("cleanup"))
        self.assertTrue((self.admission / "reservation.json").exists())

    def test_wrong_binding_and_unit_generation_refuse_exact_resume(self):
        self.prepare()
        for field, value in (
            ("binding_sha256", "sha256:" + "9" * 64),
            ("command_fingerprint", "sha256:" + "8" * 64),
        ):
            arguments = self.args("launch")
            setattr(arguments, field, value)
            with self.subTest(field=field), mock.patch.object(task, "execute") as execute:
                with self.assertRaises(task.Refusal):
                    jit.launch(arguments)
                execute.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
