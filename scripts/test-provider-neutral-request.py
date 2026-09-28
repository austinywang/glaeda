#!/usr/bin/env python3
"""Contract tests for the provider-neutral owner-local semantic request."""

from __future__ import annotations

import copy
import json
import unittest

import provider_neutral_request as module
import verify_focused_impl as focused


SOURCE = {
    "repository": "teamleaderleo/glaeda",
    "commit": "1" * 40,
    "tree": "2" * 40,
}


def document(
    *,
    request_id: str = "owner-1012-request-0001",
    operation: str = "verify_named",
    source: dict[str, object] | None = SOURCE,
    parameters: dict[str, object] | None = None,
) -> dict[str, object]:
    if parameters is None:
        parameters = {"profile_id": "verify-focused/v1"}
    return {
        "document_type": "glaeda-semantic-request",
        "schema_version": 1,
        "request_id": request_id,
        "operation": operation,
        "source": source,
        "parameters": parameters,
    }


def raw(value: dict[str, object]) -> bytes:
    return module.canonical_bytes(value) + b"\n"


def compiled_verify(request_id: str = "owner-1012-request-0001") -> module.CompiledRequest:
    request = module.decode_request(raw(document(request_id=request_id)))
    return module.compile_request(request)


def repo_query_report(compiled: module.CompiledRequest) -> dict[str, object]:
    patch_text = "diff\n"
    return {
        "document_type": "glaeda-resident-repo-query",
        "schema_version": 1,
        "profile_id": "repo-query/v1",
        "profile_generation": compiled.resolved_operation["profile_generation"],
        "authority": "observation_only",
        "request_digest": compiled.resolved_operation["request_digest"],
        "repository": "github.com/" + SOURCE["repository"],
        "object_format": "sha1",
        "requested_base": compiled.resolved_operation["base_commit"],
        "head": SOURCE["commit"],
        "head_tree": SOURCE["tree"],
        "merge_base": "4" * 40,
        "base_is_ancestor": True,
        "commits_since_merge_base": 1,
        "changed_files": [
            {
                "path": "src/lib.rs",
                "insertions": 2,
                "deletions": 0,
                "binary": False,
            }
        ],
        "changed_files_status": "complete",
        "changed_files_observed": 1,
        "changed_files_omitted": 0,
        "diff_summary": {
            "files_changed": 1,
            "text_files": 1,
            "binary_files": 0,
            "insertions": 2,
            "deletions": 0,
        },
        "patch": {
            "bytes": len(patch_text.encode("utf-8")),
            "sha256": module.sha256(patch_text.encode("utf-8")),
            "included": True,
            "omitted_bytes": 0,
            "text": patch_text,
        },
        "blobs": [],
        "path_history": [],
        "objects": [],
        "metrics": {
            "git_processes": 13,
            "git_stdout_bytes": 100,
            "git_wall_microseconds": 200,
            "complete_wall_microseconds": 300,
        },
    }


def internal_receipt(
    compiled: module.CompiledRequest, terminal: str = "succeeded"
) -> dict[str, object]:
    assert compiled.internal is not None
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


