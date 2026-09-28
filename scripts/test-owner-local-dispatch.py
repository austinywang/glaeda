#!/usr/bin/env python3
"""Contract tests for the owner-local provider-neutral dispatch adapter."""

from __future__ import annotations

import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest import mock

import owner_local_dispatch as local
import provider_neutral_request as semantic
import verify_focused_impl as focused


SOURCE = {
    "repository": "teamleaderleo/glaeda",
    "commit": "1" * 40,
    "tree": "2" * 40,
}


def raw_request(
    operation: str,
    *,
    request_id: str = "owner-local-request-0001",
    source: dict[str, str] | None = None,
    parameters: dict[str, object] | None = None,
) -> bytes:
    return semantic.canonical_bytes(
        {
            "document_type": semantic.REQUEST_DOCUMENT_TYPE,
            "schema_version": 1,
            "request_id": request_id,
            "operation": operation,
            "source": source,
            "parameters": {} if parameters is None else parameters,
        }
    ) + b"\n"


def request(
    operation: str,
    *,
    request_id: str = "owner-local-request-0001",
    source: dict[str, str] | None = None,
    parameters: dict[str, object] | None = None,
) -> semantic.SemanticRequest:
    return semantic.decode_request(
        raw_request(
            operation,
            request_id=request_id,
            source=source,
            parameters=parameters,
        )
    )


def admission(outcome: str = "ready", reason: str = "compatible") -> dict[str, object]:
    return {
        "document_type": "glaeda-owned-admission-observation",
        "schema_version": 1,
        "outcome": outcome,
        "reason": reason,
        "grants_authority": False,
        "authorizes_execution": False,
        "authorizes_redispatch": False,
    }


def verify_request(request_id: str = "owner-local-request-0001") -> semantic.SemanticRequest:
    return request(
        semantic.OP_VERIFY_NAMED,
        request_id=request_id,
        source=SOURCE,
        parameters={"profile_id": "verify-focused/v1"},
    )


def repo_query_request() -> semantic.SemanticRequest:
    return request(
        semantic.OP_REPO_QUERY,
        source=SOURCE,
        parameters={"base_commit": "3" * 40, "max_patch_bytes": 4096},
    )


def repo_query_report(compiled: semantic.CompiledRequest) -> dict[str, object]:
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
            "sha256": semantic.sha256(patch_text.encode("utf-8")),
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


def workload_receipt(compiled: semantic.CompiledRequest) -> dict[str, object]:
    assert compiled.internal is not None
    return focused.receipt(
        compiled.internal,
        "succeeded",
        0,
        1.25,
        True,
        True,
        12,
        "sha256:" + "a" * 64,
        1000,
        2250,
    )


class InstallationFixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="owner-local-install-")
        self.root = Path(self.temporary.name).resolve()
        self.checkout = self.root / "checkout"
        self.checkout.mkdir()
        self.query_program = self.root / "glaeda-repo-query"
        self.query_program.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.query_program.chmod(0o700)
        self.path = self.root / "owner-local-v1.json"
        self.write()

    def write(self, *, repository: str = "teamleaderleo/glaeda") -> None:
        document = {
            "document_type": local.INSTALLATION_DOCUMENT_TYPE,
            "schema_version": 1,
            "repositories": [
                {
                    "repository": repository,
                    "checkout": os.fspath(self.checkout),
                }
            ],
        }
        self.path.write_bytes(local.canonical_bytes(document) + b"\n")
        self.path.chmod(0o600)

    def close(self) -> None:
        self.temporary.cleanup()


class OwnerLocalDispatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.installation = InstallationFixture()
        self.addCleanup(self.installation.close)
        self.semantic_state = self.installation.root / "provider-neutral-verify-v1"
        state_patch = mock.patch.object(
            local,
            "SHARED_VERIFY_STATE_ROOT",
            self.semantic_state,
        )
        state_patch.start()
        self.addCleanup(state_patch.stop)

    def test_capabilities_need_no_installation_and_advertise_no_argv(self) -> None:
        semantic_request = request(semantic.OP_CAPABILITIES)
        with mock.patch.object(
            local,
            "load_installation",
            side_effect=AssertionError("capabilities must not read installation"),
        ):
            receipt = local.execute(
                semantic_request,
                self.installation.root / "missing.json",
            )
        self.assertEqual(receipt["state"], "succeeded")
        self.assertFalse(receipt["result"]["command_argv"]["available"])

    def test_status_needs_no_repository_binding(self) -> None:
        semantic_request = request(semantic.OP_STATUS)
        with (
            mock.patch.object(
                local,
                "load_installation",
                side_effect=AssertionError("status must not read installation"),
            ),
            mock.patch.object(
                local,
                "observe_admission",
                return_value=admission("wait", "node_draining"),
            ),
        ):
            receipt = local.execute(
                semantic_request,
                self.installation.root / "missing.json",
            )
        self.assertEqual(receipt["state"], "succeeded")
        self.assertEqual(receipt["result"]["reason"], "node_draining")

    def test_private_installation_is_closed_and_owner_only(self) -> None:
        loaded = local.load_installation(self.installation.path)
        self.assertEqual(
            loaded.checkout_for("teamleaderleo/glaeda"),
            self.installation.checkout,
        )
        self.installation.path.chmod(0o644)
        with self.assertRaisesRegex(local.LocalRefusal, "installation file"):
            local.load_installation(self.installation.path)

    def test_installation_rejects_writable_checkout_and_query_program(self) -> None:
        self.installation.checkout.chmod(0o777)
        with self.assertRaisesRegex(local.LocalRefusal, "owner-controlled directory"):
            local.load_installation(self.installation.path)
        self.installation.checkout.chmod(0o755)

        self.installation.query_program.chmod(0o722)
        with mock.patch.object(
            local,
            "REPO_QUERY_PROGRAM",
            self.installation.query_program,
        ):
            with self.assertRaisesRegex(local.LocalRefusal, "owned executable"):
                local.installed_repo_query_program()

    def test_repo_query_uses_only_installed_checkout_and_fixed_argv(self) -> None:
        semantic_request = repo_query_request()
        compiled = semantic.compile_request(semantic_request)
        report = repo_query_report(compiled)
        completed = subprocess.CompletedProcess(
            [],
            0,
            semantic.canonical_bytes(report) + b"\n",
            b"",
        )
        installation = local.load_installation(self.installation.path)
        with (
            mock.patch.object(local, "REPO_QUERY_PROGRAM", self.installation.query_program),
            mock.patch.object(local, "_run", return_value=completed) as run,
        ):
            receipt = local.run_repo_query(compiled, installation)
        argv = run.call_args.args[0]
        self.assertEqual(argv[0], os.fspath(self.installation.query_program))
        self.assertEqual(
            argv[argv.index("--checkout") + 1],
            os.fspath(self.installation.checkout),
        )
        self.assertEqual(
            argv[argv.index("--project") + 1],
            "github.com/teamleaderleo/glaeda",
        )
        self.assertNotIn("sh", argv)
        self.assertNotIn("bash", argv)
        self.assertEqual(receipt["state"], "succeeded")

    def test_verify_waits_for_owned_admission_without_launch(self) -> None:
        semantic_request = verify_request()
        with (
            mock.patch.object(
                local,
                "observe_admission",
                return_value=admission("wait", "pressure_high"),
            ),
            mock.patch.object(local, "_run") as run,
        ):
            receipt = local.execute(semantic_request, self.installation.path)
        self.assertEqual(receipt["state"], "waiting")
        self.assertEqual(receipt["result"], {"reason": "pressure_high"})
        run.assert_not_called()
        self.assertFalse(receipt["authority"]["authorizes_redispatch"])

    def test_wait_reasons_cover_draining_pressure_hold_and_capacity(self) -> None:
        compiled = semantic.compile_request(verify_request())
        for reason in (
            "node_held",
            "node_draining",
            "pressure_high",
            "capacity_unavailable",
            "reserved",
            "source_cold",
        ):
            with self.subTest(reason=reason):
                receipt = semantic.waiting_receipt(compiled, reason)
                self.assertEqual(receipt["state"], "waiting")
                self.assertFalse(receipt["authority"]["authorizes_execution"])
                self.assertFalse(receipt["authority"]["authorizes_redispatch"])

    def test_verify_uses_shared_provider_neutral_state_and_resolved_fingerprint(self) -> None:
        semantic_request = verify_request()
        compiled = semantic.compile_request(semantic_request)
        receipt_document = workload_receipt(compiled)
        completed = subprocess.CompletedProcess(
            [],
            0,
            semantic.canonical_bytes(receipt_document) + b"\n",
            b"",
        )
        with (
            mock.patch.object(local, "observe_admission", return_value=admission()),
            mock.patch.object(local, "_run", return_value=completed) as run,
        ):
            receipt = local.execute(semantic_request, self.installation.path)
        argv = run.call_args.args[0]
        self.assertEqual(
            argv[argv.index("--state-root") + 1],
            os.fspath(local.SHARED_VERIFY_STATE_ROOT),
        )
        self.assertEqual(
            argv[argv.index("--command-fingerprint") + 1],
            compiled.internal.command_fingerprint,
        )
        self.assertEqual(
            argv[argv.index("--semantic-request-id") + 1],
            semantic_request.request_id,
        )
        self.assertEqual(
            argv[argv.index("--admission-root") + 1],
            os.fspath(local.ADMISSION_ROOT),
        )
        self.assertEqual(receipt["state"], "succeeded")
        self.assertEqual(receipt["result"]["terminal_class"], "succeeded")

    def test_ambiguous_prior_execution_never_redispatches(self) -> None:
        semantic_request = verify_request()
        error = {
            "document_type": "glaeda-verify-focused-error",
            "schema_version": 1,
            "authority": "none",
            "problem": local.AMBIGUOUS_PROBLEM,
        }
        completed = subprocess.CompletedProcess(
            [],
            75,
            b"",
            semantic.canonical_bytes(error) + b"\n",
        )
        with (
            mock.patch.object(local, "observe_admission", return_value=admission()),
            mock.patch.object(local, "_run", return_value=completed),
        ):
            receipt = local.execute(semantic_request, self.installation.path)
        self.assertEqual(receipt["state"], "ambiguous")
        self.assertEqual(receipt["refusal_code"], "ambiguous_execution")
        self.assertFalse(receipt["authority"]["authorizes_redispatch"])

    def test_missing_source_binding_refuses_without_fallback(self) -> None:
        self.installation.write(repository="other/project")
        with mock.patch.object(local, "_run") as run:
            receipt = local.execute(repo_query_request(), self.installation.path)
        self.assertEqual(receipt["state"], "refused")
        self.assertEqual(receipt["refusal_code"], "source_unavailable")
        run.assert_not_called()

    def test_waiting_request_binds_identity_and_drift_refuses_before_launch(self) -> None:
        first = verify_request("owner-local-request-0001")
        with (
            mock.patch.object(
                local,
                "observe_admission",
                return_value=admission("wait", "node_held"),
            ),
            mock.patch.object(local, "_run") as run,
        ):
            waiting = local.execute(first, self.installation.path)
        self.assertEqual(waiting["state"], "waiting")
        run.assert_not_called()

        drifted_source = dict(SOURCE)
        drifted_source["commit"] = "4" * 40
        drifted = request(
            semantic.OP_VERIFY_NAMED,
            request_id="owner-local-request-0001",
            source=drifted_source,
            parameters={"profile_id": "verify-focused/v1"},
        )
        with (
            mock.patch.object(
                local,
                "observe_admission",
                side_effect=AssertionError("conflict must refuse before admission"),
            ),
            mock.patch.object(local, "_run") as run,
        ):
            refused = local.execute(drifted, self.installation.path)
        self.assertEqual(refused["state"], "refused")
        self.assertEqual(refused["refusal_code"], "request_conflict")
        run.assert_not_called()

    def test_exact_waiting_replay_keeps_one_semantic_binding(self) -> None:
        semantic_request = verify_request("owner-local-request-0001")
        with mock.patch.object(
            local,
            "observe_admission",
            return_value=admission("wait", "pressure_high"),
        ):
            first = local.execute(semantic_request, self.installation.path)
            second = local.execute(semantic_request, self.installation.path)
        self.assertEqual(first["state"], "waiting")
        self.assertEqual(second["state"], "waiting")
        binding = (
            self.semantic_state
            / "semantic-requests"
            / semantic_request.request_id
            / "binding.json"
        )
        self.assertTrue(binding.is_file())

    def test_duplicate_identity_is_shared_by_local_and_semantic_workload(self) -> None:
        first = semantic.compile_request(verify_request("owner-local-request-0001"))
        second = semantic.compile_request(verify_request("owner-local-request-0001"))
        third = semantic.compile_request(verify_request("owner-local-request-0002"))
        self.assertEqual(
            first.internal.command_fingerprint,
            second.internal.command_fingerprint,
        )
        self.assertNotEqual(
            first.internal.command_fingerprint,
            third.internal.command_fingerprint,
        )


if __name__ == "__main__":
    unittest.main()
