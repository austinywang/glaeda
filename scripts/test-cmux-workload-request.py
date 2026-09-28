#!/usr/bin/env python3
from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/cmux_workload_request.py"
SPEC = importlib.util.spec_from_file_location("cmux_workload_request", MODULE_PATH)
assert SPEC and SPEC.loader
module = importlib.util.module_from_spec(SPEC)
sys.modules["cmux_workload_request"] = module
SPEC.loader.exec_module(module)

BASE = {
    "document_type": "glaeda-cmux-workload-request",
    "schema_version": 1,
    "external_request_ref": "cmux:workload:fixture-1",
    "source": {
        "repository": "manaflow-ai/cmux",
        "commit": "1" * 40,
        "tree": "2" * 40,
    },
    "profile": {"id": "cmux.ci.guard", "generation": 1},
    "benchmark_state_class": "cold",
    "reuse_hint": "no_preference",
    "correlation": {"work_ref": "cmux:work:fixture-1"},
}


def raw(value: object) -> bytes:
    return module.canonical_bytes(value) + b"\n"


def cmux_result(request: module.Request) -> dict[str, object]:
    observations = {"python": "3.12.0"}
    result: dict[str, object] = {
        "document_type": "cmux-workload-result",
        "schema_version": 1,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "profile": {
            "id": request.profile_id,
            "generation": request.profile_generation,
        },
        "semantic_validator": "cmux.ci-guard/v1",
        "environment_class": "isolated-portable",
        "expected_result_class": "cmux.ci-guard-result/v1",
        "result": "passed",
        "parameters": request.parameters,
        "runtime_input_identities": [],
        "artifact_identities": [],
        "validation": {"missing_required_artifact_classes": []},
        "stage_timings": [{"stage": "test", "seconds": 1.25}],
        "resource_summary": {
            "resource_class": "cmux-linux-ci-small",
            "cpu_count": 4,
            "memory_bytes": 17179869184,
            "architecture": "x86_64",
        },
        "toolchain": {
            "identity": module.sha256(module.canonical_bytes(observations)),
            "observations": observations,
        },
        "benchmark": {
            "state_class": request.benchmark_state_class,
            "semantic_comparison_key": "sha256:" + "0" * 64,
            "comparison_context_key": "sha256:" + "0" * 64,
        },
        "network_class": "none",
        "timeout_class": "portable-short",
        "cleanup": {"state": "complete", "process_group_settled": True},
        "exit_code": 0,
        "started_at_unix_millis": 1000,
        "ended_at_unix_millis": 2250,
    }
    semantic = module._cmux_semantic_key(result)
    result["benchmark"]["semantic_comparison_key"] = semantic
    result["benchmark"]["comparison_context_key"] = module._cmux_context_key(
        semantic,
        request.benchmark_state_class,
        result["toolchain"]["identity"],
    )
    return result


