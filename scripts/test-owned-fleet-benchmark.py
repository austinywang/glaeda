#!/usr/bin/env python3
from __future__ import annotations

import copy
import json
import itertools
import re
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import owned_fleet_benchmark as fleet
from owned_fleet_benchmark.observe import run_shell, validate_semantic
from owned_fleet_benchmark.cli import parser as fleet_parser
from owned_fleet_benchmark.report import (
    _stable_profile_sets,
    collect_json_files,
    markdown_report,
)
from owned_fleet_benchmark.run import _validate_direct_runtime
from owned_fleet_benchmark.model import GIT_PROBE_ENV, env_for, git_identity, verify_source

NS = {name: getattr(fleet, name) for name in fleet.__all__}
FleetError = fleet.FleetError
GIB = 1024**3
SAMPLE_IDS = itertools.count()


def complete_machine() -> dict:
    return {
        "schema_version": 1,
        "document_type": "glaeda-owned-fleet-machine-receipt",
        "status": "complete",
        "machine_id": "fixture-node",
        "machine_config": "fixture linux node",
        "purchase": {
            "date": "2026-09-01",
            "currency": "USD",
            "all_in_price": 2400,
        },
        "cpu": {
            "vendor": "Fixture",
            "model": "CPU",
            "topology": {
                "physical_cores": 8,
                "logical_cpus": 8,
                "classes": [],
            },
        },
        "ram_bytes": 32 * GIB,
        "internal_storage": {
            "capacity_bytes": 10**12,
            "public_model": "fixture nvme",
            "bus": "nvme",
            "filesystem": "ext4",
        },
        "platform": {
            "os": "linux",
            "os_version": "fixture",
            "kernel": "fixture",
            "xcode": None,
            "rust_toolchain": "rustc 1.97.1",
            "python": "3.14",
        },
        "power_policy": "AC/performance",
        "filesystem": "ext4",
        "network_class": "1gbe",
        "power": {
            "measurement_status": "measured",
            "idle_watts": 10.0,
            "load_watts": 50.0,
            "measurement_method": "fixture meter",
        },
        "thermal": {
            "observation_status": "measured",
            "sustained_minutes": 30,
            "max_temperature_c": 80.0,
            "throttling_observed": False,
            "notes": "fixture",
        },
        "privacy": {
            "serial_numbers_omitted": True,
            "hostname_omitted": True,
            "unrelated_host_details_omitted": True,
        },
    }


def benchmark_receipt(
    *,
    machine: dict,
    start_ns: int,
    request_ns: int,
    latency_ms: float,
    cpu_millis: int,
    memory_bytes: int,
    validated: bool = True,
    fallback: int = 0,
    backend_id: str = "native-linux",
    state_class: str = "project_resident",
) -> dict:
    command_start = max(start_ns, request_ns)
    command_exit = command_start + 200_000_000
    final_result = request_ns + int(latency_ms * 1_000_000)
    if final_result < command_exit:
        final_result = command_exit
        latency_ms = (final_result - request_ns) / 1_000_000
    return {
        "schema_version": 1,
        "document_type": "glaeda-owned-fleet-benchmark-receipt",
        "authority": "performance_observation_only",
        "sample_id": f"sample-{next(SAMPLE_IDS)}",
        "machine_id": machine["machine_id"],
        "machine_comparison_digest": NS["machine_comparison_digest"](machine),
        "machine_receipt_digest": "sha256:fixture-full-machine",
        "workload": {
            "id": "glaeda-rust-focused-v1",
            "variant": None,
            "commit": "a" * 40,
            "tree": "b" * 40,
            "operation_digest": "sha256:operation",
        },
        "state": {
            "class": state_class,
            "preparation_id": "fixture-preparation-v1",
        },
        "storage": {
            "source_tier": "internal",
            "source_id": "machine-internal",
            "state_tier": "internal",
            "state_id": "machine-internal",
            "source_filesystem": "ext4",
            "state_filesystem": "ext4",
        },
        "toolchain": {"digest": "sha256:toolchain"},
        "execution": {
            "backend_id": backend_id,
            "runtime": {"digest": "sha256:runtime"},
            "resource_policy_id": "fixture-cgroup",
            "resource_policy_status": "enforced",
            "resource_policy_evidence_id": "fixture-policy-evidence-v1",
            "declared_cpu_millis": cpu_millis,
            "declared_memory_limit_bytes": memory_bytes,
            "timeout_seconds": 7200.0,
        },
        "timing": {
            "request_known_monotonic_ns": request_ns,
            "command_start_monotonic_ns": command_start,
            "command_exit_monotonic_ns": command_exit,
            "final_result_monotonic_ns": final_result,
        },
        "milestones": {
            "request_known_to_final_result_ms": latency_ms,
        },
        "result": {
            "class": "validated" if validated else "command_failed",
            "validated": validated,
            "exit_code": 0 if validated else 1,
            "failure_count": 0 if validated else 1,
            "timed_out": False,
            "source_unchanged": True,
            "semantic_validation": (
                "fixture_accepted" if validated else "command_exit_nonzero"
            ),
        },
        "resources": {
            "peak_aggregate_rss_kib": 1000,
            "swap_used_start_bytes": 0,
            "swap_used_max_observed_bytes": 0,
            "swap_used_end_bytes": 0,
            "memory_psi_some_avg10_max_observed": 0.2,
            "max_temperature_c_observed": 70.0,
        },
        "queue_delay_ms": 0,
        "events": {"fallback_count": fallback, "reset_count": 0},
    }


