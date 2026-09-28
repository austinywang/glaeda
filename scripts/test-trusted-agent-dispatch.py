#!/usr/bin/env python3
"""Contract tests for trusted-agent dispatch."""
from __future__ import annotations

import copy
import datetime as dt
import unittest

import external_execution_request as external
import trusted_agent_dispatch as dispatch

NOW = dt.datetime(2026, 9, 21, 10, 0, tzinfo=dt.timezone.utc)
PRINCIPAL = "github-mailbox-writers:teamleaderleo/glaeda-dispatch"
BINDING = "github-private-repo-write-policy:teamleaderleo/glaeda-dispatch"


def base_document() -> dict[str, object]:
    value: dict[str, object] = {
        "document_type": dispatch.REQUEST_DOCUMENT_TYPE,
        "schema_version": 1,
        "request_id": "dispatch-967-proof",
        "source": {
            "repository": "teamleaderleo/glaeda",
            "commit": "1" * 40,
            "tree": "2" * 40,
        },
        "operation": {
            "kind": "verify_named",
            "profile": "verify-focused/v1",
        },
        "caller": {
            "principal": PRINCIPAL,
            "provenance_binding": BINDING,
        },
        "created_at": "2026-09-21T09:55:00Z",
        "expires_at": "2026-09-21T10:25:00Z",
        "supersession": {"policy": "none"},
    }
    value["request_fingerprint"] = dispatch.fingerprint_document(value)
    return value


def raw(value: dict[str, object] | None = None) -> bytes:
    chosen = base_document() if value is None else value
    return dispatch.canonical_bytes(chosen) + b"\n"


def accepted(
    value: dict[str, object] | None = None,
) -> dispatch.AcceptedRequest:
    request = dispatch.decode_request(raw(value), now=NOW)
    return dispatch.accept_request(
        request,
        dispatch.ProvenanceEvidence(PRINCIPAL, BINDING),
    )


def external_terminal(
    item: dispatch.AcceptedRequest,
    state: str = "succeeded",
) -> dict[str, object]:
    compiled = external.compile_request(item.external_request)
    receipt = external.planned_receipt(compiled)
    receipt["state"] = state
    receipt["workload_receipt_sha256"] = (
        "sha256:" + "a" * 64
        if state not in {"refused", "ambiguous"}
        else None
    )
    if state == "refused":
        receipt["resolved_workload"] = None
        receipt["refusal_code"] = "fixture_refused"
    elif state == "ambiguous":
        receipt["refusal_code"] = "ambiguous_execution"
    else:
        receipt["refusal_code"] = None
    return receipt