class CmuxWorkloadRequestTests(unittest.TestCase):
    def test_plan_resolves_only_fixed_repository_adapter(self) -> None:
        request = module.decode_request(raw(BASE))
        plan = module.plan(request)
        self.assertEqual(plan["state"], "planned")
        self.assertEqual(
            plan["resolved_adapter"],
            {
                "kind": "cmux-repository-profile/v1",
                "repository_runner": "scripts/ci/cmux_workload_profile.py",
                "semantic_result_contract": "cmux-workload-result/v1",
            },
        )
        self.assertEqual(plan["authority"], module.AUTHORITY)
        encoded = module.canonical_bytes(plan)
        for forbidden in (b"argv", b"cwd", b"machine", b"resource_class", b"state_root"):
            self.assertNotIn(forbidden, encoded)

    def test_execution_binding_ignores_caller_correlation_and_reuse_hint(self) -> None:
        first = module.decode_request(raw(BASE))
        changed = copy.deepcopy(BASE)
        changed["external_request_ref"] = "cmux:workload:other"
        changed["correlation"]["work_ref"] = "cmux:work:other"
        changed["reuse_hint"] = "prefer_valid_reuse"
        second = module.decode_request(raw(changed))
        self.assertEqual(module.execution_binding(first), module.execution_binding(second))
        self.assertNotEqual(
            module.sha256(module.canonical_bytes(module.request_document(first))),
            module.sha256(module.canonical_bytes(module.request_document(second))),
        )

    def test_profile_generation_changes_execution_binding(self) -> None:
        first = module.decode_request(raw(BASE))
        changed = copy.deepcopy(BASE)
        changed["profile"]["generation"] = 2
        second = module.decode_request(raw(changed))
        self.assertNotEqual(module.execution_binding(first), module.execution_binding(second))

    def test_rejects_execution_controls(self) -> None:
        for field, value in {
            "argv": ["caller-selected-program"],
            "cwd": "/caller/path",
            "machine": "caller-machine",
            "backend": "caller-backend",
            "resource_class": "caller-resource",
            "state_root": "/caller/cache",
        }.items():
            with self.subTest(field=field):
                changed = copy.deepcopy(BASE)
                changed[field] = value
                with self.assertRaisesRegex(module.ContractRefusal, "request fields"):
                    module.decode_request(raw(changed))

    def test_observation_preserves_cmux_semantic_result(self) -> None:
        request = module.decode_request(raw(BASE))
        result_raw = raw(cmux_result(request))
        observation = module.observe(request, result_raw)
        self.assertEqual(observation["state"], "succeeded")
        self.assertEqual(observation["cmux_semantic_validator"], "cmux.ci-guard/v1")
        self.assertEqual(
            observation["cmux_semantic_result_sha256"], module.sha256(result_raw)
        )

    def test_observation_rejects_source_or_profile_drift(self) -> None:
        request = module.decode_request(raw(BASE))
        result = cmux_result(request)
        result["profile"]["generation"] = 2
        semantic = module._cmux_semantic_key(result)
        result["benchmark"]["semantic_comparison_key"] = semantic
        result["benchmark"]["comparison_context_key"] = module._cmux_context_key(
            semantic,
            result["benchmark"]["state_class"],
            result["toolchain"]["identity"],
        )
        with self.assertRaisesRegex(module.ContractRefusal, "differs from request"):
            module.observe(request, raw(result))

    def test_observation_rejects_incomplete_passed_result(self) -> None:
        request = module.decode_request(raw(BASE))

        missing_artifact = cmux_result(request)
        missing_artifact["validation"]["missing_required_artifact_classes"] = [
            "cmux-required-product"
        ]
        with self.assertRaisesRegex(module.ContractRefusal, "passed CMUX result"):
            module.observe(request, raw(missing_artifact))

        unsettled = cmux_result(request)
        unsettled["cleanup"] = {
            "state": "forced",
            "process_group_settled": False,
        }
        with self.assertRaisesRegex(module.ContractRefusal, "complete cleanup"):
            module.observe(request, raw(unsettled))

        bad_exit = cmux_result(request)
        bad_exit["exit_code"] = 1
        with self.assertRaisesRegex(module.ContractRefusal, "passed CMUX result"):
            module.observe(request, raw(bad_exit))

    def test_observation_rejects_result_structure_and_toolchain_tampering(self) -> None:
        request = module.decode_request(raw(BASE))

        extra = cmux_result(request)
        extra["unexpected_private_field"] = "must-not-pass"
        with self.assertRaisesRegex(module.ContractRefusal, "result fields"):
            module.observe(request, raw(extra))

        tampered_toolchain = cmux_result(request)
        tampered_toolchain["toolchain"]["observations"]["python"] = "different"
        with self.assertRaisesRegex(module.ContractRefusal, "toolchain identity"):
            module.observe(request, raw(tampered_toolchain))

        missing_cpu = cmux_result(request)
        missing_cpu["resource_summary"]["cpu_count"] = None
        with self.assertRaisesRegex(module.ContractRefusal, "resource summary"):
            module.observe(request, raw(missing_cpu))

    def test_negative_result_requires_settled_cleanup_unless_ambiguous(self) -> None:
        request = module.decode_request(raw(BASE))

        failed = cmux_result(request)
        failed["result"] = "failed"
        failed["exit_code"] = 1
        observation = module.observe(request, raw(failed))
        self.assertEqual(observation["state"], "failed")

        ambiguous = cmux_result(request)
        ambiguous["result"] = "ambiguous"
        ambiguous["exit_code"] = -15
        ambiguous["cleanup"] = {
            "state": "forced",
            "process_group_settled": False,
        }
        observation = module.observe(request, raw(ambiguous))
        self.assertEqual(observation["state"], "ambiguous")

        contradictory = cmux_result(request)
        contradictory["result"] = "ambiguous"
        with self.assertRaisesRegex(module.ContractRefusal, "ambiguous CMUX result"):
            module.observe(request, raw(contradictory))

    def test_observation_rejects_self_inconsistent_cmux_result(self) -> None:
        request = module.decode_request(raw(BASE))
        cases = []

        extra = cmux_result(request)
        extra["unexpected"] = True
        cases.append(("fields", extra))

        toolchain = cmux_result(request)
        toolchain["toolchain"]["observations"]["python"] = "changed"
        cases.append(("toolchain", toolchain))

        semantic = cmux_result(request)
        semantic["benchmark"]["semantic_comparison_key"] = "sha256:" + "f" * 64
        cases.append(("semantic comparison", semantic))

        environment = cmux_result(request)
        environment["environment_class"] = "isolated-build"
        cases.append(("semantic comparison", environment))

        cleanup = cmux_result(request)
        cleanup["cleanup"] = {"state": "forced", "process_group_settled": False}
        cases.append(("cleanup", cleanup))

        artifacts = cmux_result(request)
        artifacts["validation"]["missing_required_artifact_classes"] = [
            "cmux.required-artifact/v1"
        ]
        cases.append(("passed CMUX result", artifacts))

        for label, result in cases:
            with self.subTest(label=label):
                with self.assertRaises(module.ContractRefusal):
                    module.observe(request, raw(result))

    def test_ambiguous_result_requires_forced_unsettled_cleanup(self) -> None:
        request = module.decode_request(raw(BASE))
        result = cmux_result(request)
        result["result"] = "ambiguous"
        result["exit_code"] = 0
        result["cleanup"] = {"state": "forced", "process_group_settled": False}
        observation = module.observe(request, raw(result))
        self.assertEqual(observation["state"], "ambiguous")

        result["cleanup"] = {"state": "complete", "process_group_settled": True}
        with self.assertRaisesRegex(module.ContractRefusal, "ambiguous"):
            module.observe(request, raw(result))

    def test_repository_plan_fixture_round_trip(self) -> None:
        request_path = ROOT / "docs/experiments/cmux-workload-profile/request.json"
        plan_path = ROOT / "docs/experiments/cmux-workload-profile/plan.json"
        planned = subprocess.run(
            [sys.executable, str(MODULE_PATH), "plan"],
            input=request_path.read_bytes(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(planned.returncode, 0, planned.stderr.decode())
        self.assertEqual(planned.stdout, plan_path.read_bytes())


if __name__ == "__main__":
    unittest.main()
