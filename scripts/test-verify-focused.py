#!/usr/bin/env python3
"""Deterministic tests for the fixed credentialless verification profiles."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "verify_focused_impl", ROOT / "scripts" / "verify_focused_impl.py"
)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class VerifyFocusedTests(unittest.TestCase):
    def _execute_child(self, command, **arguments):
        # Retain ownership of test children/pipes even if a discriminator fails.
        children = []
        real_popen = subprocess.Popen
        def launch(*args, **kwargs):
            child = real_popen(*args, **kwargs)
            children.append(child)
            return child
        try:
            with mock.patch.object(MODULE.owned_task.subprocess, "Popen", side_effect=launch):
                result = MODULE.owned_task.execute(command, **arguments)
            self.assertTrue(all(child.poll() is not None for child in children))
            return result
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
                    child.wait()
                child.stdout.close()

    def test_extraction_preserves_baseline_profile_and_command_bytes(self) -> None:
        # Captured from cc4674e before extraction, with no public cache mounts.
        expected = [
            (MODULE.FOCUSED_PROFILE,
             "sha256:5c4664ac2da3dcc66826d111a5f82e5614b9589dbbbdcf4383b6a88cd38a1195",
             "sha256:f65d46e0cb520c284f08452a028b7971f3340c9196a91511f1683359a6c2d2c3"),
            (MODULE.REQUIRED_PROFILE,
             "sha256:71c2064e99793aaf39b2b38ce4b4f3e8b570c75f0c854ec01ce9993d73ea717e",
             "sha256:1f66b4a0089d55881c7cef592436af5bfa73915c7bc35f3b5432142c2f00b26f"),
        ]
        with mock.patch.object(MODULE, "public_crates_io_cache_arguments", return_value=[]):
            for profile, generation, command_digest in expected:
                with self.subTest(profile=profile.profile_id):
                    command = MODULE.sandbox_command(
                        Path("/source"), Path("/task"), Path("/cargo"), Path("/rustup"),
                        "glaeda-parity.service", profile,
                    )
                    self.assertEqual(MODULE.profile_generation(profile), generation)
                    self.assertEqual(MODULE.sha256(MODULE.canonical_bytes(command)), command_digest)

    def test_internal_network_class_cannot_be_widened_by_a_string(self) -> None:
        with self.assertRaisesRegex(MODULE.Refusal, "network class"):
            MODULE.owned_task.sandbox_command(
                Path("/source"), Path("/cargo"), Path("/rustup"), "test.service",
                systemd_properties=[], build_tmpfs_bytes=1,
                mount_arguments=[], recipe_arguments=[], network="none",
            )

    def test_extracted_capture_hashes_real_child_output(self) -> None:
        task = MODULE.owned_task
        payload = b"owned task output\n"
        with (mock.patch.object(task, "unit_absent", return_value=True),
              mock.patch.object(task, "stop_unit") as stop):
            result = self._execute_child(
                [sys.executable, "-c", "print('owned task output')"],
                unit="fixture.service", deadline_seconds=10, label="fixture",
            )
        self.assertEqual(result[0:2], ("succeeded", 0))
        self.assertTrue(result[3])
        self.assertEqual(result[4:], (len(payload), MODULE.sha256(payload)))
        stop.assert_not_called()

    def test_extracted_overflow_stops_unit_and_retains_output_digest(self) -> None:
        task = MODULE.owned_task
        with (mock.patch.object(task, "MAX_SOURCE_OUTPUT_BYTES", 32),
              mock.patch.object(task, "unit_absent", return_value=True),
              mock.patch.object(task, "stop_unit") as stop,
              mock.patch.object(task.sys, "stderr") as stderr):
            result = self._execute_child(
                [sys.executable, "-c", "import sys; sys.stdout.write('x' * 64)"],
                unit="fixture.service", deadline_seconds=10, label="fixture",
            )
        self.assertEqual(result[0], "failed")
        self.assertEqual(result[4:], (64, MODULE.sha256(b"x" * 64)))
        stop.assert_called_once_with("fixture.service")
        self.assertEqual(stderr.buffer.write.call_args_list, [mock.call(b"x" * 64), mock.call(b"\n")])

    def test_extracted_timeout_settles_owned_child_group(self) -> None:
        task = MODULE.owned_task
        # Advance the controller clock past its deadline. The actual subprocess
        # is a disposable test-owned child; systemd itself is replaced here.
        with (mock.patch.object(task.time, "monotonic", side_effect=[0, 31, 32]),
              mock.patch.object(task, "unit_absent", return_value=True),
              mock.patch.object(task, "stop_unit") as stop):
            result = self._execute_child(
                [sys.executable, "-c", "import time; time.sleep(10)"],
                unit="fixture.service", deadline_seconds=0, label="fixture",
            )
        self.assertEqual(result[0], "timed_out")
        self.assertNotEqual(result[1], 0)
        self.assertTrue(result[3])
        stop.assert_called_once_with("fixture.service")

    def test_extracted_unknown_unit_state_is_cleanup_incomplete(self) -> None:
        task = MODULE.owned_task
        with (mock.patch.object(task, "unit_absent", return_value=False),
              mock.patch.object(task, "stop_unit") as stop):
            result = self._execute_child(
                [sys.executable, "-c", "pass"],
                unit="fixture.service", deadline_seconds=10, label="fixture",
            )
        self.assertEqual(result[0], "cleanup_incomplete")
        self.assertFalse(result[3])
        stop.assert_called_once_with("fixture.service")

    def test_profile_is_fixed_compact_and_credentialless(self) -> None:
        completed = subprocess.run(
            [sys.executable, ROOT / "scripts" / "verify-focused", "profile"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        profile = json.loads(completed.stdout)
        self.assertEqual(profile["profile_id"], "verify-focused/v1")
        self.assertEqual(profile["execution_identity_class"], "credentialless_project")
        self.assertEqual(profile["recipe"], ["scripts/verify", "focused"])
        self.assertEqual(profile["source_network"], "none")
        self.assertEqual(profile["profile_generation"], MODULE.profile_generation())
        self.assertLess(len(completed.stdout), 2_000)

    def test_required_profile_is_fixed_and_uses_repository_required_recipe(self) -> None:
        completed = subprocess.run(
            [sys.executable, ROOT / "scripts" / "verify-required", "profile"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        profile = json.loads(completed.stdout)
        self.assertEqual(profile["profile_id"], "verify-required/v1")
        self.assertEqual(profile["profile_class"], "verify_required")
        self.assertEqual(profile["resource_class"], "big-red-required")
        self.assertEqual(profile["deadline_seconds"], 1200)
        self.assertEqual(profile["build_tmpfs_bytes"], 8 * 1024 * 1024 * 1024)
        self.assertEqual(profile["recipe"], ["scripts/verify", "required"])
        self.assertIn("MemoryHigh=10G", profile["systemd_properties"])
        self.assertIn("MemoryMax=12G", profile["systemd_properties"])
        self.assertEqual(
            profile["profile_generation"],
            MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
        )
        self.assertLess(len(completed.stdout), 2_000)

    def test_reviewed_admission_demand_maps_existing_profiles_only(self) -> None:
        focused = MODULE.admission_demand(MODULE.FOCUSED_PROFILE)
        required = MODULE.admission_demand(MODULE.REQUIRED_PROFILE)
        self.assertEqual(focused.memory_bytes, 8 * 1024**3)
        self.assertEqual(focused.minimum_logical_cpus, 8)
        self.assertEqual(required.memory_bytes, 12 * 1024**3)
        self.assertEqual(required.minimum_logical_cpus, 8)
        unknown = MODULE.fixed_profile(
            "verify-unknown/v1", "unknown", "unknown", 60, "focused", "unknown",
            MODULE.TARGET_TMPFS_BYTES, "1G", "1G",
        )
        with self.assertRaisesRegex(MODULE.Refusal, "no reviewed local admission demand"):
            MODULE.admission_demand(unknown)

    def test_required_run_supplies_required_demand_to_reservation_and_recovery(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(strict=True)
            os.chmod(root, 0o700)
            for name in ("repository", "cargo", "rustup", "state"):
                (root / name).mkdir(mode=0o700)
            arguments = mock.Mock(
                admission_root=str(root / "admission"),
                repository_root=str(root / "repository"),
                state_root=str(root / "state"),
                cargo_root=str(root / "cargo"),
                rustup_root=str(root / "rustup"),
                repository="teamleaderleo/glaeda",
                commit="a" * 40,
                tree="b" * 40,
                profile_generation=MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
                command_fingerprint="sha256:" + "d" * 64,
                reconcile_only=False,
            )
            admission = mock.Mock(owned=False, launch_attempted=True)
            gate = mock.MagicMock()
            gate.__enter__.return_value = admission
            gate.__exit__.return_value = False
            with (
                mock.patch.object(MODULE, "verify_resident_source"),
                mock.patch.object(MODULE.owned_task, "prepare_task"),
                mock.patch.object(MODULE, "materialize", return_value=root / "source"),
                mock.patch.object(
                    MODULE,
                    "execute_profile",
                    return_value=("succeeded", 0, 1.0, True, 0, MODULE.sha256(b"")),
                ),
                mock.patch.object(MODULE.owned_admission, "Reservation", return_value=gate) as reserve,
                mock.patch.object(MODULE, "emit"),
            ):
                self.assertEqual(MODULE.run(arguments, MODULE.REQUIRED_PROFILE), 0)
            self.assertEqual(reserve.call_count, 1)
            self.assertEqual(reserve.call_args.args[4], MODULE.REQUIRED_ADMISSION_DEMAND)
            admission.release.assert_called_once_with()

            arguments.reconcile_only = True
            with (
                mock.patch.object(MODULE, "verify_resident_source"),
                mock.patch.object(MODULE, "unit_absent", return_value=True),
                mock.patch.object(MODULE, "emit"),
                mock.patch.object(
                    MODULE.owned_admission,
                    "recover",
                    side_effect=lambda *args: args[4](),
                ) as recover,
            ):
                self.assertEqual(MODULE.run(arguments, MODULE.REQUIRED_PROFILE), 0)
            recover.assert_called_once()
            self.assertEqual(recover.call_args.args[5], MODULE.REQUIRED_ADMISSION_DEMAND)

    def test_cli_has_no_remote_command_environment_or_url(self) -> None:
        help_text = subprocess.run(
            [sys.executable, ROOT / "scripts" / "verify-focused", "run", "--help"],
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        ).stdout
        for forbidden in ("--shell", "--argv", "--environment", "--remote-url", "--executable"):
            self.assertNotIn(forbidden, help_text)
        self.assertIn("--repository", help_text)
        self.assertIn("--commit", help_text)
        self.assertIn("--tree", help_text)

    def test_semantic_request_binding_replays_exactly_and_rejects_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            state_root = MODULE.private_state_directory(
                os.fspath(Path(temporary) / "state")
            )
            first = MODULE.Request(
                "teamleaderleo/glaeda",
                "1" * 40,
                "2" * 40,
                MODULE.profile_generation(),
                "sha256:" + "a" * 64,
            )
            MODULE.bind_semantic_request(
                state_root,
                "accepted-request-0001",
                first,
            )
            MODULE.bind_semantic_request(
                state_root,
                "accepted-request-0001",
                first,
            )
            binding = (
                state_root
                / "semantic-requests"
                / "accepted-request-0001"
                / "binding.json"
            )
            self.assertTrue(binding.is_file())

            drifted = MODULE.Request(
                "teamleaderleo/glaeda",
                "3" * 40,
                "4" * 40,
                MODULE.profile_generation(),
                "sha256:" + "b" * 64,
            )
            with self.assertRaises(MODULE.SemanticRequestConflict):
                MODULE.bind_semantic_request(
                    state_root,
                    "accepted-request-0001",
                    drifted,
                )

    def test_semantic_request_binding_rejects_unbounded_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            state_root = MODULE.private_state_directory(
                os.fspath(Path(temporary) / "state")
            )
            request = MODULE.Request(
                "teamleaderleo/glaeda",
                "1" * 40,
                "2" * 40,
                MODULE.profile_generation(),
                "sha256:" + "a" * 64,
            )
            with self.assertRaisesRegex(MODULE.Refusal, "identity"):
                MODULE.bind_semantic_request(state_root, "../escape", request)

    def test_sandbox_clears_authority_and_owns_recipe(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("source", "cargo", "rustup"):
                (root / name).mkdir()
            (root / "cargo" / "bin").mkdir()
            for kind in ("cache", "index", "src"):
                cache = root / "cargo" / "registry" / kind / "index.crates.io-public"
                cache.mkdir(parents=True)
            unrelated = root / "cargo" / "git" / "checkouts" / "unrelated-project"
            unrelated.mkdir(parents=True)
            command = MODULE.sandbox_command(
                root / "source",
                root,
                root / "cargo",
                root / "rustup",
                "exact.service",
            )
        joined = "\0".join(command)
        self.assertIn("--unshare-all", command)
        self.assertIn("--disable-userns", command)
        self.assertIn("--clearenv", command)
        self.assertIn(str(MODULE.TARGET_TMPFS_BYTES), command)
        self.assertIn("--property=NoNewPrivileges=yes", command)
        self.assertIn("--property=RestrictSUIDSGID=yes", command)
        self.assertIn("--pipe", command)
        self.assertNotIn("--property=StandardOutput=journal", command)
        self.assertIn("/workspace/source/scripts/verify\0focused", joined)
        cache_target = "/cargo-home/registry/cache/index.crates.io-public"
        cache_index = command.index(cache_target)
        self.assertEqual(command[cache_index - 2], "--ro-bind")
        self.assertNotIn("/cargo-home/git", joined)
        self.assertNotIn("unrelated-project", joined)
        self.assertNotIn("SSH_AUTH_SOCK", joined)
        self.assertNotIn("GITHUB_TOKEN", joined)
        for forbidden_host_state in (
            "/etc/shadow",
            "/etc/gshadow",
            "/etc/ssh",
            "/run",
            "/var/run",
        ):
            self.assertNotIn(forbidden_host_state, command)
        self.assertEqual(
            [
                argument.removeprefix("--property=")
                for argument in command
                if argument.startswith("--property=")
            ],
            MODULE.PROFILE_SPEC["systemd_properties"],
        )

    def test_required_sandbox_selects_only_the_named_required_recipe(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("source", "cargo", "rustup"):
                (root / name).mkdir()
            (root / "cargo" / "bin").mkdir()
            command = MODULE.sandbox_command(
                root / "source",
                root,
                root / "cargo",
                root / "rustup",
                "exact.service",
                MODULE.REQUIRED_PROFILE,
            )
        joined = "\0".join(command)
        self.assertIn("/workspace/source/scripts/verify\0required", joined)
        self.assertIn("--tmpfs\0/workspace/source/target", joined)
        self.assertIn("CARGO_TARGET_DIR\0/workspace/source/target", joined)
        self.assertNotIn("/workspace/target", command)
        self.assertNotIn("/workspace/source/scripts/verify\0focused", joined)
        self.assertIn("--property=RuntimeMaxSec=1200", command)
        self.assertIn("--property=MemoryHigh=10G", command)
        self.assertIn("--property=MemoryMax=12G", command)
        self.assertIn(str(MODULE.REQUIRED_TARGET_TMPFS_BYTES), command)
        for safe_host_fact in (
            "/etc/os-release",
            "/etc/nsswitch.conf",
            "/etc/passwd",
            "/etc/group",
            "/etc/subuid",
            "/etc/subgid",
            "/etc/containers",
            "/var/lib/dpkg",
        ):
            self.assertIn(safe_host_fact, command)

    def test_focused_sandbox_does_not_receive_required_host_plan_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("source", "cargo", "rustup"):
                (root / name).mkdir()
            (root / "cargo" / "bin").mkdir()
            command = MODULE.sandbox_command(
                root / "source",
                root,
                root / "cargo",
                root / "rustup",
                "exact.service",
            )
        for required_only_fact in ("/etc/os-release", "/etc/passwd", "/var/lib/dpkg"):
            self.assertNotIn(required_only_fact, command)

    def test_materialized_source_prepares_only_the_ignored_target_mountpoint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repository = root / "repository"
            repository.mkdir()
            subprocess.run(["git", "init", "--quiet"], cwd=repository, check=True)
            subprocess.run(
                ["git", "config", "user.email", "verify@example.invalid"],
                cwd=repository,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "Verify Fixture"],
                cwd=repository,
                check=True,
            )
            (repository / ".gitignore").write_text("/target/\n", encoding="utf-8")
            (repository / "tracked").write_text("exact\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=repository, check=True)
            subprocess.run(["git", "commit", "--quiet", "-m", "fixture"], cwd=repository, check=True)
            commit, tree = subprocess.run(
                ["git", "rev-parse", "HEAD", "HEAD^{tree}"],
                cwd=repository,
                check=True,
                text=True,
                stdout=subprocess.PIPE,
            ).stdout.splitlines()
            request = MODULE.Request(
                repository="teamleaderleo/glaeda",
                commit=commit,
                tree=tree,
                profile_generation=MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
                command_fingerprint="sha256:" + "a" * 64,
                profile=MODULE.REQUIRED_PROFILE,
            )
            task = root / "task"
            task.mkdir()
            source = MODULE.materialize(repository, task, request)
            self.assertTrue((source / "target").is_dir())
            self.assertEqual(
                subprocess.run(
                    ["git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
                    cwd=source,
                    check=True,
                    stdout=subprocess.PIPE,
                ).stdout,
                b"",
            )
    def test_required_receipt_has_distinct_profile_identity(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
            "sha256:" + "e" * 64,
            MODULE.REQUIRED_PROFILE,
        )
        document = MODULE.receipt(
            request, "succeeded", 0, 1.0, True, True, 0, MODULE.sha256(b""), 1, 2
        )
        self.assertEqual(document["document_type"], "glaeda-verify-required-receipt")
        self.assertEqual(document["profile"]["id"], "verify-required/v1")
        self.assertTrue(MODULE.valid_terminal_receipt(document, request))

    def test_exact_receipt_replays_without_execution(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(),
            "sha256:" + "c" * 64,
        )
        document = MODULE.receipt(
            request,
            "succeeded",
            0,
            1.0,
            True,
            True,
            0,
            MODULE.sha256(b""),
            1,
            2,
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(strict=True)
            os.chmod(root, 0o700)
            for name in ("repository", "cargo", "rustup", "state"):
                (root / name).mkdir(mode=0o700)
            command_root = root / "state" / ("c" * 64)
            command_root.mkdir(mode=0o700)
            path = command_root / "receipt.json"
            MODULE.publish_document(path, document, replace=False)
            self.assertEqual(MODULE.read_document(path), document)
            self.assertTrue(MODULE.matches_request(document, request))
            self.assertTrue(MODULE.valid_terminal_receipt(document, request))

            original_bytes = path.read_bytes()
            original_mtime = path.stat().st_mtime_ns
            for reconcile_only in (False, True):
                arguments = mock.Mock(
                    semantic_request_id=None,
                    admission_root=None,
                    repository_root=str(root / "repository"), state_root=str(root / "state"),
                    cargo_root=str(root / "cargo"), rustup_root=str(root / "rustup"),
                    repository=request.repository, commit=request.commit, tree=request.tree,
                    profile_generation=request.profile_generation,
                    command_fingerprint=request.command_fingerprint, reconcile_only=reconcile_only,
                )
                with (mock.patch.object(MODULE, "verify_resident_source"),
                      mock.patch.object(MODULE.owned_task, "prepare_task") as prepare,
                      mock.patch.object(MODULE, "materialize") as materialize,
                      mock.patch.object(MODULE, "execute_profile") as execute,
                      mock.patch.object(MODULE, "emit") as emit):
                    self.assertEqual(MODULE.run(arguments), 0)
                    emit.assert_called_once_with(document)
                    prepare.assert_not_called()
                    materialize.assert_not_called()
                    execute.assert_not_called()
                self.assertEqual(path.read_bytes(), original_bytes)
                self.assertEqual(path.stat().st_mtime_ns, original_mtime)

    def test_receipt_replay_rejects_incomplete_security_claims(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(),
            "sha256:" + "c" * 64,
        )
        original = MODULE.receipt(
            request,
            "succeeded",
            0,
            1.0,
            True,
            True,
            1,
            MODULE.sha256(b"x"),
            1,
            2,
        )
        mutations = (
            ("result", "task_cleanup_complete", False),
            ("result", "output_sha256", "not-a-digest"),
            ("result", "resource_accounting", "none"),
            ("isolation", "filesystem", "host_writable"),
            ("isolation", "ambient_environment", "inherited"),
            ("isolation", "unrelated_writable_projects", "present"),
            ("isolation", "build_state", "host_shared"),
            ("isolation", "package_cache", "host_private_writable"),
        )
        for section, key, value in mutations:
            with self.subTest(section=section, key=key):
                document = json.loads(json.dumps(original))
                document[section][key] = value
                self.assertFalse(MODULE.valid_terminal_receipt(document, request))

        document = json.loads(json.dumps(original))
        document["unexpected"] = True
        self.assertFalse(MODULE.valid_terminal_receipt(document, request))

    def test_required_replay_rejects_impossible_terminal_evidence(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
            "sha256:" + "f" * 64,
            MODULE.REQUIRED_PROFILE,
        )
        original = MODULE.receipt(
            request,
            "succeeded",
            0,
            1.0,
            True,
            True,
            0,
            MODULE.sha256(b""),
            1,
            2,
        )
        for key, value in (
            ("exit_code", 9),
            ("elapsed_seconds", float("inf")),
            ("elapsed_seconds", float("nan")),
        ):
            with self.subTest(key=key, value=value):
                document = json.loads(json.dumps(original))
                document["result"][key] = value
                self.assertFalse(MODULE.valid_terminal_receipt(document, request))

    def test_non_finite_json_state_is_corrupt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve(strict=True) / "receipt.json"
            path.write_bytes(b'{"elapsed_seconds":Infinity}\n')
            os.chmod(path, 0o600)
            with self.assertRaisesRegex(MODULE.Refusal, "state is corrupt"):
                MODULE.read_document(path)

    def test_reconcile_without_receipt_never_executes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(strict=True)
            os.chmod(root, 0o700)
            for name in ("repository", "cargo", "rustup"):
                (root / name).mkdir()
            arguments = mock.Mock(
                semantic_request_id=None,
                admission_root=None,
                repository_root=str(root / "repository"),
                state_root=str(root / "state"),
                cargo_root=str(root / "cargo"),
                rustup_root=str(root / "rustup"),
                repository="teamleaderleo/glaeda",
                commit="a" * 40,
                tree="b" * 40,
                profile_generation=MODULE.profile_generation(),
                command_fingerprint="sha256:" + "c" * 64,
                reconcile_only=True,
            )
            with (
                mock.patch.object(MODULE, "verify_resident_source"),
                mock.patch.object(MODULE, "materialize") as materialize,
                mock.patch.object(MODULE, "execute_profile") as execute,
            ):
                with self.assertRaisesRegex(MODULE.Refusal, "no terminal receipt"):
                    MODULE.run(arguments)
                materialize.assert_not_called()
                execute.assert_not_called()

    def test_failed_unit_observation_is_not_settled(self) -> None:
        failed = subprocess.CompletedProcess([], 1, stdout=b"", stderr=b"")
        absent = subprocess.CompletedProcess([], 0, stdout=b"not-found\n", stderr=b"")
        with mock.patch.object(MODULE.subprocess, "run", return_value=failed):
            self.assertFalse(MODULE.unit_absent("exact.service"))
        with mock.patch.object(MODULE.subprocess, "run", return_value=absent):
            self.assertTrue(MODULE.unit_absent("exact.service"))

    def test_cleanup_oserror_publishes_terminal_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(strict=True)
            os.chmod(root, 0o700)
            for name in ("repository", "cargo", "rustup"):
                (root / name).mkdir()
            arguments = mock.Mock(
                semantic_request_id=None,
                admission_root=None,
                repository_root=str(root / "repository"),
                state_root=str(root / "state"),
                cargo_root=str(root / "cargo"),
                rustup_root=str(root / "rustup"),
                repository="teamleaderleo/glaeda",
                commit="a" * 40,
                tree="b" * 40,
                profile_generation=MODULE.profile_generation(),
                command_fingerprint="sha256:" + "d" * 64,
                reconcile_only=False,
            )
            with (
                mock.patch.object(MODULE, "verify_resident_source"),
                mock.patch.object(MODULE, "materialize", return_value=root / "source"),
                mock.patch.object(
                    MODULE,
                    "execute_profile",
                    return_value=("succeeded", 0, 1.0, True, 0, MODULE.sha256(b"")),
                ),
                mock.patch.object(MODULE, "remove_task", side_effect=OSError("busy")),
                mock.patch.object(MODULE, "emit"),
            ):
                self.assertEqual(MODULE.run(arguments), 0)
            receipt = MODULE.read_document(
                root / "state" / ("d" * 64) / "receipt.json"
            )
            self.assertEqual(receipt["result"]["terminal_class"], "cleanup_incomplete")
            self.assertFalse(receipt["result"]["task_cleanup_complete"])


if __name__ == "__main__":
    unittest.main()
