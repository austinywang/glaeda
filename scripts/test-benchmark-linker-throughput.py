#!/usr/bin/env python3
"""Focused tests for the explicit linker-throughput benchmark harness."""

from __future__ import annotations

import ast
import json
import os
import runpy
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


SCRIPT = Path(__file__).with_name("benchmark-linker-throughput")
MODULE = SimpleNamespace(**runpy.run_path(os.fspath(SCRIPT), run_name="linker_benchmark_test"))


class LinkerBenchmarkTests(unittest.TestCase):
    def test_every_subprocess_call_has_an_explicit_environment(self) -> None:
        tree = ast.parse(SCRIPT.read_text(encoding="utf-8"), filename=os.fspath(SCRIPT))
        missing = []
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute):
                continue
            if (
                isinstance(node.func.value, ast.Name)
                and node.func.value.id == "subprocess"
                and node.func.attr in {"run", "Popen"}
                and not any(keyword.arg == "env" for keyword in node.keywords)
            ):
                missing.append(node.lineno)
        self.assertEqual(missing, [])

    def test_closed_child_environment_excludes_ambient_secrets(self) -> None:
        with mock.patch.dict(
            os.environ, {"REVIEW_FAKE_SECRET": "must-not-cross"}, clear=False
        ):
            environment = MODULE.closed_child_environment()
        self.assertEqual(environment, {"LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"})
        self.assertNotIn("REVIEW_FAKE_SECRET", environment)

    def test_encoded_flags_force_each_linker_arm(self) -> None:
        clang = Path("/usr/bin/clang").resolve(strict=True)
        self.assertEqual(
            MODULE.encoded_rustflags("gnu"),
            f"-Clinker={clang}\x1f-Clink-arg=-fuse-ld=bfd",
        )
        self.assertEqual(
            MODULE.encoded_rustflags("lld"),
            f"-Clinker={clang}\x1f-Clink-arg=-fuse-ld=lld",
        )
        self.assertEqual(
            MODULE.encoded_rustflags("mold"),
            f"-Clinker={clang}\x1f-Clink-arg=-fuse-ld=mold",
        )
        with self.assertRaises(MODULE.BenchmarkError):
            MODULE.encoded_rustflags("other")

    def test_summary_uses_nearest_rank_p90(self) -> None:
        self.assertEqual(
            MODULE.summarize([6.0, 1.0, 4.0, 2.0, 5.0, 3.0]),
            {"minimum": 1.0, "median": 3.5, "p90": 6.0, "maximum": 6.0},
        )

    def test_default_nine_round_schedule_balances_every_arm_position(self) -> None:
        arms = [(linker, width) for linker in MODULE.LINKERS for width in MODULE.WIDTHS]
        self.assertEqual(len(arms), 9)
        self.assertTrue(MODULE.schedule_is_position_balanced(arms, 9))
        self.assertFalse(MODULE.schedule_is_position_balanced(arms, 6))
        for arm in arms:
            positions = [
                MODULE.measured_order(arms, round_index).index(arm)
                for round_index in range(9)
            ]
            self.assertEqual(sorted(positions), list(range(9)))

    def test_storage_preflight_uses_available_blocks(self) -> None:
        with mock.patch.object(
            MODULE.os,
            "statvfs",
            return_value=SimpleNamespace(f_bavail=1, f_frsize=4096),
        ):
            with self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.require_experiment_storage(Path("/private"))
        self.assertEqual(raised.exception.code, "insufficient_storage")

    def test_worker_environment_is_closed_and_forces_linker_identity(self) -> None:
        capabilities = MODULE.Capabilities(
            paths={"clang": Path("/usr/bin/clang")},
            versions={},
            build_executables=frozenset(),
            path_identity_sha256="sha256:" + "a" * 64,
        )
        with tempfile.TemporaryDirectory() as temporary, mock.patch.object(
            MODULE.pwd,
            "getpwuid",
            return_value=SimpleNamespace(pw_dir=temporary),
        ), mock.patch.dict(
            os.environ,
            {
                "REVIEW_FAKE_SECRET": "must-not-cross",
                "CARGO_PROFILE_DEV_OPT_LEVEL": "3",
                "RUSTC_WRAPPER": "/tmp/ambient-wrapper",
            },
            clear=False,
        ):
            environment = MODULE.worker_environment(
                Path("/source"), Path("/target"), "gnu", capabilities
            )
        self.assertEqual(
            set(environment),
            {
                "HOME",
                "PATH",
                "LANG",
                "LC_ALL",
                "CARGO_TERM_COLOR",
                "CARGO_BUILD_JOBS",
                "CARGO_TARGET_DIR",
                "CARGO_ENCODED_RUSTFLAGS",
            },
        )
        self.assertNotIn("REVIEW_FAKE_SECRET", environment)
        self.assertNotIn("CARGO_PROFILE_DEV_OPT_LEVEL", environment)
        self.assertNotIn("RUSTC_WRAPPER", environment)
        self.assertIn("-fuse-ld=bfd", environment["CARGO_ENCODED_RUSTFLAGS"])

    def test_worker_environment_rejects_ambient_cargo_config(self) -> None:
        capabilities = MODULE.Capabilities(
            paths={"clang": Path("/usr/bin/clang")},
            versions={},
            build_executables=frozenset(),
            path_identity_sha256="sha256:" + "a" * 64,
        )
        with tempfile.TemporaryDirectory() as temporary:
            cargo_home = Path(temporary) / ".cargo"
            cargo_home.mkdir()
            (cargo_home / "config.toml").write_text("[build]\n", encoding="utf-8")
            with mock.patch.object(
                MODULE.pwd,
                "getpwuid",
                return_value=SimpleNamespace(pw_dir=temporary),
            ), self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.worker_environment(
                    Path("/source"), Path("/target"), "gnu", capabilities
                )
        self.assertEqual(raised.exception.code, "ambient_cargo_config")

    def test_scope_command_has_crash_backstop_and_closed_control_environment(self) -> None:
        process = mock.Mock(pid=123)
        scope = MODULE.OwnedScope("unit.scope", -1, -1, -1, -1)
        replacements = {
            "unique_scope_unit": mock.Mock(return_value="unit.scope"),
            "systemd_control_environment": mock.Mock(
                return_value={"LANG": "C.UTF-8"}
            ),
            "admit_scope": mock.Mock(return_value=scope),
        }
        with mock.patch.dict(
            MODULE.start_owned_scope.__globals__, replacements
        ), mock.patch.object(
            MODULE.subprocess, "Popen", return_value=process
        ) as popen:
            observed_process, observed_scope = MODULE.start_owned_scope(
                ["/usr/bin/python3", "/private/entry"], 90.0, subprocess.DEVNULL
            )
        self.assertIs(observed_process, process)
        self.assertIs(observed_scope, scope)
        argv = popen.call_args.args[0]
        self.assertIn("--property=KillMode=control-group", argv)
        self.assertIn("--property=SendSIGKILL=yes", argv)
        self.assertIn("--property=RuntimeMaxSec=95s", argv)
        self.assertIn("--property=TimeoutStopSec=2s", argv)
        self.assertEqual(popen.call_args.kwargs["env"], {"LANG": "C.UTF-8"})
        self.assertTrue(popen.call_args.kwargs["start_new_session"])

    def test_scope_launch_interrupt_after_popen_stops_the_transient_unit(self) -> None:
        process = mock.Mock(pid=123)
        replacements = {
            "unique_scope_unit": mock.Mock(return_value="unit.scope"),
            "systemd_control_environment": mock.Mock(
                return_value={"LANG": "C.UTF-8"}
            ),
            "admit_scope": mock.Mock(side_effect=KeyboardInterrupt()),
            "stop_transient_unit": mock.Mock(),
        }
        with mock.patch.dict(
            MODULE.start_owned_scope.__globals__, replacements
        ), mock.patch.object(MODULE.subprocess, "Popen", return_value=process):
            with self.assertRaises(KeyboardInterrupt):
                MODULE.start_owned_scope(
                    ["/usr/bin/python3", "/private/entry"],
                    90.0,
                    subprocess.DEVNULL,
                )
        replacements["stop_transient_unit"].assert_called_once_with(
            "unit.scope", process
        )

    def test_scope_observation_and_kill_use_held_descriptors(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "cgroup.procs").write_text("123\n456\n", encoding="ascii")
            (root / "cgroup.events").write_text("populated 1\nfrozen 0\n", encoding="ascii")
            (root / "cgroup.kill").write_bytes(b"")
            cgroup_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            kill_fd = os.open("cgroup.kill", os.O_WRONLY, dir_fd=cgroup_fd)
            scope = MODULE.OwnedScope("unit.scope", cgroup_fd, kill_fd, -1, -1)
            try:
                self.assertEqual(MODULE.scope_processes(scope), {123, 456})
                self.assertTrue(MODULE.scope_is_populated(scope))
                MODULE.kill_scope(scope)
                self.assertEqual((root / "cgroup.kill").read_bytes(), b"1")
            finally:
                scope.close()

    def test_finish_workers_refuses_a_scope_that_never_empties(self) -> None:
        scope = MODULE.OwnedScope("unit.scope", -1, -1, -1, -1)
        worker = MODULE.Worker(
            mock.Mock(), Path("receipt"), Path("log"), Path("target"), scope
        )
        with mock.patch.object(
            MODULE, "scope_is_populated", return_value=True
        ), mock.patch.object(MODULE, "kill_scope"), mock.patch.object(
            MODULE, "wait_scope_empty", return_value=False
        ), self.assertRaises(MODULE.BenchmarkError) as raised:
            MODULE.finish_workers([worker])
        self.assertEqual(raised.exception.code, "worker_cleanup_incomplete")

    def test_spawn_failure_stops_every_already_started_worker(self) -> None:
        capabilities = MODULE.Capabilities(
            paths={"python": Path("/usr/bin/python3")},
            versions={},
            build_executables=frozenset(),
            path_identity_sha256="sha256:" + "a" * 64,
        )
        process = mock.Mock(pid=123)
        scope = MODULE.OwnedScope("unit.scope", -1, -1, -1, -1)
        admission_error = MODULE.BenchmarkError(
            "scope_admission", "the second worker was not admitted"
        )
        with tempfile.TemporaryDirectory() as temporary:
            experiment = Path(temporary)
            for name in ("targets", "receipts", "logs"):
                (experiment / name).mkdir()
            replacements = {
                "foreign_build_count": mock.Mock(return_value=0),
                "start_owned_scope": mock.Mock(
                    side_effect=[(process, scope), admission_error]
                ),
                "stop_workers": mock.Mock(),
            }
            with mock.patch.dict(MODULE.run_batch.__globals__, replacements):
                with self.assertRaises(MODULE.BenchmarkError) as raised:
                    MODULE.run_batch(
                        root=Path("/source"),
                        experiment=experiment,
                        source=MODULE.SourceIdentity("a" * 40, "b" * 40),
                        capabilities=capabilities,
                        linker="gnu",
                        width=2,
                        phase="cold",
                        index=0,
                    )
        self.assertIs(raised.exception, admission_error)
        stopped = replacements["stop_workers"].call_args.args[0]
        self.assertEqual(len(stopped), 1)
        self.assertIs(stopped[0].process, process)
        self.assertIs(stopped[0].scope, scope)

    def test_post_admission_log_close_failure_stops_the_registered_worker(self) -> None:
        capabilities = MODULE.Capabilities(
            paths={"python": Path("/usr/bin/python3")},
            versions={},
            build_executables=frozenset(),
            path_identity_sha256="sha256:" + "a" * 64,
        )
        process = mock.Mock(pid=123)
        scope = MODULE.OwnedScope("unit.scope", -1, -1, -1, -1)
        stream = mock.Mock()
        stream.close.side_effect = OSError("validation-only close failure")
        with tempfile.TemporaryDirectory() as temporary:
            experiment = Path(temporary)
            for name in ("targets", "receipts", "logs"):
                (experiment / name).mkdir()
            replacements = {
                "foreign_build_count": mock.Mock(return_value=0),
                "start_owned_scope": mock.Mock(return_value=(process, scope)),
                "stop_workers": mock.Mock(),
            }
            with mock.patch.dict(
                MODULE.run_batch.__globals__, replacements
            ), mock.patch.object(MODULE.Path, "open", return_value=stream):
                with self.assertRaises(MODULE.BenchmarkError) as raised:
                    MODULE.run_batch(
                        root=Path("/source"),
                        experiment=experiment,
                        source=MODULE.SourceIdentity("a" * 40, "b" * 40),
                        capabilities=capabilities,
                        linker="gnu",
                        width=1,
                        phase="cold",
                        index=0,
                    )
        self.assertEqual(raised.exception.code, "worker_spawn")
        stopped = replacements["stop_workers"].call_args.args[0]
        self.assertEqual(len(stopped), 1)
        self.assertIs(stopped[0].process, process)
        self.assertIs(stopped[0].scope, scope)

    def test_post_fork_sigint_is_replayed_only_after_worker_registration(self) -> None:
        capabilities = MODULE.Capabilities(
            paths={"python": Path("/usr/bin/python3")},
            versions={},
            build_executables=frozenset(),
            path_identity_sha256="sha256:" + "a" * 64,
        )
        process = mock.Mock(pid=123)
        scope = MODULE.OwnedScope("unit.scope", -1, -1, -1, -1)

        def interrupted_popen(*_arguments: object, **_keywords: object) -> mock.Mock:
            os.kill(os.getpid(), signal.SIGINT)
            return process

        with tempfile.TemporaryDirectory() as temporary:
            experiment = Path(temporary)
            for name in ("targets", "receipts", "logs"):
                (experiment / name).mkdir()
            replacements = {
                "foreign_build_count": mock.Mock(return_value=0),
                "stop_workers": mock.Mock(),
            }
            scope_replacements = {
                "unique_scope_unit": mock.Mock(return_value="unit.scope"),
                "systemd_control_environment": mock.Mock(
                    return_value={"LANG": "C.UTF-8"}
                ),
                "admit_scope": mock.Mock(return_value=scope),
                "stop_transient_unit": mock.Mock(),
            }
            with mock.patch.dict(
                MODULE.run_batch.__globals__, replacements
            ), mock.patch.dict(
                MODULE.start_owned_scope.__globals__, scope_replacements
            ), mock.patch.object(
                MODULE.subprocess, "Popen", side_effect=interrupted_popen
            ):
                with self.assertRaises(KeyboardInterrupt):
                    MODULE.run_batch(
                        root=Path("/source"),
                        experiment=experiment,
                        source=MODULE.SourceIdentity("a" * 40, "b" * 40),
                        capabilities=capabilities,
                        linker="gnu",
                        width=1,
                        phase="cold",
                        index=0,
                    )
        stopped = replacements["stop_workers"].call_args.args[0]
        self.assertEqual(len(stopped), 1)
        self.assertIs(stopped[0].process, process)
        self.assertIs(stopped[0].scope, scope)
        scope_replacements["stop_transient_unit"].assert_not_called()

    @unittest.skipUnless(
        os.environ.get("GLAEDA_RUN_LINKER_SCOPE_PHYSICAL") == "1",
        "explicit physical user-systemd scope test",
    )
    def test_physical_scope_kills_nested_session(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pids = root / "pids"
            with (root / "scope.log").open("wb") as stream:
                process, scope = MODULE.start_owned_scope(
                    [
                        sys.executable,
                        SCRIPT,
                        MODULE.INTERNAL_NESTED_FIXTURE,
                        "--",
                        pids,
                    ],
                    10.0,
                    stream,
                )
            worker = MODULE.Worker(
                process, Path("receipt"), Path("log"), Path("target"), scope
            )
            deadline = time.monotonic() + 2
            while not pids.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            leader, nested = [int(value) for value in pids.read_text().split()]
            self.assertNotEqual(os.getpgid(leader), os.getpgid(nested))
            MODULE.stop_workers([worker])
            for pid in (leader, nested):
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)

    def test_process_group_keeps_reparented_worker_owned(self) -> None:
        roots = {100}
        table = {
            101: (1, 100, "rustc", 4096),
            200: (1, 200, "cargo", 8192),
        }
        self.assertTrue(MODULE.is_worker_owned(101, roots, table))
        self.assertEqual(MODULE.foreign_build_count(roots, table), 1)
        self.assertEqual(MODULE.foreign_build_executables(roots, table), ["cargo"])
        self.assertEqual(MODULE.descendant_rss_kib(roots, table), 4)

    def test_quiescence_waits_for_a_clean_observation(self) -> None:
        replacement = mock.Mock(side_effect=[1, 0])
        with mock.patch.dict(
            MODULE.wait_for_foreign_build_quiescence.__globals__,
            {"foreign_build_count": replacement},
        ), mock.patch.object(MODULE.time, "sleep"):
            MODULE.wait_for_foreign_build_quiescence(
                quiet_seconds=0.0, wait_seconds=1.0
            )
        self.assertEqual(replacement.call_count, 2)

    def test_foreign_build_detection_covers_canonical_tool_names(self) -> None:
        table = {
            101: (1, 101, "x86_64-linux-gnu-gcc-15", 4096),
            102: (1, 102, "x86_64-linux-gnu-ld.bfd", 4096),
            103: (1, 103, "lld", 4096),
            104: (1, 104, "go", 4096),
        }
        self.assertEqual(
            MODULE.foreign_build_executables(set(), table),
            ["go", "lld", "x86_64-linux-gnu-gcc-15", "x86_64-linux-gnu-ld.bfd"],
        )

    def test_aggregate_rejects_an_overlapped_accepted_observation(self) -> None:
        with self.assertRaises(MODULE.BenchmarkError) as raised:
            MODULE.aggregate(
                MODULE.SourceIdentity("a" * 40, "b" * 40),
                {},
                "sha256:" + "c" * 64,
                1,
                [{"phase": "measured", "foreign_build_overlap_observed": True}],
                [],
                {},
                {},
                {},
                {},
                1,
                1,
            )
        self.assertEqual(raised.exception.code, "accepted_overlap")

    def test_target_stats_rejects_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "file").write_bytes(b"payload")
            clean = MODULE.target_stats(root)
            self.assertEqual(clean["entries"], 1)
            (root / "link").symlink_to(root / "file")
            with self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.target_stats(root)
            self.assertEqual(raised.exception.code, "target_symlink")

    def test_atomic_json_is_private(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "receipt.json"
            MODULE.write_json(output, {"schema_version": 1})
            self.assertEqual(json.loads(output.read_text()), {"schema_version": 1})
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            with self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.write_json(output, {"schema_version": 2})
            self.assertEqual(raised.exception.code, "receipt_write")
            self.assertEqual(json.loads(output.read_text()), {"schema_version": 1})

    def test_receipt_validator_accepts_exact_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "target"
            target.mkdir()
            digest = MODULE.hashlib.sha256(
                b"glaeda-local-verification-path-v1\0"
            )
            digest.update(os.fsencode(target.resolve()))
            document = {
                "schema_version": 1,
                "document_type": "glaeda-local-verification-receipt",
                "authority": "performance_observation_only",
                "profile": "fast",
                "source": {"commit": "a" * 40, "tree": "b" * 40, "unchanged": True},
                "environment": {
                    "cargo_build_jobs": {"source": "configured", "value": 4},
                    "cargo_target": {
                        "source": "configured",
                        "identity_digest": f"sha256:{digest.hexdigest()}",
                    },
                },
                "plan": {"declared_phase_count": 7, "executed_phase_count": 7},
                "phases": [
                    {"name": name, "exit_code": 0, "elapsed_seconds": 1.0}
                    for name in MODULE.FAST_PHASES
                ],
                "result": {
                    "exit_code": 0,
                    "elapsed_seconds": 7.0,
                    "child_user_cpu_seconds": 8.0,
                    "child_system_cpu_seconds": 1.0,
                    "process_lifetime_child_max_rss_kib": 100,
                },
            }
            receipt = root / "receipt.json"
            receipt.write_text(json.dumps(document))
            observed = MODULE.load_receipt(
                receipt, MODULE.SourceIdentity("a" * 40, "b" * 40), target
            )
            self.assertEqual(observed["result"]["exit_code"], 0)
            document["source"]["unchanged"] = False
            receipt.write_text(json.dumps(document))
            with self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.load_receipt(
                    receipt, MODULE.SourceIdentity("a" * 40, "b" * 40), target
                )
            self.assertEqual(raised.exception.code, "invalid_receipt")


if __name__ == "__main__":
    unittest.main()
