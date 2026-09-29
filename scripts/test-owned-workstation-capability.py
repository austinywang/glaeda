#!/usr/bin/env python3

import datetime as dt
import importlib.util
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("owned_workstation_capability.py")
SPEC = importlib.util.spec_from_file_location("owned_workstation_capability", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OwnedWorkstationCapabilityTests(unittest.TestCase):
    def test_projects_bounded_expiring_zero_authority_snapshot(self) -> None:
        snapshot = build(repo_query_projects=[repo_query_project()])
        self.assertEqual(snapshot["schema"], MODULE.SCHEMA)
        self.assertEqual(snapshot["expiresAt"], "2026-08-31T05:03:00.000Z")
        projects = {value["repository"]: value for value in snapshot["projects"]}
        self.assertEqual(projects["teamleaderleo/glaeda"]["heatClass"], "resident_hot")
        self.assertEqual(projects["teamleaderleo/quarry"], repo_query_project())
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
            "verify-required/v1",
            projects["teamleaderleo/glaeda"]["verificationProfiles"],
        )
        self.assertLessEqual(len(MODULE.canonical_json(snapshot)), 4096)

    def test_admits_only_canonical_path_private_project_observations(self) -> None:
        project = MODULE.repo_query_project_from_report(project_report())
        self.assertEqual(project, repo_query_project())
        self.assertNotIn("/home/leo/Projects/quarry", str(project))

        cased = project_report()
        cased["observation"]["primary_project"] = "github.com/TeamLeaderLeo/Quarry"
        self.assertEqual(MODULE.repo_query_project_from_report(cased), repo_query_project())
        self.assertEqual(
            MODULE.normalize_repo_query_project(repo_query_project("TeamLeaderLeo/Quarry")),
            repo_query_project(),
        )

        ambiguous = project_report()
        ambiguous["observation"]["source_ambiguous"] = True
        with self.assertRaisesRegex(MODULE.SnapshotError, "identity is not canonical"):
            MODULE.repo_query_project_from_report(ambiguous)

        wrong_authority = project_report()
        wrong_authority["authority"] = "execution"
        with self.assertRaisesRegex(MODULE.SnapshotError, "contract changed"):
            MODULE.repo_query_project_from_report(wrong_authority)

    def test_observer_output_limit_kills_producer_before_late_effect(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "late-effect"
            observer = root / "observer.py"
            observer.write_text(
                "#!/usr/bin/env python3\n"
                "import sys\n"
                "from pathlib import Path\n"
                "import time\n"
                f"sys.stdout.buffer.write(b'x' * {MODULE.MAX_OBSERVATION_BYTES + 1})\n"
                "sys.stdout.flush()\n"
                "time.sleep(2)\n"
                f"Path({str(marker)!r}).write_text('late', encoding='utf-8')\n",
                encoding="utf-8",
            )
            observer.chmod(0o755)
            with self.assertRaisesRegex(MODULE.SnapshotError, "exceeded output limit"):
                MODULE.repo_query_project_observation(observer, root)
            # The observer writes the marker at t≈2s. Checking immediately
            # would pass whether or not the kill worked, so wait past that.
            time.sleep(3)
            self.assertFalse(marker.exists())

    def test_observer_timeout_is_refused_without_naming_the_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            observer = root / "observer.py"
            observer.write_text(
                "#!/usr/bin/env python3\n"
                "import time\n"
                "time.sleep(60)\n",
                encoding="utf-8",
            )
            observer.chmod(0o755)
            original = MODULE.bounded_process_stdout

            def brief(argv, **kwargs):
                return original(argv, **{**kwargs, "timeout": 0.5})

            MODULE.bounded_process_stdout = brief
            try:
                with self.assertRaises(MODULE.SnapshotError) as caught:
                    MODULE.repo_query_project_observation(observer, root)
            finally:
                MODULE.bounded_process_stdout = original
            # TimeoutExpired stringifies its argv, which carries --checkout.
            self.assertNotIn(str(root), str(caught.exception))
            self.assertIn("timed out", str(caught.exception))

    def test_missing_observer_is_refused_without_naming_the_program(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            observer = root / "observer.py"
            observer.write_text("#!/usr/bin/env python3\n", encoding="utf-8")
            observer.chmod(0o644)
            with self.assertRaises(MODULE.SnapshotError) as caught:
                MODULE.repo_query_project_observation(observer, root)
            self.assertNotIn(str(root), str(caught.exception))

    def test_refuses_duplicate_and_over_ceiling_projects(self) -> None:
        with self.assertRaisesRegex(MODULE.SnapshotError, "duplicate projects"):
            build(repo_query_projects=[repo_query_project("teamleaderleo/glaeda")])
        with self.assertRaisesRegex(MODULE.SnapshotError, "duplicate projects"):
            build(repo_query_projects=[repo_query_project("TeamLeaderLeo/Glaeda")])
        with self.assertRaisesRegex(MODULE.SnapshotError, "too many projects"):
            build(repo_query_projects=[
                repo_query_project(f"teamleaderleo/project-{index}")
                for index in range(MODULE.MAX_PROJECTS)
            ])
        with self.assertRaisesRegex(MODULE.SnapshotError, "fields changed"):
            build(repo_query_projects=[{
                **repo_query_project(),
                "checkout": "/home/leo/Projects/quarry",
            }])

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
        "repo_query_projects": [],
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


def repo_query_project(repository="teamleaderleo/quarry"):
    return {
        "heatClass": "resident_cold",
        "repository": repository,
        "source": {"commitOid": "3" * 40, "treeOid": "4" * 40},
        "sourceObjectClass": "exact_commit_and_tree_present",
        "verificationProfiles": ["repo-query/v1"],
    }


def project_report():
    return {
        "document_type": "glaeda-project-observation",
        "schema_version": 1,
        "authority": "observation_only",
        "observation": {
            "schema_version": 2,
            "identity_generation": "glaeda_v2",
            "materialization_id": "sha256:" + "5" * 64,
            "primary_project": "github.com/teamleaderleo/quarry",
            "remotes": [{"name": "origin", "project": "github.com/teamleaderleo/quarry"}],
            "source_ambiguous": False,
            "commit": "3" * 40,
            "tree": "4" * 40,
            "branch": {"state": "attached", "name": "main"},
            "tracked_changes_present": False,
            "untracked_entry_count": 0,
            "upstream_configured": True,
            "local_commits_ahead": 0,
            "linked_worktree_count": 1,
            "submodules_present": False,
            "owner_matches_parent": True,
            "remote_freshness": "unknown",
        },
    }


if __name__ == "__main__":
    unittest.main()