class TrustedDispatchTests(unittest.TestCase):
    def test_accepts_exact_request_and_compiles_reviewed_workload(self) -> None:
        item = accepted()
        self.assertEqual(
            item.accepted_document["resolved_workload"]["id"],
            "verify-focused/v1",
        )
        self.assertEqual(
            item.accepted_document["authority"],
            dispatch.AUTHORITY,
        )
        self.assertEqual(
            item.accepted_document["caller"]["principal"],
            PRINCIPAL,
        )
        compiled = external.compile_request(
            item.external_request,
            semantic_request_id=item.accepted_document["semantic_request_id"],
        )
        self.assertEqual(
            item.accepted_document["workload_command_fingerprint"],
            compiled.internal.command_fingerprint,
        )
        self.assertLessEqual(
            len(dispatch.canonical_bytes(item.accepted_document) + b"\n"),
            4096,
        )

    def test_acceptance_mints_transport_scoped_semantic_identity(self) -> None:
        item = accepted()
        semantic_id = item.accepted_document["semantic_request_id"]
        self.assertRegex(semantic_id, r"^accepted-[a-f0-9]{55}$")
        self.assertEqual(item.semantic_request.request_id, semantic_id)
        self.assertEqual(
            item.accepted_document["semantic_request_sha256"],
            item.semantic_request_sha256,
        )

        changed = base_document()
        changed["request_id"] = "dispatch-967-proof-two"
        changed["request_fingerprint"] = dispatch.fingerprint_document(changed)
        other = accepted(changed)
        self.assertNotEqual(
            semantic_id,
            other.accepted_document["semantic_request_id"],
        )
        first_compiled = external.compile_request(
            item.external_request,
            semantic_request_id=semantic_id,
        )
        other_compiled = external.compile_request(
            other.external_request,
            semantic_request_id=other.accepted_document["semantic_request_id"],
        )
        self.assertNotEqual(
            first_compiled.internal.command_fingerprint,
            other_compiled.internal.command_fingerprint,
        )

    def test_same_external_id_is_partitioned_by_caller_namespace(self) -> None:
        first = accepted()
        replay = accepted()
        self.assertEqual(
            first.accepted_document["semantic_request_id"],
            replay.accepted_document["semantic_request_id"],
        )
        self.assertEqual(
            first.accepted_document["workload_command_fingerprint"],
            replay.accepted_document["workload_command_fingerprint"],
        )

        changed = base_document()
        changed["caller"] = {
            "principal": "cmux-ci:teamleaderleo/glaeda",
            "provenance_binding": "cmux-controller:reviewed-local",
        }
        changed["request_fingerprint"] = dispatch.fingerprint_document(changed)
        request = dispatch.decode_request(raw(changed), now=NOW)
        other = dispatch.accept_request(
            request,
            dispatch.ProvenanceEvidence(
                "cmux-ci:teamleaderleo/glaeda",
                "cmux-controller:reviewed-local",
            ),
        )

        self.assertEqual(
            first.request.request_id,
            other.request.request_id,
        )
        self.assertNotEqual(
            first.accepted_document["semantic_request_id"],
            other.accepted_document["semantic_request_id"],
        )
        self.assertNotEqual(
            first.accepted_document["workload_command_fingerprint"],
            other.accepted_document["workload_command_fingerprint"],
        )

    def test_same_external_id_partitions_callers_but_exact_replay_is_stable(self) -> None:
        original_document = base_document()
        first = accepted(original_document)
        replay = accepted(copy.deepcopy(original_document))
        self.assertEqual(
            first.accepted_document["semantic_request_id"],
            replay.accepted_document["semantic_request_id"],
        )
        self.assertEqual(
            first.accepted_document["workload_command_fingerprint"],
            replay.accepted_document["workload_command_fingerprint"],
        )

        other_principal = "github-mailbox-writers:teamleaderleo/other-dispatch"
        other_binding = "github-private-repo-write-policy:teamleaderleo/other-dispatch"
        other_document = copy.deepcopy(original_document)
        other_document["caller"] = {
            "principal": other_principal,
            "provenance_binding": other_binding,
        }
        other_document["request_fingerprint"] = dispatch.fingerprint_document(
            other_document
        )
        other_request = dispatch.decode_request(raw(other_document), now=NOW)
        other = dispatch.accept_request(
            other_request,
            dispatch.ProvenanceEvidence(other_principal, other_binding),
        )
        self.assertEqual(first.request.request_id, other.request.request_id)
        self.assertEqual(first.request.repository, other.request.repository)
        self.assertEqual(first.request.commit, other.request.commit)
        self.assertEqual(first.request.tree, other.request.tree)
        self.assertNotEqual(
            first.accepted_document["semantic_request_id"],
            other.accepted_document["semantic_request_id"],
        )
        self.assertNotEqual(
            first.accepted_document["workload_command_fingerprint"],
            other.accepted_document["workload_command_fingerprint"],
        )

    def test_caller_request_schema_does_not_gain_physical_identity_fields(self) -> None:
        document = base_document()
        self.assertNotIn("semantic_request_id", document)
        self.assertNotIn("backend", document)
        self.assertNotIn("argv", document)

    def test_transport_projection_gets_the_same_deterministic_identity(self) -> None:
        document = base_document()
        expected = dispatch.decode_request(raw(document), now=NOW)
        projection = dict(document)
        projection.pop("request_fingerprint")
        derived = dispatch.decode_projection(
            dispatch.canonical_bytes(projection) + b"\n",
            now=NOW,
        )
        self.assertEqual(derived, expected)

    def test_observed_provenance_is_separate_from_request_bytes(self) -> None:
        request = dispatch.decode_request(raw(), now=NOW)
        with self.assertRaisesRegex(
            dispatch.DispatchRefusal,
            "observed provenance",
        ):
            dispatch.accept_request(
                request,
                dispatch.ProvenanceEvidence(
                    "github-user:commit-author",
                    BINDING,
                ),
            )

    def test_fingerprint_detects_semantic_drift(self) -> None:
        value = base_document()
        value["source"]["commit"] = "3" * 40
        with self.assertRaisesRegex(
            dispatch.DispatchRefusal,
            "fingerprint",
        ):
            dispatch.decode_request(raw(value), now=NOW)

    def test_stale_future_malformed_and_supersession_refuse(self) -> None:
        expired = base_document()
        expired["created_at"] = "2026-09-21T08:00:00Z"
        expired["expires_at"] = "2026-09-21T08:30:00Z"
        expired["request_fingerprint"] = dispatch.fingerprint_document(expired)
        with self.assertRaisesRegex(dispatch.DispatchRefusal, "expired"):
            dispatch.decode_request(raw(expired), now=NOW)

        future = base_document()
        future["created_at"] = "2026-09-21T10:06:00Z"
        future["expires_at"] = "2026-09-21T10:30:00Z"
        future["request_fingerprint"] = dispatch.fingerprint_document(future)
        with self.assertRaisesRegex(dispatch.DispatchRefusal, "future"):
            dispatch.decode_request(raw(future), now=NOW)

        superseded = base_document()
        superseded["supersession"] = {"policy": "latest_wins"}
        superseded["request_fingerprint"] = dispatch.fingerprint_document(
            superseded
        )
        with self.assertRaisesRegex(
            dispatch.DispatchRefusal,
            "supersession",
        ):
            dispatch.decode_request(raw(superseded), now=NOW)

        with self.assertRaisesRegex(dispatch.DispatchRefusal, "valid JSON"):
            dispatch.decode_request(b"{\n", now=NOW)

    def test_request_vocabulary_rejects_shell_and_backend_fields(self) -> None:
        for field, value in {
            "argv": ["sh", "-c", "id"],
            "backend": "big-red",
            "environment": {"TOKEN": "x"},
        }.items():
            with self.subTest(field=field):
                document = base_document()
                document[field] = value
                document["request_fingerprint"] = (
                    dispatch.fingerprint_document(document)
                )
                with self.assertRaisesRegex(
                    dispatch.DispatchRefusal,
                    "unsupported fields",
                ):
                    dispatch.decode_request(raw(document), now=NOW)

    def test_launch_mark_is_required_before_terminal_and_restart_reconciles(
        self,
    ) -> None:
        item = accepted()
        initial = dispatch.initial_lifecycle(item)
        self.assertEqual(
            dispatch.restart_disposition(initial, item),
            "launch_after_durable_mark",
        )
        with self.assertRaisesRegex(dispatch.DispatchRefusal, "launch"):
            dispatch.settle_from_external(
                initial,
                item,
                external_terminal(item),
            )

        launching = dispatch.mark_launching(initial, item)
        self.assertEqual(
            dispatch.restart_disposition(launching, item),
            "reconcile_only",
        )
        self.assertIs(dispatch.mark_launching(launching, item), launching)

        settled = dispatch.settle_from_external(
            launching,
            item,
            external_terminal(item),
        )
        self.assertEqual(
            dispatch.restart_disposition(settled, item),
            "return_settlement",
        )
        with self.assertRaisesRegex(
            dispatch.DispatchRefusal,
            "another execution",
        ):
            dispatch.mark_launching(settled, item)

    def test_ambiguous_settlement_never_becomes_fresh_execution(self) -> None:
        item = accepted()
        launching = dispatch.mark_launching(
            dispatch.initial_lifecycle(item),
            item,
        )
        ambiguous = dispatch.settle_from_external(
            launching,
            item,
            external_terminal(item, "ambiguous"),
        )
        self.assertEqual(ambiguous["state"], "ambiguous")
        self.assertEqual(
            dispatch.restart_disposition(ambiguous, item),
            "reconcile_only",
        )
        self.assertFalse(
            ambiguous["authority"]["authorizes_redispatch"]
        )

    def test_restored_lifecycle_refuses_state_terminal_mismatch(self) -> None:
        item = accepted()
        launching = dispatch.mark_launching(
            dispatch.initial_lifecycle(item),
            item,
        )
        cases = [
            ("refused", "succeeded"),
            ("ambiguous", "failed"),
            ("terminal", "refused"),
            ("terminal", "ambiguous"),
        ]
        for state, terminal_class in cases:
            with self.subTest(state=state, terminal_class=terminal_class):
                value = dispatch.settle_from_external(
                    launching,
                    item,
                    external_terminal(
                        item,
                        "refused" if state == "refused" else (
                            "ambiguous" if state == "ambiguous" else "succeeded"
                        ),
                    ),
                )
                value["state"] = state
                value["terminal_class"] = terminal_class
                with self.assertRaisesRegex(
                    dispatch.DispatchRefusal,
                    "state and terminal class disagree",
                ):
                    dispatch.validate_lifecycle(value, item)
                with self.assertRaises(dispatch.DispatchRefusal):
                    dispatch.restart_disposition(value, item)
                with self.assertRaises(dispatch.DispatchRefusal):
                    dispatch.result_document(value, item)

    def test_terminal_receipt_requires_state_specific_evidence(self) -> None:
        item = accepted()
        launching = dispatch.mark_launching(
            dispatch.initial_lifecycle(item),
            item,
        )
        forged = external_terminal(item)
        forged["workload_receipt_sha256"] = None
        with self.assertRaisesRegex(
            dispatch.DispatchRefusal,
            "fields are inconsistent",
        ):
            dispatch.settle_from_external(launching, item, forged)

    def test_exact_settlement_replay_is_idempotent_and_drift_conflicts(
        self,
    ) -> None:
        item = accepted()
        launching = dispatch.mark_launching(
            dispatch.initial_lifecycle(item),
            item,
        )
        receipt = external_terminal(item)
        settled = dispatch.settle_from_external(
            launching,
            item,
            receipt,
        )
        self.assertEqual(
            dispatch.settle_from_external(settled, item, receipt),
            settled,
        )
        drift = copy.deepcopy(receipt)
        drift["state"] = "failed"
        with self.assertRaisesRegex(
            dispatch.DispatchRefusal,
            "different receipt",
        ):
            dispatch.settle_from_external(settled, item, drift)

    def test_bounded_result_contains_no_logs_paths_environment_or_credentials(
        self,
    ) -> None:
        item = accepted()
        launching = dispatch.mark_launching(
            dispatch.initial_lifecycle(item),
            item,
        )
        settled = dispatch.settle_from_external(
            launching,
            item,
            external_terminal(item),
        )
        result = dispatch.result_document(settled, item)
        encoded = dispatch.canonical_bytes(result) + b"\n"
        self.assertLessEqual(len(encoded), 4096)
        self.assertFalse(result["contains_credentials"])
        self.assertFalse(result["contains_private_content"])
        self.assertEqual(set(result), dispatch.RESULT_KEYS)
        for forbidden in (
            b"/home/",
            b"stderr",
            b"stdout",
            b"environment",
            b"TOKEN",
        ):
            self.assertNotIn(forbidden, encoded)

    def test_accepted_identity_cannot_change_across_restart(self) -> None:
        item = accepted()
        existing = copy.deepcopy(item.accepted_document)
        self.assertEqual(
            dispatch.validate_accepted(existing, item),
            existing,
        )
        existing["source"]["tree"] = "f" * 40
        with self.assertRaisesRegex(
            dispatch.DispatchRefusal,
            "changed",
        ):
            dispatch.validate_accepted(existing, item)


if __name__ == "__main__":
    unittest.main()