class ProviderNeutralRequestTests(unittest.TestCase):
    def test_closed_first_operation_vocabulary(self) -> None:
        for operation, source, parameters in (
            ("capabilities", None, {}),
            ("status", None, {}),
            (
                "repo_query",
                SOURCE,
                {"base_commit": "3" * 40, "max_patch_bytes": 4096},
            ),
            ("verify_named", SOURCE, {"profile_id": "verify-focused/v1"}),
        ):
            with self.subTest(operation=operation):
                request = module.decode_request(
                    raw(
                        document(
                            operation=operation,
                            source=source,
                            parameters=parameters,
                        )
                    )
                )
                compiled = module.compile_request(request)
                self.assertEqual(compiled.resolved_operation["kind"], operation)

        unsupported = document(operation="command_argv", source=SOURCE, parameters={})
        with self.assertRaisesRegex(module.ContractRefusal, "unsupported"):
            module.decode_request(raw(unsupported))

    def test_read_only_node_operations_carry_no_source_or_parameters(self) -> None:
        for operation in ("capabilities", "status"):
            bad = document(operation=operation, source=SOURCE, parameters={})
            with self.subTest(operation=operation):
                with self.assertRaisesRegex(module.ContractRefusal, "unexpected source"):
                    module.decode_request(raw(bad))

    def test_verify_named_resolves_only_reviewed_focused_profile(self) -> None:
        compiled = compiled_verify()
        self.assertEqual(compiled.resolved_operation["profile_id"], "verify-focused/v1")
        self.assertEqual(compiled.resolved_operation["resource_class"], "big-red-focused")
        self.assertEqual(compiled.resolved_operation["network_class"], "none")
        self.assertEqual(compiled.resolved_operation["environment_class"], "minimal")
        self.assertEqual(compiled.resolved_operation["deadline_seconds"], 600)
        self.assertIsNotNone(compiled.internal)

        bad = module.decode_request(
            raw(
                document(
                    parameters={"profile_id": "verify-required/v1"},
                )
            )
        )
        with self.assertRaisesRegex(module.ContractRefusal, "not admitted"):
            module.compile_request(bad)

    def test_explicit_semantic_request_identity_partitions_physical_execution(self) -> None:
        first = compiled_verify("owner-1012-request-0001")
        second = compiled_verify("owner-1012-request-0002")
        assert first.internal is not None and second.internal is not None
        self.assertNotEqual(
            first.internal.command_fingerprint,
            second.internal.command_fingerprint,
        )
        self.assertNotEqual(first.request_sha256, second.request_sha256)

    def test_capabilities_are_bounded_and_advertise_no_argv_surface(self) -> None:
        request = module.decode_request(
            raw(document(operation="capabilities", source=None, parameters={}))
        )
        receipt = module.capabilities_receipt(module.compile_request(request))
        payload = receipt["result"]
        self.assertEqual(payload["document_type"], "glaeda-semantic-capabilities")
        self.assertFalse(payload["command_argv"]["available"])
        self.assertEqual(receipt["authority"], module.AUTHORITY)
        self.assertLessEqual(
            len(module.canonical_bytes(receipt) + b"\n"),
            module.MAX_RECEIPT_BYTES,
        )

    def test_status_wraps_only_existing_owned_admission_observation(self) -> None:
        request = module.decode_request(
            raw(document(operation="status", source=None, parameters={}))
        )
        compiled = module.compile_request(request)
        observation = {
            "document_type": "glaeda-owned-admission-observation",
            "schema_version": 1,
            "outcome": "wait",
            "reason": "node_draining",
            "grants_authority": False,
            "authorizes_execution": False,
            "authorizes_redispatch": False,
        }
        receipt = module.status_receipt(compiled, observation)
        self.assertEqual(receipt["state"], "succeeded")
        self.assertEqual(receipt["result"]["reason"], "node_draining")

        unknown = dict(observation)
        unknown["reason"] = "scheduler_says_yes"
        with self.assertRaisesRegex(module.ContractRefusal, "outside the reviewed"):
            module.status_receipt(compiled, unknown)

        bad = dict(observation)
        bad["authorizes_execution"] = True
        with self.assertRaisesRegex(module.ContractRefusal, "outside the reviewed"):
            module.status_receipt(compiled, bad)

    def test_repo_query_wraps_exact_reviewed_profile_and_projects_closed_result(self) -> None:
        request = module.decode_request(
            raw(
                document(
                    operation="repo_query",
                    source=SOURCE,
                    parameters={"base_commit": "3" * 40, "max_patch_bytes": 4096},
                )
            )
        )
        compiled = module.compile_request(request)
        self.assertEqual(
            compiled.resolved_operation["profile_generation"],
            "sha256:f575e0e3cd40e54ca4f868f99777e40386a2fe909cb91362f777e8881302ef65",
        )
        self.assertEqual(
            compiled.resolved_operation["request_digest"],
            "sha256:8b8f146edc28989d4d1d1f986a40f986c951d60f57c6af19b0e7ccdcbf692cd8",
        )
        report = repo_query_report(compiled)
        receipt = module.repo_query_receipt(compiled, report)
        self.assertEqual(receipt["state"], "succeeded")
        self.assertEqual(
            receipt["result"]["document_type"],
            module.REPO_QUERY_RESULT_DOCUMENT_TYPE,
        )
        self.assertEqual(receipt["result"]["repository"], SOURCE["repository"])
        self.assertEqual(
            receipt["result"]["request_digest"],
            compiled.resolved_operation["request_digest"],
        )
        self.assertNotIn("blobs", receipt["result"])
        self.assertNotIn("private_path", receipt["result"])

        for field, value in (
            ("head", "5" * 40),
            ("profile_generation", "sha256:" + "b" * 64),
            ("request_digest", "sha256:" + "c" * 64),
        ):
            with self.subTest(field=field):
                drift = copy.deepcopy(report)
                drift[field] = value
                with self.assertRaisesRegex(module.ContractRefusal, "does not match"):
                    module.repo_query_receipt(compiled, drift)

        unexpected = copy.deepcopy(report)
        unexpected["private_path"] = "/home/owner/project"
        with self.assertRaisesRegex(module.ContractRefusal, "does not match"):
            module.repo_query_receipt(compiled, unexpected)


    def test_verify_terminal_projection_uses_existing_typed_receipt(self) -> None:
        compiled = compiled_verify()
        receipt = module.terminal_verify_receipt(compiled, internal_receipt(compiled))
        self.assertEqual(receipt["state"], "succeeded")
        self.assertEqual(receipt["result"]["terminal_class"], "succeeded")
        self.assertTrue(receipt["result"]["process_tree_settled"])
        self.assertRegex(
            receipt["result"]["workload_receipt_sha256"],
            r"^sha256:[a-f0-9]{64}$",
        )

        drift = internal_receipt(compiled)
        drift["source"]["tree"] = "f" * 40
        with self.assertRaisesRegex(module.ContractRefusal, "does not match"):
            module.terminal_verify_receipt(compiled, drift)

    def test_wait_and_ambiguity_never_authorize_redispatch(self) -> None:
        compiled = compiled_verify()
        waiting = module.waiting_receipt(compiled, "pressure_high")
        ambiguous = module.ambiguous_receipt(compiled)
        self.assertEqual(waiting["state"], "waiting")
        self.assertEqual(ambiguous["state"], "ambiguous")
        self.assertFalse(waiting["authority"]["authorizes_redispatch"])
        self.assertFalse(ambiguous["authority"]["authorizes_redispatch"])

    def test_exact_request_replay_and_conflict_are_explicit(self) -> None:
        request = module.decode_request(raw(document()))
        existing = module.planned_receipt(module.compile_request(request))
        self.assertIs(module.validate_replay(existing, request), existing)

        drifted = module.decode_request(
            raw(
                document(
                    parameters={"profile_id": "verify-required/v1"},
                )
            )
        )
        with self.assertRaisesRegex(module.ContractRefusal, "different semantics"):
            module.validate_replay(existing, drifted)

        different_id = module.decode_request(
            raw(document(request_id="owner-1012-request-0002"))
        )
        with self.assertRaisesRegex(module.ContractRefusal, "another request"):
            module.validate_replay(existing, different_id)

    def test_receipt_inspection_rejects_result_tampering(self) -> None:
        request = module.decode_request(
            raw(document(operation="capabilities", source=None, parameters={}))
        )
        receipt = module.capabilities_receipt(module.compile_request(request))
        inspected = module.inspect_receipt(module.canonical_bytes(receipt) + b"\n")
        self.assertEqual(inspected, receipt)

        tampered = copy.deepcopy(receipt)
        tampered["result"]["command_argv"]["available"] = True
        with self.assertRaisesRegex(module.ContractRefusal, "result digest"):
            module.inspect_receipt(module.canonical_bytes(tampered) + b"\n")

        typed_tamper = copy.deepcopy(receipt)
        typed_tamper["result"]["command_argv"]["available"] = True
        typed_tamper["result_sha256"] = module.sha256(
            module.canonical_bytes(typed_tamper["result"]) + b"\n"
        )
        with self.assertRaisesRegex(module.ContractRefusal, "capabilities result"):
            module.inspect_receipt(module.canonical_bytes(typed_tamper) + b"\n")

        digest_tamper = copy.deepcopy(receipt)
        digest_tamper["request_sha256"] = "sha256:" + "f" * 64
        with self.assertRaisesRegex(module.ContractRefusal, "does not match its request"):
            module.inspect_receipt(module.canonical_bytes(digest_tamper) + b"\n")

        drifted_resolution = copy.deepcopy(receipt)
        drifted_resolution["resolved_operation"]["kind"] = "status"
        with self.assertRaisesRegex(module.ContractRefusal, "resolution"):
            module.inspect_receipt(
                module.canonical_bytes(drifted_resolution) + b"\n"
            )

        drifted_source = copy.deepcopy(receipt)
        drifted_source["source"] = SOURCE
        with self.assertRaisesRegex(module.ContractRefusal, "unexpected source"):
            module.inspect_receipt(module.canonical_bytes(drifted_source) + b"\n")

    def test_receipt_inspection_recompiles_repo_query_resolution(self) -> None:
        request = module.decode_request(
            raw(
                document(
                    operation="repo_query",
                    source=SOURCE,
                    parameters={"base_commit": "3" * 40, "max_patch_bytes": 4096},
                )
            )
        )
        compiled = module.compile_request(request)
        receipt = module.repo_query_receipt(compiled, repo_query_report(compiled))
        tampered = copy.deepcopy(receipt)
        fake = "sha256:" + "d" * 64
        tampered["resolved_operation"]["request_digest"] = fake
        tampered["result"]["request_digest"] = fake
        tampered["result_sha256"] = module.sha256(
            module.canonical_bytes(tampered["result"]) + b"\n"
        )
        with self.assertRaisesRegex(module.ContractRefusal, "resolution does not match"):
            module.inspect_receipt(module.canonical_bytes(tampered) + b"\n")

    def test_receipt_inspection_rejects_rehashed_invalid_verify_projection(self) -> None:
        receipt = module.terminal_verify_receipt(
            compiled_verify(),
            internal_receipt(compiled_verify()),
        )
        tampered = copy.deepcopy(receipt)
        tampered["result"]["process_tree_settled"] = False
        tampered["result_sha256"] = module.sha256(
            module.canonical_bytes(tampered["result"]) + b"\n"
        )
        with self.assertRaisesRegex(module.ContractRefusal, "verification result"):
            module.inspect_receipt(module.canonical_bytes(tampered) + b"\n")

    def test_noncanonical_request_is_refused(self) -> None:
        value = document()
        pretty = (json.dumps(value, indent=2) + "\n").encode()
        with self.assertRaisesRegex(module.ContractRefusal, "canonical"):
            module.decode_request(pretty)


if __name__ == "__main__":
    unittest.main()