def window_manifest(profile_id: str, items: list[dict], *, start_ns: int = 1_000_000_000) -> dict:
    return {
        "schema_version": 1,
        "document_type": "glaeda-owned-fleet-window-manifest",
        "experiment_id": "fixture-window",
        "machine_id": "fixture-node",
        "workload_id": "glaeda-rust-focused-v1",
        "variant": None,
        "state_class": "project_resident",
        "profile_id": profile_id,
        "window_start_monotonic_ns": start_ns,
        "window_elapsed_seconds": 10,
        "arrival_pattern_id": "simultaneous-burst-v1",
        "arrival_tolerance_ms": 1,
        "resource_policy_id": "fixture-cgroup",
        "resource_policy_status": "enforced",
        "resource_policy_evidence_id": "fixture-policy-evidence-v1",
        "aggregate_cpu_millis": 8000,
        "aggregate_memory_limit_bytes": 16 * GIB,
        "offered_work": items,
    }


class FleetHarnessTests(unittest.TestCase):
    def test_benchmark_environment_is_closed(self) -> None:
        workload = {
            "environment": {
                "CARGO_TARGET_DIR": "{state_dir}/target",
            }
        }
        ambient = {
            "PATH": "/usr/bin:/bin",
            "HOME": "/home/fixture",
            "CARGO_HOME": "/home/fixture/.cargo",
            "RUSTUP_HOME": "/home/fixture/.rustup",
            "DEVELOPER_DIR": "/Applications/Xcode.app",
            "XDG_RUNTIME_DIR": "/run/user/1000",
            "SSH_AUTH_SOCK": "/tmp/agent.sock",
            "GITHUB_TOKEN": "secret",
            "PYTHONPATH": "/tmp/injected",
            "GIT_CONFIG_COUNT": "1",
            "TMPDIR": "/tmp/caller",
        }
        with patch.dict("os.environ", ambient, clear=True):
            environment = env_for(workload, Path("/private/benchmark-state"))

        self.assertEqual(environment["PATH"], ambient["PATH"])
        self.assertEqual(environment["HOME"], ambient["HOME"])
        self.assertEqual(environment["DEVELOPER_DIR"], ambient["DEVELOPER_DIR"])
        self.assertEqual(environment["XDG_RUNTIME_DIR"], ambient["XDG_RUNTIME_DIR"])
        self.assertEqual(
            environment["CARGO_TARGET_DIR"],
            "/private/benchmark-state/target",
        )
        self.assertEqual(environment["LANG"], "C")
        self.assertEqual(environment["LC_ALL"], "C")
        self.assertEqual(environment["GIT_CONFIG_GLOBAL"], "/dev/null")
        self.assertEqual(environment["GIT_CONFIG_NOSYSTEM"], "1")
        for forbidden in (
            "SSH_AUTH_SOCK",
            "GITHUB_TOKEN",
            "PYTHONPATH",
            "GIT_CONFIG_COUNT",
            "TMPDIR",
        ):
            self.assertNotIn(forbidden, environment)

    def test_git_source_identity_uses_closed_environment(self) -> None:
        calls = []

        def fake_run(argv, **kwargs):
            calls.append((argv, kwargs))
            if "rev-parse" in argv:
                return __import__("subprocess").CompletedProcess(
                    argv,
                    0,
                    stdout="a" * 40 + "\n" + "b" * 40 + "\n",
                    stderr="",
                )
            return __import__("subprocess").CompletedProcess(
                argv,
                0,
                stdout=b"",
                stderr=b"",
            )

        workload = {"commit": "a" * 40, "tree": "b" * 40}
        with patch("owned_fleet_benchmark.model.subprocess.run", side_effect=fake_run):
            self.assertEqual(
                git_identity(Path("/fixture")),
                ("a" * 40, "b" * 40),
            )
            verify_source(Path("/fixture"), workload)

        self.assertEqual(calls[0][0][0], "/usr/bin/git")
        self.assertEqual(calls[0][1]["env"], GIT_PROBE_ENV)
        status_call = next(call for call in calls if "status" in call[0])
        self.assertEqual(status_call[0][0], "/usr/bin/git")
        self.assertIn("--ignore-submodules=none", status_call[0])
        self.assertEqual(status_call[1]["env"], GIT_PROBE_ENV)
        for _, kwargs in calls:
            for forbidden in (
                "HOME",
                "PATH",
                "GIT_DIR",
                "GIT_WORK_TREE",
                "SSH_AUTH_SOCK",
            ):
                self.assertNotIn(forbidden, kwargs["env"])

    def test_reviewed_shell_runner_uses_absolute_bash(self) -> None:
        observed = {}

        def fake_run(argv, **kwargs):
            observed["argv"] = argv
            observed["kwargs"] = kwargs
            return __import__("subprocess").CompletedProcess(
                argv,
                0,
                stdout="ok",
                stderr=None,
            )

        environment = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "LANG": "C"}
        with patch("owned_fleet_benchmark.observe.subprocess.run", side_effect=fake_run):
            result = run_shell("printf ok", Path("/fixture"), environment)

        self.assertEqual(result.returncode, 0)
        self.assertEqual(observed["argv"], ["/bin/bash", "-c", "printf ok"])
        self.assertEqual(observed["kwargs"]["env"], environment)

    def test_catalog_keeps_only_contention_shape(self) -> None:
        value = NS["catalog"]()
        self.assertEqual(len(value["workloads"]), 6)
        self.assertEqual(
            value["contention_profiles"],
            {"large": {"jobs": 1}, "medium": {"jobs": 2}, "small": {"jobs": 4}},
        )
        incremental = next(
            item
            for item in value["workloads"]
            if item["id"] == "cmux-native-apple-incremental-v1"
        )
        self.assertIn("touch Sources/RestorableAgentSession.swift", incremental["operation"]["command"])
        self.assertIsNotNone(
            re.search(
                incremental["toolchain"]["required_output_regex"],
                "26.3",
                re.MULTILINE,
            )
        )

    def test_direct_backend_is_bound_to_native_runtime_identity(self) -> None:
        machine = complete_machine()
        machine["platform"]["os"] = "linux"
        machine["platform"]["kernel"] = "6.8.0-fixture"
        machine["cpu"]["topology"]["logical_cpus"] = 8
        with (
            patch("owned_fleet_benchmark.run.platform.system", return_value="Linux"),
            patch("owned_fleet_benchmark.run.platform.release", return_value="6.8.0-fixture"),
            patch("owned_fleet_benchmark.run.os.cpu_count", return_value=8),
        ):
            _validate_direct_runtime(machine, "native-linux")
            with self.assertRaises(FleetError):
                _validate_direct_runtime(machine, "lima-vz")
            machine["platform"]["kernel"] = "different"
            with self.assertRaises(FleetError):
                _validate_direct_runtime(machine, "native-linux")

    def test_partial_machine_is_planning_only(self) -> None:
        partial = json.loads(
            (
                ROOT
                / "benchmarks"
                / "fleet"
                / "machine-receipt.v1.template.json"
            ).read_text()
        )
        NS["validate_machine"](partial, require_complete=False)
        with self.assertRaises(FleetError):
            NS["validate_machine"](partial, require_complete=True)

    def test_machine_comparison_digest_ignores_economics_and_observations(self) -> None:
        machine = complete_machine()
        first = NS["machine_comparison_digest"](machine)
        changed = copy.deepcopy(machine)
        changed["purchase"]["all_in_price"] = 3000
        changed["power"]["idle_watts"] = 12
        changed["power"]["load_watts"] = 60
        changed["thermal"]["max_temperature_c"] = 90
        self.assertEqual(first, NS["machine_comparison_digest"](changed))
        changed["power_policy"] = "balanced"
        self.assertNotEqual(first, NS["machine_comparison_digest"](changed))

    def test_complete_machine_requires_privacy_and_observed_identity(self) -> None:
        machine = complete_machine()
        self.assertEqual(
            NS["validate_machine"](machine, require_complete=True)["machine_id"],
            "fixture-node",
        )
        machine["privacy"]["hostname_omitted"] = False
        with self.assertRaises(FleetError):
            NS["validate_machine"](machine, require_complete=True)

    def test_state_classes_have_exact_boolean_contracts(self) -> None:
        for state in NS["catalog"]()["state_classes"]:
            value = NS["state_template"](state)
            value["preparation_id"] = f"fixture-{state}"
            self.assertEqual(
                NS["validate_state_evidence"](value)["state_class"], state
            )
        bad = NS["state_template"]("project_resident")
        bad["preparation_id"] = "fixture"
        bad["observed"]["exact_reusable_product_present"] = True
        with self.assertRaises(FleetError):
            NS["validate_state_evidence"](bad)

    def test_direct_runner_cli_refuses_unowned_routing_event_inputs(self) -> None:
        base = [
            "run",
            "--workload",
            "glaeda-rust-focused-v1",
            "--repo-root",
            "/tmp/repo",
            "--machine",
            "/tmp/machine.json",
            "--state-evidence",
            "/tmp/state.json",
            "--state-dir",
            "/tmp/state",
            "--backend-id",
            "native-linux",
            "--output",
            "/tmp/out.json",
        ]
        for unowned in (
            ["--queue-delay-ms", "9000"],
            ["--fallback-count", "4"],
            ["--reset-count", "3"],
        ):
            with self.subTest(unowned=unowned):
                with self.assertRaises(SystemExit):
                    fleet_parser().parse_args(base + unowned)

    def test_zero_settled_window_preserves_bounded_negative_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            items = [
                {
                    "work_id": f"w{index}",
                    "arrival_offset_ms": index * 1000,
                    "receipt": None,
                }
                for index in range(4)
            ]
            partial = NS["reduce_window"](
                window_manifest("small", items), root
            )

        self.assertEqual(
            partial["document_type"],
            "glaeda-owned-fleet-window-partial-receipt",
        )
        self.assertEqual(partial["authority"], "declared_offer_only")
        self.assertEqual(partial["evidence_class"], "manifest_only")
        self.assertEqual(partial["counts"]["offered"], 4)
        self.assertEqual(partial["counts"]["settled"], 0)
        self.assertEqual(partial["counts"]["validated_completions"], 0)
        self.assertEqual(partial["counts"]["unfinished"], 4)
        self.assertIsNone(partial["final_result_latency_ms"]["p50"])
        self.assertIsNone(partial["final_result_latency_ms"]["p90"])
        self.assertEqual(
            partial["concurrency"]["maximum_simultaneous_observed"], 0
        )
        with self.assertRaisesRegex(
            FleetError, "unsupported window receipt"
        ):
            NS["compare_windows"]([partial, partial, partial])

    def test_partial_window_remains_visible_in_machine_report(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            partial = NS["reduce_window"](
                window_manifest(
                    "small",
                    [
                        {
                            "work_id": f"w{index}",
                            "arrival_offset_ms": index * 1000,
                            "receipt": None,
                        }
                        for index in range(4)
                    ],
                ),
                root,
            )

        report = markdown_report(machine, [], [partial], [])
        self.assertIn("unobserved", report)
        self.assertIn("evidence=manifest_only", report)
        self.assertNotIn("settled no offered work", report)

    def test_window_reducer_uses_validated_numerator_and_unfinished(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            items = []
            profile_jobs = 2
            per_cpu = 8000 // profile_jobs
            per_memory = 16 * GIB // profile_jobs
            for index, (offset_ms, latency_ms) in enumerate(
                ((0, 1000.0), (0, 2000.0), (3000, 3000.0))
            ):
                request_ns = 1_000_000_000 + offset_ms * 1_000_000
                value = benchmark_receipt(
                    machine=machine,
                    start_ns=request_ns + 10_000_000,
                    request_ns=request_ns,
                    latency_ms=latency_ms,
                    cpu_millis=per_cpu,
                    memory_bytes=per_memory,
                )
                path = root / f"r{index}.json"
                path.write_text(json.dumps(value))
                items.append(
                    {
                        "work_id": f"w{index}",
                        "arrival_offset_ms": offset_ms,
                        "receipt": path.name,
                    }
                )
            items.append(
                {
                    "work_id": "w3",
                    "arrival_offset_ms": 6000,
                    "receipt": None,
                }
            )
            result = NS["reduce_window"](
                window_manifest("medium", items), root
            )
            self.assertEqual(result["counts"]["validated_completions"], 3)
            self.assertEqual(result["counts"]["unfinished"], 1)
            self.assertEqual(result["final_result_latency_ms"]["p50"], 2000.0)
            self.assertEqual(result["final_result_latency_ms"]["p90"], 3000.0)
            self.assertEqual(
                result["concurrency"]["maximum_simultaneous_observed"], 2
            )

    def test_window_reducer_does_not_blame_preexisting_swap_on_window(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            value = benchmark_receipt(
                machine=machine,
                start_ns=1_010_000_000,
                request_ns=1_000_000_000,
                latency_ms=500,
                cpu_millis=8000,
                memory_bytes=16 * GIB,
            )
            value["resources"]["swap_used_start_bytes"] = 4 * GIB
            value["resources"]["swap_used_max_observed_bytes"] = 4 * GIB
            value["resources"]["swap_used_end_bytes"] = 4 * GIB
            path = root / "r0.json"
            path.write_text(json.dumps(value))
            manifest = window_manifest(
                "large",
                [
                    {
                        "work_id": "w0",
                        "arrival_offset_ms": 0,
                        "receipt": path.name,
                    }
                ],
            )
            reduced = NS["reduce_window"](manifest, root)
            self.assertEqual(
                reduced["resources"]["swap_growth_max_observed_bytes"], 0.0
            )

    def test_window_reducer_refuses_over_concurrency(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            items = []
            for index in range(2):
                request_ns = 1_000_000_000
                value = benchmark_receipt(
                    machine=machine,
                    start_ns=1_010_000_000,
                    request_ns=request_ns,
                    latency_ms=1000,
                    cpu_millis=8000,
                    memory_bytes=16 * GIB,
                )
                path = root / f"r{index}.json"
                path.write_text(json.dumps(value))
                items.append(
                    {
                        "work_id": f"w{index}",
                        "arrival_offset_ms": 0,
                        "receipt": path.name,
                    }
                )
            with self.assertRaises(FleetError):
                NS["reduce_window"](window_manifest("large", items), root)

    def test_window_reducer_preserves_distinct_terminal_classes(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            items = []
            classes = (
                "validated",
                "semantic_mismatch",
                "source_changed",
                "command_failed",
            )
            for index, result_class in enumerate(classes):
                request_ns = 1_000_000_000 + index * 1_000_000_000
                value = benchmark_receipt(
                    machine=machine,
                    start_ns=request_ns + 10_000_000,
                    request_ns=request_ns,
                    latency_ms=500,
                    cpu_millis=2000,
                    memory_bytes=4 * GIB,
                    validated=result_class == "validated",
                )
                value["result"]["class"] = result_class
                value["result"]["validated"] = result_class == "validated"
                value["result"]["timed_out"] = False
                value["result"]["failure_count"] = (
                    1 if result_class == "command_failed" else 0
                )
                value["result"]["exit_code"] = (
                    1 if result_class == "command_failed" else 0
                )
                value["result"]["source_unchanged"] = result_class != "source_changed"
                value["result"]["semantic_validation"] = {
                    "validated": "fixture_accepted",
                    "semantic_mismatch": "semantic_receipt_rejected",
                    "source_changed": "source_changed_after_execution",
                    "command_failed": "command_exit_nonzero",
                }[result_class]
                path = root / f"r{index}.json"
                path.write_text(json.dumps(value))
                items.append(
                    {
                        "work_id": f"w{index}",
                        "arrival_offset_ms": index * 1000,
                        "receipt": path.name,
                    }
                )

            reduced = NS["reduce_window"](
                window_manifest("small", items), root
            )

        self.assertEqual(reduced["counts"]["validated_completions"], 1)
        self.assertEqual(reduced["counts"]["semantic_mismatch_count"], 1)
        self.assertEqual(reduced["counts"]["unvalidated_completion_count"], 1)
        self.assertEqual(reduced["counts"]["failure_count"], 1)
        self.assertEqual(reduced["counts"]["unknown_result_count"], 0)

    def test_window_reducer_refuses_inconsistent_member_reliability(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            base = benchmark_receipt(
                machine=machine,
                start_ns=1_010_000_000,
                request_ns=1_000_000_000,
                latency_ms=500,
                cpu_millis=8000,
                memory_bytes=16 * GIB,
            )

            cases = []
            bad_failure = copy.deepcopy(base)
            bad_failure["result"]["failure_count"] = 1
            cases.append(bad_failure)

            bad_fallback = copy.deepcopy(base)
            bad_fallback["events"]["fallback_count"] = 2
            cases.append(bad_fallback)

            bad_reset = copy.deepcopy(base)
            bad_reset["events"]["reset_count"] = -1
            cases.append(bad_reset)

            bad_latency = copy.deepcopy(base)
            bad_latency["milestones"]["request_known_to_final_result_ms"] += 10
            cases.append(bad_latency)

            bad_authority = copy.deepcopy(base)
            bad_authority["authority"] = "declared_only"
            cases.append(bad_authority)

            bad_exit = copy.deepcopy(base)
            bad_exit["result"]["class"] = "command_failed"
            bad_exit["result"]["validated"] = False
            bad_exit["result"]["failure_count"] = 1
            bad_exit["result"]["exit_code"] = 0
            bad_exit["result"]["semantic_validation"] = "command_exit_nonzero"
            cases.append(bad_exit)

            for index, value in enumerate(cases):
                with self.subTest(index=index):
                    path = root / f"bad-{index}.json"
                    path.write_text(json.dumps(value))
                    manifest = window_manifest(
                        "large",
                        [
                            {
                                "work_id": "w0",
                                "arrival_offset_ms": 0,
                                "receipt": path.name,
                            }
                        ],
                    )
                    with self.assertRaises(FleetError):
                        NS["reduce_window"](manifest, root)

    def test_window_reducer_refuses_duplicate_sample_receipt(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            value = benchmark_receipt(
                machine=machine,
                start_ns=1_010_000_000,
                request_ns=1_000_000_000,
                latency_ms=500,
                cpu_millis=4000,
                memory_bytes=8 * GIB,
            )
            items = []
            for index in range(2):
                path = root / f"r{index}.json"
                path.write_text(json.dumps(value))
                items.append(
                    {
                        "work_id": f"w{index}",
                        "arrival_offset_ms": 0,
                        "receipt": path.name,
                    }
                )
            with self.assertRaises(FleetError):
                NS["reduce_window"](window_manifest("medium", items), root)

    def test_window_reducer_refuses_mixed_machine_identity(self) -> None:
        machine = complete_machine()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            items = []
            for index in range(2):
                request_ns = 1_000_000_000 + index * 1_000_000_000
                value = benchmark_receipt(
                    machine=machine,
                    start_ns=request_ns + 10_000_000,
                    request_ns=request_ns,
                    latency_ms=500,
                    cpu_millis=4000,
                    memory_bytes=8 * GIB,
                )
                if index:
                    value["machine_comparison_digest"] = "sha256:different"
                path = root / f"r{index}.json"
                path.write_text(json.dumps(value))
                items.append(
                    {
                        "work_id": f"w{index}",
                        "arrival_offset_ms": index * 1000,
                        "receipt": path.name,
                    }
                )
            with self.assertRaises(FleetError):
                NS["reduce_window"](window_manifest("medium", items), root)

    def _reduced_windows(self) -> list[dict]:
        machine = complete_machine()
        values = []
        for profile_id, jobs in (("large", 1), ("medium", 2), ("small", 4)):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                items = []
                for index in range(4):
                    offset_ms = index * 2000
                    request_ns = 1_000_000_000 + offset_ms * 1_000_000
                    value = benchmark_receipt(
                        machine=machine,
                        start_ns=request_ns + 10_000_000,
                        request_ns=request_ns,
                        latency_ms=500,
                        cpu_millis=8000 // jobs,
                        memory_bytes=16 * GIB // jobs,
                    )
                    path = root / f"r{index}.json"
                    path.write_text(json.dumps(value))
                    items.append(
                        {
                            "work_id": f"w{index}",
                            "arrival_offset_ms": offset_ms,
                            "receipt": path.name,
                        }
                    )
                values.append(
                    NS["reduce_window"](
                        window_manifest(profile_id, items), root
                    )
                )
        return values

    def test_window_comparison_requires_all_three_profiles(self) -> None:
        values = self._reduced_windows()
        with self.assertRaises(FleetError):
            NS["compare_windows"](values[:2])
        compared = NS["compare_windows"](values)
        self.assertEqual(
            [row["profile_id"] for row in compared["rows"]],
            ["large", "medium", "small"],
        )

    def test_report_does_not_turn_reliability_failures_into_buy_signals(self) -> None:
        machine = complete_machine()
        values = self._reduced_windows()
        values[1] = copy.deepcopy(values[1])
        values[1]["counts"]["validated_completions"] -= 1
        values[1]["counts"]["failure_count"] = 1
        report = markdown_report(machine, [], values, [])

        bottleneck = report.split(
            "## What existing bottleneck would buying another one remove?",
            1,
        )[1].split("## Correctness and reliability caveats", 1)[0]
        caveats = report.split(
            "## Correctness and reliability caveats",
            1,
        )[1].split(
            "## At what utilization does ownership beat observed hosted alternatives?",
            1,
        )[0]

        self.assertNotIn("failed jobs", bottleneck)
        self.assertIn("failed jobs", caveats)
        self.assertIn("zero evidence that another node would remove the problem", caveats)

    def test_window_comparison_surfaces_failures(self) -> None:
        values = self._reduced_windows()
        values[1] = copy.deepcopy(values[1])
        values[1]["counts"]["failure_count"] = 1
        compared = NS["compare_windows"](values)
        medium = next(
            row for row in compared["rows"] if row["profile_id"] == "medium"
        )
        self.assertEqual(medium["failure_count"], 1)
        self.assertIn("failed_work", medium["collapse_flags"])

    def test_role_stability_rejects_swap_growth_and_p90_collapse(self) -> None:
        values = self._reduced_windows()
        for value in values:
            value["concurrency"]["underfilled"] = False
        stable, unproven = _stable_profile_sets(values)
        self.assertIn(("glaeda-rust-focused-v1", "native-linux"), stable)
        self.assertEqual(unproven, [])

        swapped = copy.deepcopy(values)
        swapped[1]["resources"]["swap_growth_max_observed_bytes"] = 1024
        self.assertNotIn(
            ("glaeda-rust-focused-v1", "native-linux"),
            _stable_profile_sets(swapped)[0],
        )

        collapsed = copy.deepcopy(values)
        collapsed[2]["final_result_latency_ms"]["p90"] = (
            collapsed[0]["final_result_latency_ms"]["p90"] * 2
        )
        self.assertNotIn(
            ("glaeda-rust-focused-v1", "native-linux"),
            _stable_profile_sets(collapsed)[0],
        )

    def test_role_stability_refuses_unmeasured_swap_and_missing_failure_count(
        self,
    ) -> None:
        values = self._reduced_windows()
        for value in values:
            value["concurrency"]["underfilled"] = False
        self.assertEqual(
            values[0]["resources"]["swap_growth_max_observed_bytes"], 0.0
        )

        # swap_used_bytes() returns None when the probe fails or the platform is
        # unsupported, so None means never observed, not observed to be zero.
        unobserved = copy.deepcopy(values)
        unobserved[1]["resources"]["swap_growth_max_observed_bytes"] = None
        stable, unproven = _stable_profile_sets(unobserved)
        self.assertNotIn(("glaeda-rust-focused-v1", "native-linux"), stable)
        self.assertTrue(any("swap" in reason for reason in unproven))

        missing = copy.deepcopy(values)
        del missing[2]["counts"]["failure_count"]
        stable, unproven = _stable_profile_sets(missing)
        self.assertNotIn(("glaeda-rust-focused-v1", "native-linux"), stable)
        self.assertTrue(any("failure_count" in reason for reason in unproven))

        # A boolean is not a measured byte count.
        boolean = copy.deepcopy(values)
        boolean[0]["resources"]["swap_growth_max_observed_bytes"] = False
        self.assertNotIn(
            ("glaeda-rust-focused-v1", "native-linux"),
            _stable_profile_sets(boolean)[0],
        )

    def test_report_names_the_unproven_stability_input(self) -> None:
        machine = complete_machine()
        values = self._reduced_windows()
        for value in values:
            value["concurrency"]["underfilled"] = False
        values[1] = copy.deepcopy(values[1])
        values[1]["resources"]["swap_growth_max_observed_bytes"] = None
        report = markdown_report(machine, [], values, [])
        role_section = report.split("## Fleet-planning role evidence", 1)[1]
        self.assertIn("swap", role_section)
        self.assertIn("glaeda-rust-focused-v1", role_section)

    def test_report_preserves_unknown_failure_count(self) -> None:
        values = self._reduced_windows()
        del values[1]["counts"]["failure_count"]
        report = markdown_report(complete_machine(), [], values, [])
        window_line = next(
            line for line in report.splitlines()
            if "`medium`:" in line
        )
        self.assertIn("failures=unobserved", window_line)

    def test_stability_reports_nonfinite_measurements_as_unproven(self) -> None:
        for invalid in (float("nan"), float("inf"), float("-inf")):
            with self.subTest(invalid=invalid):
                values = self._reduced_windows()
                values[0]["resources"]["swap_growth_max_observed_bytes"] = invalid
                stable, reasons = _stable_profile_sets(values)
                self.assertEqual(stable, set())
                self.assertTrue(any("swap" in reason for reason in reasons))

    def test_stability_requires_finite_nonnegative_latency(self) -> None:
        for invalid in (float("inf"), float("nan"), -1, None, False):
            with self.subTest(invalid=invalid):
                values = self._reduced_windows()
                for value in values:
                    value["concurrency"]["underfilled"] = False
                    value["final_result_latency_ms"]["p90"] = invalid
                stable, reasons = _stable_profile_sets(values)
                self.assertEqual(stable, set())
                self.assertTrue(any("p90" in reason for reason in reasons))

    def test_collect_json_files_refuses_a_corrupt_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "good.json").write_text(
                json.dumps(
                    {"document_type": "glaeda-owned-vs-hosted-economics"}
                )
            )
            self.assertEqual(
                len(
                    collect_json_files(
                        root, "glaeda-owned-vs-hosted-economics"
                    )
                ),
                1,
            )
            (root / "corrupt.json").write_text("{not json")
            with self.assertRaisesRegex(FleetError, "corrupt.json"):
                collect_json_files(root, "glaeda-owned-vs-hosted-economics")

    def test_collect_json_files_refuses_a_non_object_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "list.json").write_text("[]")
            with self.assertRaisesRegex(FleetError, "list.json"):
                collect_json_files(root, "glaeda-owned-vs-hosted-economics")

    def test_window_comparison_refuses_changed_budget(self) -> None:
        values = self._reduced_windows()
        values[1] = copy.deepcopy(values[1])
        values[1]["window_elapsed_seconds"] = 11
        with self.assertRaises(FleetError):
            NS["compare_windows"](values)

    def test_hosted_economics_requires_measurement_and_rate_provenance(self) -> None:
        machine = complete_machine()
        owned = benchmark_receipt(
            machine=machine,
            start_ns=1_010_000_000,
            request_ns=1_000_000_000,
            latency_ms=60_000,
            cpu_millis=8000,
            memory_bytes=16 * GIB,
        )
        hosted = {
            "schema_version": 1,
            "document_type": "glaeda-hosted-equivalent-job-receipt",
            "backend": "fixture-hosted",
            "measurement_date": "2026-09-21",
            "validated": True,
            "workload_id": "glaeda-rust-focused-v1",
            "variant": None,
            "source_commit": "a" * 40,
            "source_tree": "b" * 40,
            "operation_digest": "sha256:operation",
            "toolchain_digest": "sha256:toolchain",
            "state_class": "project_resident",
            "actual_wall_seconds": 60,
            "queue_delay_seconds": 20,
            "rate_per_minute": 2.0,
            "rate_source": "fixture-provider-rate-card",
            "billing_currency": "USD",
            "billing_increment_seconds": 1,
            "minimum_billed_seconds": 0,
            "fx_to_purchase_currency": None,
            "fx_observed_at": None,
        }

        with self.assertRaisesRegex(FleetError, "measurement_evidence"):
            NS["economics"](
                owned, hosted, machine, 1.0, [36], [0.5]
            )

        hosted["measurement_evidence_sha256"] = "sha256:" + "e" * 64
        del hosted["rate_source"]
        with self.assertRaisesRegex(FleetError, "rate_source"):
            NS["economics"](
                owned, hosted, machine, 1.0, [36], [0.5]
            )

    def test_economics_allows_different_heat_state_and_null_same_currency_fx(self) -> None:
        machine = complete_machine()
        owned = benchmark_receipt(
            machine=machine,
            start_ns=1_010_000_000,
            request_ns=1_000_000_000,
            latency_ms=60_000,
            cpu_millis=8000,
            memory_bytes=16 * GIB,
        )
        owned["queue_delay_ms"] = 0
        hosted = {
            "schema_version": 1,
            "document_type": "glaeda-hosted-equivalent-job-receipt",
            "backend": "fixture-hosted",
            "measurement_date": "2026-09-21",
            "validated": True,
            "workload_id": "glaeda-rust-focused-v1",
            "variant": None,
            "source_commit": "a" * 40,
            "source_tree": "b" * 40,
            "operation_digest": "sha256:operation",
            "toolchain_digest": "sha256:toolchain",
            "state_class": "cold",
            "actual_wall_seconds": 60,
            "queue_delay_seconds": 20,
            "measurement_evidence_sha256": "sha256:" + "e" * 64,
            "rate_per_minute": 2.0,
            "rate_source": "fixture-provider-rate-card",
            "billing_currency": "USD",
            "billing_increment_seconds": 1,
            "minimum_billed_seconds": 0,
            "fx_to_purchase_currency": None,
            "fx_observed_at": None,
        }
        reference = benchmark_receipt(
            machine=machine,
            start_ns=1_010_000_000,
            request_ns=1_000_000_000,
            latency_ms=90_000,
            cpu_millis=8000,
            memory_bytes=16 * GIB,
            state_class="cold",
        )
        result = NS["economics"](
            owned, hosted, machine, 1.0, [36], [0.10, 0.50], reference, 0.33
        )
        self.assertEqual(
            result["hosted"]["measurement_evidence_sha256"],
            "sha256:" + "e" * 64,
        )
        self.assertEqual(
            result["hosted"]["rate_source"],
            "fixture-provider-rate-card",
        )
        self.assertFalse(result["state_context"]["same_state_class"])
        self.assertTrue(result["hot_state_benefit"]["available"])
        self.assertEqual(result["hot_state_benefit"]["reference_state_class"], "cold")
        self.assertAlmostEqual(result["hot_state_benefit"]["seconds_saved"], 30.0)
        observed_rows = [
            row
            for row in result["sensitivities"]
            if row["utilization_basis"] == "observed"
        ]
        self.assertEqual(len(observed_rows), 1)
        self.assertAlmostEqual(observed_rows[0]["utilization"], 0.33)
        sensitivity_rows = [
            row
            for row in result["sensitivities"]
            if row["utilization_basis"] == "sensitivity"
        ]
        low, high = sensitivity_rows
        self.assertGreater(
            low["accounted_watt_hours_per_validated_completion"],
            high["accounted_watt_hours_per_validated_completion"],
        )
        self.assertIsNotNone(low["break_even_utilization"])

    def test_economics_refuses_different_machine_identity(self) -> None:
        machine = complete_machine()
        owned = benchmark_receipt(
            machine=machine,
            start_ns=1_010_000_000,
            request_ns=1_000_000_000,
            latency_ms=60_000,
            cpu_millis=8000,
            memory_bytes=16 * GIB,
        )
        owned["machine_comparison_digest"] = "sha256:different"
        hosted = {
            "schema_version": 1,
            "document_type": "glaeda-hosted-equivalent-job-receipt",
            "backend": "fixture-hosted",
            "measurement_date": "2026-09-21",
            "validated": True,
            "workload_id": "glaeda-rust-focused-v1",
            "variant": None,
            "source_commit": "a" * 40,
            "source_tree": "b" * 40,
            "operation_digest": "sha256:operation",
            "toolchain_digest": "sha256:toolchain",
            "state_class": "project_resident",
            "actual_wall_seconds": 60,
            "queue_delay_seconds": 0,
            "measurement_evidence_sha256": "sha256:" + "e" * 64,
            "rate_per_minute": 1,
            "rate_source": "fixture-provider-rate-card",
            "billing_currency": "USD",
            "billing_increment_seconds": 1,
            "minimum_billed_seconds": 0,
            "fx_to_purchase_currency": None,
            "fx_observed_at": None,
        }
        with self.assertRaises(FleetError):
            NS["economics"](owned, hosted, machine, 1.0, [36], [0.5])

    def test_glaeda_semantic_receipt_requires_exact_profile(self) -> None:
        workload = {
            "commit": "a" * 40,
            "tree": "b" * 40,
            "toolchain": {},
        }
        value = {
            "document_type": "glaeda-local-verification-receipt",
            "profile": "full-tests",
            "source": {
                "commit": "a" * 40,
                "tree": "b" * 40,
                "unchanged": True,
            },
            "result": {"exit_code": 0},
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "semantic.json"
            path.write_text(json.dumps(value))
            valid, _, _ = validate_semantic(
                "glaeda_verification_receipt",
                {"expected_profile": "focused"},
                0,
                False,
                path,
                workload,
            )
            self.assertFalse(valid)

    def test_quarry_parallel_receipt_requires_exact_verifier_closure(self) -> None:
        workload = {
            "commit": "a" * 40,
            "tree": "b" * 40,
            "toolchain": {"known_verifier_toolchain_id": "toolchain:expected"},
        }
        value = {
            "schema_version": 2,
            "receipt_kind": "quarry-parallel-verification-receipt-v2",
            "result": {"class": "passed", "termination_reason": "completed"},
            "cleanup": {"status": "passed"},
            "evidence_scope": "exact_head",
            "hosted_ci_evidence": False,
            "merge_authority": False,
            "verified_head": "a" * 40,
            "plan": {
                "key": {
                    "source": {"commit": "a" * 40, "tree": "b" * 40},
                    "toolchain_id": "toolchain:different",
                }
            },
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "semantic.json"
            path.write_text(json.dumps(value))
            valid, _, _ = validate_semantic(
                "quarry_parallel_receipt",
                {},
                0,
                False,
                path,
                workload,
            )
            self.assertFalse(valid)

    def test_report_refuses_mixed_machine_evidence(self) -> None:
        machine = complete_machine()
        value = benchmark_receipt(
            machine=machine,
            start_ns=1_010_000_000,
            request_ns=1_000_000_000,
            latency_ms=500,
            cpu_millis=8000,
            memory_bytes=16 * GIB,
        )
        value["machine_id"] = "other-node"
        with self.assertRaises(FleetError):
            NS["markdown_report"](machine, [value], [], [])

    def test_report_contains_decision_questions_without_premature_role(self) -> None:
        machine = complete_machine()
        value = benchmark_receipt(
            machine=machine,
            start_ns=1_010_000_000,
            request_ns=1_000_000_000,
            latency_ms=500,
            cpu_millis=8000,
            memory_bytes=16 * GIB,
        )
        report = NS["markdown_report"](machine, [value], [], [])
        self.assertIn("What does this machine do well?", report)
        self.assertIn("How many useful concurrent jobs can it sustain?", report)
        self.assertIn("What existing bottleneck would buying another one remove?", report)
        self.assertIn(
            "At what utilization does ownership beat observed hosted alternatives?",
            report,
        )
        self.assertIn("What workloads should still stay hosted?", report)
        self.assertIn(
            "No fleet-planning role is classified before repeated validated samples",
            report,
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
