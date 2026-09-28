#!/usr/bin/env python3
"""Synthetic contract tests for the caller-neutral external execution request."""
from __future__ import annotations

import copy
from pathlib import Path
import subprocess
import sys
import unittest

import external_execution_request as module
import verify_focused_impl as focused

BASE = {
    "document_type": "glaeda-external-execution-request",
    "schema_version": 1,
    "external_request_ref": "cmux:exec:1050:fixture-1",
    "source": {
        "repository": "teamleaderleo/glaeda",
        "commit": "0409c2f4e82385d0770bb2b34f34fd3e6e2dbc36",
        "tree": "03613a93ce152aefddbb246613084705043b1397",
    },
    "operation": "verify_focused",
    "requested_capability_class": "credentialless_project",
    "reuse_hint": "prefer_valid_reuse",
    "correlation": {"work_ref": "cmux:work:1050"},
}


def raw(value: object = BASE) -> bytes:
    return module.canonical_bytes(value) + b"\n"


def internal_receipt(
    compiled: module.CompiledRequest, terminal: str = "succeeded"
) -> dict[str, object]:
    return focused.receipt(
        compiled.internal,
        terminal,
        0 if terminal == "succeeded" else 1,
        1.25,
        True,
        True,
        12,
        "sha256:" + "a" * 64,
        1000,
        2250,
    )


