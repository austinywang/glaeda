#!/usr/bin/env python3
"""Contract tests for scripts/hot-observe."""

from __future__ import annotations

import hashlib
import json
import os
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
HOT_OBSERVE = ROOT / "scripts" / "hot-observe"
RUNTIME_DIGEST = f"sha256:{'1' * 64}"


def run_observer(arguments: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, os.fspath(HOT_OBSERVE), *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )


def all_keys(value: object) -> set[str]:
    if isinstance(value, dict):
        return set(value) | set().union(*(all_keys(item) for item in value.values()))
    if isinstance(value, list):
        return set().union(*(all_keys(item) for item in value))
    return set()


class HotObserveTests(unittest.TestCase):
    def dependency_arguments(
        self, fixture: Path, dependency_root: Path
    ) -> list[str]:
        return [
            "--output",
            "json",
            "dependency",
            "--project-id",
            "fixture-project",
            "--dependency-root",
            os.fspath(dependency_root),
            "--runtime-id",
            "fixture-runtime",
            "--runtime-sha256",
            RUNTIME_DIGEST,
            "--parent",
            f"lock={fixture / 'lock'}",
            "--parent",
            f"manifest={fixture / 'manifest'}",
            "--anchor",
            f"lock={dependency_root / 'lock'}",
            "--anchor",
            f"layout={dependency_root / 'layout'}",
        ]

    def test_dependency_observation_is_aligned_bounded_and_path_free(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            dependency_root = fixture / "dependencies"
            dependency_root.mkdir()
            (fixture / "lock").write_text("same-lock\n", encoding="utf-8")
            (fixture / "manifest").write_text("runtime=fixture\n", encoding="utf-8")
            (dependency_root / "lock").write_text("same-lock\n", encoding="utf-8")
            (dependency_root / "layout").write_text("layout=v1\n", encoding="utf-8")
            (dependency_root / "package").mkdir()
            (dependency_root / "package" / "module").write_bytes(b"content")

            result = run_observer(self.dependency_arguments(fixture, dependency_root))
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["state"], "anchor_aligned")
            self.assertEqual(report["authority"], "observation_only")
            self.assertEqual(report["semantic_limit"], "declared_anchors_only")
            self.assertTrue(report["anchor_window_stable"])
            self.assertEqual(report["comparisons"], [{"label": "lock", "matches": True}])
            self.assertEqual(report["physical"]["entry_count"], 5)
            self.assertGreater(report["physical"]["allocated_bytes"], 0)
            self.assertNotIn(os.fspath(fixture), result.stdout)
            self.assertNotIn("path", all_keys(report))

            repeated = run_observer(self.dependency_arguments(fixture, dependency_root))
            self.assertEqual(repeated.returncode, 0, repeated.stderr)
            self.assertEqual(
                json.loads(repeated.stdout)["generation_id"], report["generation_id"]
            )

            (fixture / "manifest").write_text("runtime=changed\n", encoding="utf-8")
            changed_parent = run_observer(
                self.dependency_arguments(fixture, dependency_root)
            )
            self.assertEqual(changed_parent.returncode, 0, changed_parent.stderr)
            self.assertNotEqual(
                json.loads(changed_parent.stdout)["generation_id"],
                report["generation_id"],
            )

            runtime_arguments = self.dependency_arguments(fixture, dependency_root)
            runtime_arguments[runtime_arguments.index(RUNTIME_DIGEST)] = (
                f"sha256:{'2' * 64}"
            )
            changed_runtime = run_observer(runtime_arguments)
            self.assertEqual(changed_runtime.returncode, 0, changed_runtime.stderr)
            self.assertNotEqual(
                json.loads(changed_runtime.stdout)["generation_id"],
                json.loads(changed_parent.stdout)["generation_id"],
            )

    def test_dependency_mismatch_requires_revalidation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            dependency_root = fixture / "dependencies"
            dependency_root.mkdir()
            (fixture / "lock").write_text("expected\n", encoding="utf-8")
            (fixture / "manifest").write_text("manifest\n", encoding="utf-8")
            (dependency_root / "lock").write_text("drifted\n", encoding="utf-8")
            (dependency_root / "layout").write_text("layout\n", encoding="utf-8")

            result = run_observer(self.dependency_arguments(fixture, dependency_root))
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["state"], "revalidate_required")
            self.assertEqual(report["comparisons"], [{"label": "lock", "matches": False}])

    def test_absent_dependency_is_observed_without_private_error(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            dependency_root = fixture / "absent"
            (fixture / "lock").write_text("expected\n", encoding="utf-8")
            (fixture / "manifest").write_text("manifest\n", encoding="utf-8")

            result = run_observer(self.dependency_arguments(fixture, dependency_root))
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["state"], "absent")
            self.assertIsNone(report["physical"])
            self.assertIsNone(report["generation_id"])
            self.assertFalse(report["anchors"][0]["present"])
            self.assertNotIn(os.fspath(fixture), result.stdout)

    def test_dependency_symlink_and_unsafe_identity_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            dependency_root = fixture / "dependencies"
            dependency_root.mkdir()
            (fixture / "lock").write_text("lock\n", encoding="utf-8")
            (fixture / "manifest").write_text("manifest\n", encoding="utf-8")
            (dependency_root / "real-lock").write_text("lock\n", encoding="utf-8")
            (dependency_root / "lock").symlink_to("real-lock")
            (dependency_root / "layout").write_text("layout\n", encoding="utf-8")

            arguments = self.dependency_arguments(fixture, dependency_root)
            arguments[arguments.index("fixture-project")] = "../unsafe"
            unsafe = run_observer(arguments)
            self.assertEqual(unsafe.returncode, 2)
            self.assertNotIn(os.fspath(fixture), unsafe.stderr)

            symlink = run_observer(self.dependency_arguments(fixture, dependency_root))
            self.assertEqual(symlink.returncode, 2)
            self.assertIn("regular no-follow file", symlink.stderr)
            self.assertNotIn(os.fspath(fixture), symlink.stderr)

            outside_arguments = self.dependency_arguments(fixture, dependency_root)
            outside_anchor = f"lock={dependency_root / 'lock'}"
            outside_arguments[outside_arguments.index(outside_anchor)] = (
                f"lock={fixture / 'lock'}"
            )
            outside = run_observer(outside_arguments)
            self.assertEqual(outside.returncode, 2)
            self.assertIn("outside its state root", outside.stderr)
            self.assertNotIn(os.fspath(fixture), outside.stderr)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux procfs is required")
    def test_service_observation_matches_exact_runtime_workspace_and_listener(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            executable = Path(sys.executable).resolve()
            runtime_digest = f"sha256:{hashlib.sha256(executable.read_bytes()).hexdigest()}"
            program = (
                "import socket,time; "
                "server=socket.socket(); "
                "server.bind(('127.0.0.1',0)); "
                "server.listen(); "
                "print(server.getsockname()[1],flush=True); "
                "time.sleep(30)"
            )
            process = subprocess.Popen(
                [os.fspath(executable), "-c", program],
                cwd=workspace,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                assert process.stdout is not None
                port = int(process.stdout.readline())
                arguments = [
                    "--output",
                    "json",
                    "service",
                    "--project-id",
                    "fixture-project",
                    "--service-id",
                    "fixture-service",
                    "--workspace",
                    os.fspath(workspace),
                    "--runtime-id",
                    "python-fixture",
                    "--runtime-sha256",
                    runtime_digest,
                    "--port",
                    str(port),
                ]
                result = run_observer(arguments)
                self.assertEqual(result.returncode, 0, result.stderr)
                report = json.loads(result.stdout)
                self.assertEqual(report["state"], "physical_match")
                self.assertEqual(report["listener_count"], 1)
                self.assertEqual(report["process_count"], 1)
                self.assertEqual(report["unresolved_listener_count"], 0)
                self.assertTrue(report["process"]["runtime_match"])
                self.assertTrue(report["process"]["workspace_match"])
                self.assertEqual(report["process"]["exposures"], ["loopback"])
                self.assertNotIn(os.fspath(workspace), result.stdout)
                self.assertNotIn("pid", all_keys(report))
                self.assertNotIn("argv", all_keys(report))

                repeated = run_observer(arguments)
                self.assertEqual(repeated.returncode, 0, repeated.stderr)
                self.assertEqual(
                    json.loads(repeated.stdout)["process"]["process_identity"],
                    report["process"]["process_identity"],
                )

                drifted = list(arguments)
                drifted[drifted.index(runtime_digest)] = RUNTIME_DIGEST
                drift = run_observer(drifted)
                self.assertEqual(drift.returncode, 0, drift.stderr)
                self.assertEqual(json.loads(drift.stdout)["state"], "drift")

                wrong_exposure = [*arguments, "--exposure", "any"]
                exposure = run_observer(wrong_exposure)
                self.assertEqual(exposure.returncode, 0, exposure.stderr)
                self.assertEqual(json.loads(exposure.stdout)["state"], "drift")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
                if process.stdout is not None:
                    process.stdout.close()
                if process.stderr is not None:
                    process.stderr.close()

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux procfs is required")
    def test_absent_service_is_a_normal_observation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with socket.socket() as probe:
                probe.bind(("127.0.0.1", 0))
                port = probe.getsockname()[1]
            result = run_observer(
                [
                    "--output",
                    "json",
                    "service",
                    "--project-id",
                    "fixture-project",
                    "--service-id",
                    "fixture-service",
                    "--workspace",
                    directory,
                    "--runtime-id",
                    "fixture-runtime",
                    "--runtime-sha256",
                    RUNTIME_DIGEST,
                    "--port",
                    str(port),
                ]
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["state"], "absent")
            self.assertEqual(report["listener_count"], 0)
            self.assertIsNone(report["process"])


if __name__ == "__main__":
    unittest.main()
