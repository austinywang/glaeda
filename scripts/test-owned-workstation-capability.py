#!/usr/bin/env python3

import datetime as dt
import importlib.util
from pathlib import Path
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("owned_workstation_capability.py")
SPEC = importlib.util.spec_from_file_location("owned_workstation_capability", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OwnedWorkstationCapabilityTests(unittest.TestCase):
    def test_projects_bounded_expiring_zero_authority_snapshot(self) -> None:
        snapshot = build()
        self.assertEqual(snapshot["schema"], MODULE.SCHEMA)
        self.assertEqual(snapshot["expiresAt"], "2026-08-31T05:03:00.000Z")
        self.assertEqual(snapshot["projects"][0]["heatClass"], "resident_hot")
        self.assertFalse(snapshot["authorizesDispatch"])
        self.assertFalse(snapshot["authorizesExecution"])
        self.assertEqual(
            snapshot["profiles"],
            [
                {
                    "class": "repo_query",
                    "id": "repo-query/v1",
                    "versionSha256": "sha256:" + "c" * 64,
                },
                {
                    "class": "verify_focused",
                    "id": "verify-focused/v1",
                    "versionSha256": "sha256:" + "e" * 64,
                },
                {
                    "class": "verify_required",
                    "id": "verify-required/v1",
                    "versionSha256": "sha256:" + "f" * 64,
                },
            ],
        )
        self.assertIn(
            "verify-required/v1", snapshot["projects"][0]["verificationProfiles"]
        )
        self.assertLessEqual(len(MODULE.canonical_json(snapshot)), 4096)

    def test_refuses_stale_windows_unknown_nodes_and_non_314_python(self) -> None:
        with self.assertRaisesRegex(MODULE.SnapshotError, "between 30 and 1800"):
            build(ttl_seconds=1801)
        with self.assertRaisesRegex(MODULE.SnapshotError, "Node ID"):
            build(node_id="Air Blue")
        with self.assertRaisesRegex(MODULE.SnapshotError, "Python 3.14"):
            with mock.patch.object(MODULE.subprocess, "run") as run:
                run.return_value.returncode = 0
                run.return_value.stdout = b"Python 3.9.6\n"
                run.return_value.stderr = b""
                MODULE.python_evidence(Path("/usr/bin/python3"))

    def test_cli_accepts_an_exact_composed_runtime_digest_without_a_path(self) -> None:
        arguments = MODULE.parser().parse_args([
            "--node-id", "big-red",
            "--node-generation", "1",
            "--os-class", "linux",
            "--architecture", "x86_64",
            "--glaeda-runtime-sha256", "sha256:" + "a" * 64,
            "--python-interpreter", "/usr/bin/python3.14",
            "--profile-generation", "sha256:" + "b" * 64,
            "--verify-required-generation", "sha256:" + "c" * 64,
        ])
        self.assertIsNone(arguments.glaeda_runtime)
        self.assertEqual(arguments.glaeda_runtime_sha256, "sha256:" + "a" * 64)


def build(**changes):
    values = {
        "receipt": receipt(),
        "node_id": "air-blue",
        "node_generation": 1,
        "os_class": "macos",
        "architecture_class": "arm64",
        "glaeda_runtime_sha256": "sha256:" + "a" * 64,
        "python": {"executableSha256": "sha256:" + "b" * 64, "version": "3.14.6"},
        "profile_generation": "sha256:" + "c" * 64,
        "verification_profile_generations": {
            "verify-focused/v1": "sha256:" + "e" * 64,
            "verify-required/v1": "sha256:" + "f" * 64,
        },
        "observed_at": dt.datetime(2026, 8, 31, 5, 0, tzinfo=dt.UTC),
        "ttl_seconds": 180,
    }
    values.update(changes)
    return MODULE.build_snapshot(**values)


def receipt():
    return {
        "state": "ready_with_declared_deviations",
        "repository_root": {"repository": "teamleaderleo/glaeda"},
        "source": {"commit": "1" * 40, "tree": "2" * 40},
        "declared_cache_paths": [{
            "name": "cargo-target",
            "exists": True,
            "directory": True,
            "ownership": "current-user",
            "symlink_alias_detected": False,
        }],
        "next_verification_profiles": ["glaeda.required", "glaeda.doctor"],
        "capability_fingerprint": "sha256:" + "d" * 64,
    }


if __name__ == "__main__":
    unittest.main()