class ContractTests(unittest.TestCase):
    def test_compiles_only_semantic_fields_to_fixed_workload(self) -> None:
        request = module.decode_request(raw())
        compiled = module.compile_request(request)
        self.assertEqual(compiled.internal.profile.profile_id, "verify-focused/v1")
        self.assertEqual(compiled.internal.profile_generation, focused.profile_generation())
        self.assertEqual(compiled.internal.profile.resource_class, "big-red-focused")
        self.assertEqual(compiled.internal.profile.deadline_seconds, 600)
        self.assertEqual(compiled.internal.command_fingerprint[:7], "sha256:")

    def test_caller_refs_and_reuse_do_not_mint_execution_identity(self) -> None:
        left = module.compile_request(module.decode_request(raw()))
        changed = copy.deepcopy(BASE)
        changed["external_request_ref"] = "cmux:exec:other"
        changed["correlation"]["work_ref"] = "cmux:work:other"
        changed["reuse_hint"] = "no_preference"
        right = module.compile_request(module.decode_request(raw(changed)))
        self.assertEqual(
            left.internal.command_fingerprint, right.internal.command_fingerprint
        )
        self.assertNotEqual(left.request_sha256, right.request_sha256)

    def test_internal_semantic_identity_partitions_physical_execution(self) -> None:
        request = module.decode_request(raw())
        legacy = module.compile_request(request)
        compiled_first = module.compile_request(
            request,
            semantic_request_id="accepted-1054-request-0001",
        )
        compiled_second = module.compile_request(
            request,
            semantic_request_id="accepted-1054-request-0002",
        )
        self.assertNotEqual(
            legacy.internal.command_fingerprint,
            compiled_first.internal.command_fingerprint,
        )
        self.assertNotEqual(
            compiled_first.internal.command_fingerprint,
            compiled_second.internal.command_fingerprint,
        )
        self.assertEqual(compiled_first.request_sha256, compiled_second.request_sha256)

    def test_external_request_cannot_supply_semantic_execution_identity(self) -> None:
        request = module.decode_request(raw())
        self.assertEqual(module.request_document(request), BASE)
        widened = copy.deepcopy(BASE)
        widened["semantic_request_id"] = "accepted-1054-request-0001"
        with self.assertRaisesRegex(module.ContractRefusal, "unsupported fields"):
            module.decode_request(raw(widened))

    def test_rejects_caller_workspace_and_execution_controls(self) -> None:
        forbidden = {
            "cwd": "/tmp/x",
            "argv": ["sh", "-c", "id"],
            "environment": {"A": "B"},
            "machine": "big-red",
            "backend": "linux",
            "systemd_properties": ["MemoryMax=99G"],
            "profile_generation": "sha256:" + "0" * 64,
        }
        for field, value in forbidden.items():
            with self.subTest(field=field):
                document = copy.deepcopy(BASE)
                document[field] = value
                with self.assertRaisesRegex(module.ContractRefusal, "unsupported fields"):
                    module.decode_request(raw(document))

    def test_unsupported_operation_and_capability_are_bounded_refusals(self) -> None:
        operation = copy.deepcopy(BASE)
        operation["operation"] = "shell"
        receipt = module.plan_from_bytes(raw(operation))
        self.assertEqual(
            (receipt["state"], receipt["refusal_code"]),
            ("refused", "unsupported_operation"),
        )

        capability = copy.deepcopy(BASE)
        capability["requested_capability_class"] = "operator_equivalent"
        receipt = module.plan_from_bytes(raw(capability))
        self.assertEqual(
            (receipt["state"], receipt["refusal_code"]),
            ("refused", "unsupported_capability"),
        )

    def test_planned_receipt_is_bounded_and_grants_zero_authority(self) -> None:
        receipt = module.plan_from_bytes(raw())
        self.assertEqual(receipt["state"], "planned")
        self.assertEqual(receipt["resolved_workload"]["id"], "verify-focused/v1")
        self.assertEqual(receipt["authority"], module.AUTHORITY)
        encoded = module.canonical_bytes(receipt) + b"\n"
        self.assertLessEqual(len(encoded), module.MAX_RECEIPT_BYTES)
        for forbidden in (
            b"/tmp/",
            b"systemd",
            b"big-red",
            b"command_fingerprint",
        ):
            self.assertNotIn(forbidden, encoded)

    def test_terminal_receipt_wraps_only_matching_typed_workload_receipt(self) -> None:
        compiled = module.compile_request(module.decode_request(raw()))
        receipt = module.terminal_receipt(compiled, internal_receipt(compiled))
        self.assertEqual(receipt["state"], "succeeded")
        self.assertRegex(
            receipt["workload_receipt_sha256"], r"^sha256:[a-f0-9]{64}$"
        )

        drifted = internal_receipt(compiled)
        drifted["source"]["tree"] = "f" * 40
        with self.assertRaisesRegex(module.ContractRefusal, "does not match"):
            module.terminal_receipt(compiled, drifted)

    def test_ambiguous_state_never_authorizes_redispatch(self) -> None:
        compiled = module.compile_request(module.decode_request(raw()))
        receipt = module.ambiguous_receipt(compiled)
        self.assertEqual(
            (receipt["state"], receipt["refusal_code"]),
            ("ambiguous", "ambiguous_execution"),
        )
        self.assertFalse(receipt["authority"]["authorizes_redispatch"])

    def test_exact_replay_returns_same_receipt_and_drift_conflicts(self) -> None:
        request = module.decode_request(raw())
        existing = module.planned_receipt(module.compile_request(request))
        self.assertIs(module.validate_replay(existing, request), existing)

        drift = copy.deepcopy(BASE)
        drift["source"]["commit"] = "a" * 40
        with self.assertRaisesRegex(module.ContractRefusal, "different semantics"):
            module.validate_replay(existing, module.decode_request(raw(drift)))

    def test_replay_rejects_state_semantic_corruption(self) -> None:
        request = module.decode_request(raw())
        compiled = module.compile_request(request)

        planned = module.planned_receipt(compiled)
        corrupted = copy.deepcopy(planned)
        corrupted["state"] = "succeeded"
        with self.assertRaisesRegex(module.ContractRefusal, "state fields"):
            module.validate_replay(corrupted, request)

        terminal = module.terminal_receipt(compiled, internal_receipt(compiled))
        corrupted = copy.deepcopy(terminal)
        corrupted["workload_receipt_sha256"] = None
        with self.assertRaisesRegex(module.ContractRefusal, "state fields"):
            module.validate_replay(corrupted, request)

        refused = module.refused_receipt(
            request,
            module.ContractRefusal("unsupported_capability", "refused"),
        )
        corrupted = copy.deepcopy(refused)
        corrupted["resolved_workload"] = {
            "id": "verify-focused/v1",
            "generation": focused.profile_generation(),
            "capability_class": focused.EXECUTION_IDENTITY_CLASS,
        }
        with self.assertRaisesRegex(module.ContractRefusal, "state fields"):
            module.validate_replay(corrupted, request)


    def test_replay_rejects_wrong_resolved_generation_and_oversized_receipt(self) -> None:
        request = module.decode_request(raw())
        receipt = module.planned_receipt(module.compile_request(request))
        drift = copy.deepcopy(receipt)
        drift["resolved_workload"]["generation"] = "sha256:" + "f" * 64
        with self.assertRaisesRegex(module.ContractRefusal, "state fields"):
            module.validate_replay(drift, request)

        oversized = copy.deepcopy(receipt)
        oversized["refusal_code"] = "x" * module.MAX_RECEIPT_BYTES
        with self.assertRaisesRegex(module.ContractRefusal, "fixed ceiling"):
            module.validate_receipt(oversized)


    def test_repository_fixture_round_trip(self) -> None:
        root = Path(__file__).resolve().parents[1]
        request_path = (
            root / "docs/experiments/external-execution-request/cmux-request.json"
        )
        result_path = (
            root / "docs/experiments/external-execution-request/glaeda-result.json"
        )
        completed = subprocess.run(
            [
                sys.executable,
                str(root / "scripts/external_execution_request.py"),
                "plan",
            ],
            input=request_path.read_bytes(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        self.assertEqual(completed.stdout, result_path.read_bytes())
        self.assertEqual(completed.stderr, b"")


if __name__ == "__main__":
    unittest.main()
