#!/usr/bin/env python3
"""Transport-independent trusted-agent dispatch contract for one reviewed Glaeda workload.

The request carries exact source identity, one closed semantic operation, caller/provenance
correlation, and a bounded lifetime. Caller bytes never grant execution authority. Acceptance
requires separately observed provenance evidence, then compiles through the provider-neutral Glaeda
semantic request and the caller-neutral external workload adapter. Physical attempt identity,
backend, resources, reusable-state eligibility,
process lifecycle, and recovery remain Glaeda-owned.
"""
from __future__ import annotations

import argparse
import datetime as dt
from dataclasses import dataclass
import json
import re
import sys
from typing import NoReturn

import external_execution_request as external
import provider_neutral_request as semantic

REQUEST_DOCUMENT_TYPE = "glaeda-trusted-agent-dispatch-request"
ACCEPTED_DOCUMENT_TYPE = "glaeda-trusted-agent-dispatch-accepted"
LIFECYCLE_DOCUMENT_TYPE = "glaeda-trusted-agent-dispatch-lifecycle"
RESULT_DOCUMENT_TYPE = "glaeda-trusted-agent-dispatch-result"
SCHEMA_VERSION = 1
MAX_REQUEST_BYTES = 4096
MAX_DOCUMENT_BYTES = 4096
MAX_REFERENCE_BYTES = 160
MAX_LIFETIME = dt.timedelta(hours=1)
MAX_FUTURE_SKEW = dt.timedelta(minutes=5)
OPERATION_KIND = "verify_named"
PROFILE_ID = "verify-focused/v1"
SUPERSESSION_POLICY = "none"
AUTHORITY = {
    "authorizes_execution": False,
    "authorizes_redispatch": False,
    "authorizes_host_selection": False,
    "authorizes_cleanup": False,
}
TOKEN_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,159}$")
REQUEST_ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{7,63}$")
REPOSITORY_PATTERN = re.compile(r"^[a-z0-9_.-]+/[a-z0-9_.-]+$")
OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
REQUEST_KEYS = {
    "document_type",
    "schema_version",
    "request_id",
    "source",
    "operation",
    "caller",
    "created_at",
    "expires_at",
    "supersession",
    "request_fingerprint",
}
SOURCE_KEYS = {"repository", "commit", "tree"}
OPERATION_KEYS = {"kind", "profile"}
CALLER_KEYS = {"principal", "provenance_binding"}
SUPERSESSION_KEYS = {"policy"}
PROJECTION_KEYS = REQUEST_KEYS - {"request_fingerprint"}
ACCEPTED_KEYS = {
    "document_type",
    "schema_version",
    "request_id",
    "request_fingerprint",
    "source",
    "operation",
    "caller",
    "created_at",
    "expires_at",
    "supersession",
    "semantic_request_id",
    "semantic_request_sha256",
    "external_request_sha256",
    "workload_command_fingerprint",
    "resolved_workload",
    "authority",
}
LIFECYCLE_KEYS = {
    "document_type",
    "schema_version",
    "request_id",
    "request_fingerprint",
    "state",
    "terminal_class",
    "external_receipt_sha256",
    "authority",
}
RESULT_KEYS = {
    "document_type",
    "schema_version",
    "request_id",
    "request_fingerprint",
    "source",
    "operation",
    "caller_principal",
    "semantic_request_id",
    "state",
    "terminal_class",
    "external_receipt_sha256",
    "contains_credentials",
    "contains_private_content",
    "authority",
}
TERMINAL_EXTERNAL_STATES = {
    "refused",
    "ambiguous",
    "succeeded",
    "failed",
    "timed_out",
    "cleanup_incomplete",
}


class DispatchRefusal(ValueError):
    """A bounded contract refusal."""

    def __init__(self, code: str, problem: str):
        super().__init__(problem)
        self.code = code
        self.problem = problem


@dataclass(frozen=True)
class ProvenanceEvidence:
    principal: str
    binding: str


@dataclass(frozen=True)
class DispatchRequest:
    request_id: str
    repository: str
    commit: str
    tree: str
    operation_kind: str
    profile: str
    caller_principal: str
    provenance_binding: str
    created_at: str
    expires_at: str
    supersession_policy: str
    request_fingerprint: str


@dataclass(frozen=True)
class AcceptedRequest:
    request: DispatchRequest
    semantic_request: semantic.SemanticRequest
    semantic_request_sha256: str
    external_request: external.ExternalRequest
    external_request_sha256: str
    accepted_document: dict[str, object]


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return external.sha256(value)


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def _token(value: object, label: str) -> str:
    if (
        not isinstance(value, str)
        or len(value.encode("utf-8")) > MAX_REFERENCE_BYTES
        or TOKEN_PATTERN.fullmatch(value) is None
    ):
        raise DispatchRefusal("invalid_request", f"{label} is invalid")
    return value


def _parse_utc(value: object, label: str) -> dt.datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise DispatchRefusal("invalid_request", f"{label} must be a UTC timestamp")
    try:
        parsed = dt.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise DispatchRefusal("invalid_request", f"{label} must be a UTC timestamp") from error
    if parsed.tzinfo != dt.timezone.utc:
        raise DispatchRefusal("invalid_request", f"{label} must be UTC")
    return parsed


def identity_document(request: DispatchRequest | dict[str, object]) -> dict[str, object]:
    if isinstance(request, DispatchRequest):
        return {
            "document_type": REQUEST_DOCUMENT_TYPE,
            "schema_version": SCHEMA_VERSION,
            "request_id": request.request_id,
            "source": {
                "repository": request.repository,
                "commit": request.commit,
                "tree": request.tree,
            },
            "operation": {"kind": request.operation_kind, "profile": request.profile},
            "caller": {
                "principal": request.caller_principal,
                "provenance_binding": request.provenance_binding,
            },
            "created_at": request.created_at,
            "expires_at": request.expires_at,
            "supersession": {"policy": request.supersession_policy},
        }
    value = dict(request)
    value.pop("request_fingerprint", None)
    return value


def fingerprint_document(value: dict[str, object]) -> str:
    return sha256(canonical_bytes(identity_document(value)))


def decode_request(
    raw: bytes,
    *,
    now: dt.datetime | None = None,
    allow_expired: bool = False,
) -> DispatchRequest:
    if len(raw) > MAX_REQUEST_BYTES:
        raise DispatchRefusal("oversized_request", "dispatch request exceeds its fixed ceiling")
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise DispatchRefusal("invalid_request", "dispatch request is not valid JSON") from error
    if not isinstance(value, dict) or set(value) != REQUEST_KEYS:
        raise DispatchRefusal("invalid_request", "dispatch request has unsupported fields")
    if canonical_bytes(value) + b"\n" != raw:
        raise DispatchRefusal("noncanonical_request", "dispatch request is not canonical JSON")
    if (
        value["document_type"] != REQUEST_DOCUMENT_TYPE
        or type(value["schema_version"]) is not int
        or value["schema_version"] != SCHEMA_VERSION
    ):
        raise DispatchRefusal("unsupported_schema", "dispatch request schema is unsupported")

    request_id = value["request_id"]
    if not isinstance(request_id, str) or REQUEST_ID_PATTERN.fullmatch(request_id) is None:
        raise DispatchRefusal("invalid_request", "request id is invalid")

    source = value["source"]
    if not isinstance(source, dict) or set(source) != SOURCE_KEYS:
        raise DispatchRefusal("invalid_request", "source identity is invalid")
    repository = source["repository"]
    commit = source["commit"]
    tree = source["tree"]
    if not isinstance(repository, str) or REPOSITORY_PATTERN.fullmatch(repository) is None:
        raise DispatchRefusal("invalid_request", "source repository identity is invalid")
    if (
        not isinstance(commit, str)
        or OID_PATTERN.fullmatch(commit) is None
        or not isinstance(tree, str)
        or OID_PATTERN.fullmatch(tree) is None
    ):
        raise DispatchRefusal("invalid_request", "source commit/tree identity is invalid")

    operation = value["operation"]
    if not isinstance(operation, dict) or set(operation) != OPERATION_KEYS:
        raise DispatchRefusal("invalid_request", "operation is invalid")
    operation_kind = _token(operation["kind"], "operation kind")
    profile = _token(operation["profile"], "operation profile")
    if operation_kind != OPERATION_KIND or profile != PROFILE_ID:
        raise DispatchRefusal("unsupported_operation", "operation is outside the closed v1 vocabulary")

    caller = value["caller"]
    if not isinstance(caller, dict) or set(caller) != CALLER_KEYS:
        raise DispatchRefusal("invalid_request", "caller provenance is invalid")
    caller_principal = _token(caller["principal"], "caller principal")
    provenance_binding = _token(caller["provenance_binding"], "provenance binding")

    supersession = value["supersession"]
    if not isinstance(supersession, dict) or set(supersession) != SUPERSESSION_KEYS:
        raise DispatchRefusal("invalid_request", "supersession policy is invalid")
    policy = supersession["policy"]
    if policy != SUPERSESSION_POLICY:
        raise DispatchRefusal("unsupported_supersession", "supersession policy is unsupported")

    created = _parse_utc(value["created_at"], "created_at")
    expires = _parse_utc(value["expires_at"], "expires_at")
    current = dt.datetime.now(dt.timezone.utc) if now is None else now
    if current.tzinfo is None:
        raise ValueError("now must be timezone-aware")
    current = current.astimezone(dt.timezone.utc)
    if expires <= created or expires - created > MAX_LIFETIME:
        raise DispatchRefusal("invalid_request", "request lifetime is outside v1 bounds")
    if created > current + MAX_FUTURE_SKEW:
        raise DispatchRefusal("future_request", "request creation time is too far in the future")
    if expires <= current and not allow_expired:
        raise DispatchRefusal("stale_request", "request expired before acceptance")

    expected_fingerprint = fingerprint_document(value)
    fingerprint = value["request_fingerprint"]
    if (
        not isinstance(fingerprint, str)
        or SHA256_PATTERN.fullmatch(fingerprint) is None
        or fingerprint != expected_fingerprint
    ):
        raise DispatchRefusal(
            "request_identity_conflict",
            "request fingerprint does not match its semantics",
        )

    return DispatchRequest(
        request_id=request_id,
        repository=repository,
        commit=commit,
        tree=tree,
        operation_kind=operation_kind,
        profile=profile,
        caller_principal=caller_principal,
        provenance_binding=provenance_binding,
        created_at=value["created_at"],
        expires_at=value["expires_at"],
        supersession_policy=policy,
        request_fingerprint=fingerprint,
    )



def decode_projection(
    raw: bytes,
    *,
    now: dt.datetime | None = None,
    allow_expired: bool = False,
) -> DispatchRequest:
    if len(raw) > MAX_REQUEST_BYTES:
        raise DispatchRefusal(
            "oversized_request",
            "dispatch projection exceeds its fixed ceiling",
        )
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise DispatchRefusal(
            "invalid_request",
            "dispatch projection is not valid JSON",
        ) from error
    if not isinstance(value, dict) or set(value) != PROJECTION_KEYS:
        raise DispatchRefusal(
            "invalid_request",
            "dispatch projection has unsupported fields",
        )
    if canonical_bytes(value) + b"\n" != raw:
        raise DispatchRefusal(
            "noncanonical_request",
            "dispatch projection is not canonical JSON",
        )
    complete = dict(value)
    complete["request_fingerprint"] = fingerprint_document(value)
    return decode_request(
        canonical_bytes(complete) + b"\n",
        now=now,
        allow_expired=allow_expired,
    )


def request_document(request: DispatchRequest) -> dict[str, object]:
    value = identity_document(request)
    value["request_fingerprint"] = request.request_fingerprint
    return value


def semantic_request_id(request: DispatchRequest) -> str:
    digest = request.request_fingerprint.removeprefix("sha256:")
    if len(digest) != 64:
        raise DispatchRefusal(
            "internal_contract_error",
            "accepted dispatch fingerprint is invalid",
        )
    return "accepted-" + digest[:55]


def _bounded(value: dict[str, object], label: str) -> dict[str, object]:
    if len(canonical_bytes(value) + b"\n") > MAX_DOCUMENT_BYTES:
        raise DispatchRefusal(
            "internal_contract_error",
            f"{label} exceeds its fixed ceiling",
        )
    return value


def accept_request(
    request: DispatchRequest,
    provenance: ProvenanceEvidence,
) -> AcceptedRequest:
    principal = _token(provenance.principal, "observed provenance principal")
    binding = _token(provenance.binding, "observed provenance binding")
    if principal != request.caller_principal or binding != request.provenance_binding:
        raise DispatchRefusal(
            "untrusted_provenance",
            "observed provenance does not match the request",
        )

    try:
        semantic_request = semantic.make_verify_named_request(
            semantic_request_id(request),
            request.repository,
            request.commit,
            request.tree,
            request.profile,
        )
        compiled_semantic = semantic.compile_request(semantic_request)
    except semantic.ContractRefusal as error:
        raise DispatchRefusal(error.code, str(error)) from error
    if compiled_semantic.workload is None:
        raise DispatchRefusal(
            "internal_contract_error",
            "semantic request did not resolve a source-executing workload",
        )
    compiled = compiled_semantic.workload
    external_request = compiled.external
    planned = external.planned_receipt(compiled)
    resolved = planned.get("resolved_workload")
    if not isinstance(resolved, dict):
        raise DispatchRefusal(
            "internal_contract_error",
            "external adapter did not resolve a workload",
        )

    accepted = _bounded(
        {
            "document_type": ACCEPTED_DOCUMENT_TYPE,
            "schema_version": SCHEMA_VERSION,
            "request_id": request.request_id,
            "request_fingerprint": request.request_fingerprint,
            "source": {
                "repository": request.repository,
                "commit": request.commit,
                "tree": request.tree,
            },
            "operation": {
                "kind": request.operation_kind,
                "profile": request.profile,
            },
            "caller": {
                "principal": request.caller_principal,
                "provenance_binding": request.provenance_binding,
            },
            "created_at": request.created_at,
            "expires_at": request.expires_at,
            "supersession": {"policy": request.supersession_policy},
            "semantic_request_id": semantic_request_id(request),
            "semantic_request_sha256": compiled_semantic.request_sha256,
            "external_request_sha256": compiled.request_sha256,
            "workload_command_fingerprint": compiled.internal.command_fingerprint,
            "resolved_workload": resolved,
            "authority": dict(AUTHORITY),
        },
        "accepted request",
    )
    return AcceptedRequest(
        request,
        semantic_request,
        compiled_semantic.request_sha256,
        external_request,
        compiled.request_sha256,
        accepted,
    )


def validate_accepted(
    value: dict[str, object],
    accepted: AcceptedRequest,
) -> dict[str, object]:
    if set(value) != ACCEPTED_KEYS or value != accepted.accepted_document:
        raise DispatchRefusal(
            "accepted_identity_conflict",
            "accepted dispatch identity changed",
        )
    return value


def initial_lifecycle(accepted: AcceptedRequest) -> dict[str, object]:
    return _bounded(
        {
            "document_type": LIFECYCLE_DOCUMENT_TYPE,
            "schema_version": SCHEMA_VERSION,
            "request_id": accepted.request.request_id,
            "request_fingerprint": accepted.request.request_fingerprint,
            "state": "accepted",
            "terminal_class": None,
            "external_receipt_sha256": None,
            "authority": dict(AUTHORITY),
        },
        "dispatch lifecycle",
    )


def validate_lifecycle(
    value: dict[str, object],
    accepted: AcceptedRequest,
) -> dict[str, object]:
    if (
        set(value) != LIFECYCLE_KEYS
        or value.get("document_type") != LIFECYCLE_DOCUMENT_TYPE
        or type(value.get("schema_version")) is not int
        or value.get("schema_version") != SCHEMA_VERSION
        or value.get("request_id") != accepted.request.request_id
        or value.get("request_fingerprint") != accepted.request.request_fingerprint
        or value.get("authority") != AUTHORITY
        or value.get("state")
        not in {"accepted", "launching", "refused", "ambiguous", "terminal"}
    ):
        raise DispatchRefusal(
            "lifecycle_identity_conflict",
            "dispatch lifecycle is invalid",
        )
    state = value["state"]
    terminal = value["terminal_class"]
    digest = value["external_receipt_sha256"]
    if state in {"accepted", "launching"}:
        if terminal is not None or digest is not None:
            raise DispatchRefusal(
                "lifecycle_identity_conflict",
                "nonterminal lifecycle carries settlement",
            )
        return value

    if not isinstance(digest, str) or SHA256_PATTERN.fullmatch(digest) is None:
        raise DispatchRefusal(
            "lifecycle_identity_conflict",
            "terminal lifecycle settlement is invalid",
        )
    if state == "refused":
        valid_terminal = terminal == "refused"
    elif state == "ambiguous":
        valid_terminal = terminal == "ambiguous"
    else:
        valid_terminal = terminal in {
            "succeeded",
            "failed",
            "timed_out",
            "cleanup_incomplete",
        }
    if not valid_terminal:
        raise DispatchRefusal(
            "lifecycle_identity_conflict",
            "lifecycle state and terminal class disagree",
        )
    return value


def mark_launching(
    value: dict[str, object],
    accepted: AcceptedRequest,
) -> dict[str, object]:
    current = validate_lifecycle(value, accepted)
    if current["state"] == "accepted":
        next_value = dict(current)
        next_value["state"] = "launching"
        return _bounded(next_value, "dispatch lifecycle")
    if current["state"] == "launching":
        return current
    raise DispatchRefusal(
        "settled_request",
        "settled dispatch cannot start another execution",
    )


def restart_disposition(
    value: dict[str, object],
    accepted: AcceptedRequest,
) -> str:
    state = validate_lifecycle(value, accepted)["state"]
    if state == "accepted":
        return "launch_after_durable_mark"
    if state in {"launching", "ambiguous"}:
        return "reconcile_only"
    return "return_settlement"



def validate_external_terminal(
    receipt: dict[str, object],
    accepted: AcceptedRequest,
) -> str:
    try:
        external.validate_replay(receipt, accepted.external_request)
    except external.ContractRefusal as error:
        raise DispatchRefusal(
            "invalid_external_receipt",
            str(error),
        ) from error
    terminal = receipt.get("state")
    if terminal not in TERMINAL_EXTERNAL_STATES:
        raise DispatchRefusal(
            "invalid_external_receipt",
            "external receipt is not terminal",
        )
    resolved = receipt.get("resolved_workload")
    workload_digest = receipt.get("workload_receipt_sha256")
    refusal_code = receipt.get("refusal_code")
    expected_resolved = accepted.accepted_document["resolved_workload"]
    if terminal in {"succeeded", "failed", "timed_out", "cleanup_incomplete"}:
        valid = (
            resolved == expected_resolved
            and isinstance(workload_digest, str)
            and SHA256_PATTERN.fullmatch(workload_digest) is not None
            and refusal_code is None
        )
    elif terminal == "ambiguous":
        valid = (
            resolved == expected_resolved
            and workload_digest is None
            and refusal_code == "ambiguous_execution"
        )
    else:
        valid = (
            resolved is None
            and workload_digest is None
            and isinstance(refusal_code, str)
            and TOKEN_PATTERN.fullmatch(refusal_code) is not None
        )
    if not valid:
        raise DispatchRefusal(
            "invalid_external_receipt",
            "external terminal receipt fields are inconsistent",
        )
    return terminal

def settle_from_external(
    value: dict[str, object],
    accepted: AcceptedRequest,
    external_receipt: dict[str, object],
) -> dict[str, object]:
    current = validate_lifecycle(value, accepted)
    if current["state"] in {"refused", "ambiguous", "terminal"}:
        candidate_digest = sha256(canonical_bytes(external_receipt) + b"\n")
        if candidate_digest == current["external_receipt_sha256"]:
            return current
        raise DispatchRefusal(
            "settlement_conflict",
            "settled dispatch received a different receipt",
        )
    if current["state"] != "launching":
        raise DispatchRefusal(
            "launch_not_recorded",
            "terminal settlement requires a durable launching state",
        )

    terminal = validate_external_terminal(external_receipt, accepted)
    digest = sha256(canonical_bytes(external_receipt) + b"\n")
    state = "terminal"
    if terminal == "refused":
        state = "refused"
    elif terminal == "ambiguous":
        state = "ambiguous"
    return _bounded(
        {
            "document_type": LIFECYCLE_DOCUMENT_TYPE,
            "schema_version": SCHEMA_VERSION,
            "request_id": accepted.request.request_id,
            "request_fingerprint": accepted.request.request_fingerprint,
            "state": state,
            "terminal_class": terminal,
            "external_receipt_sha256": digest,
            "authority": dict(AUTHORITY),
        },
        "dispatch lifecycle",
    )


def result_document(
    value: dict[str, object],
    accepted: AcceptedRequest,
) -> dict[str, object]:
    lifecycle = validate_lifecycle(value, accepted)
    if lifecycle["state"] not in {"refused", "ambiguous", "terminal"}:
        raise DispatchRefusal(
            "result_unavailable",
            "dispatch has no terminal result",
        )
    return _bounded(
        {
            "document_type": RESULT_DOCUMENT_TYPE,
            "schema_version": SCHEMA_VERSION,
            "request_id": accepted.request.request_id,
            "request_fingerprint": accepted.request.request_fingerprint,
            "source": {
                "repository": accepted.request.repository,
                "commit": accepted.request.commit,
                "tree": accepted.request.tree,
            },
            "operation": {
                "kind": accepted.request.operation_kind,
                "profile": accepted.request.profile,
            },
            "caller_principal": accepted.request.caller_principal,
            "semantic_request_id": accepted.accepted_document["semantic_request_id"],
            "state": lifecycle["state"],
            "terminal_class": lifecycle["terminal_class"],
            "external_receipt_sha256": lifecycle["external_receipt_sha256"],
            "contains_credentials": False,
            "contains_private_content": False,
            "authority": dict(AUTHORITY),
        },
        "dispatch result",
    )


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("command", choices=("accept", "accept-projection"))
    value.add_argument("--principal", required=True)
    value.add_argument("--provenance-binding", required=True)
    value.add_argument("--allow-expired", action="store_true")
    return value


def main() -> int:
    arguments = parser().parse_args()
    raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    try:
        if arguments.command == "accept-projection":
            request = decode_projection(raw, allow_expired=arguments.allow_expired)
        else:
            request = decode_request(raw, allow_expired=arguments.allow_expired)
        accepted = accept_request(
            request,
            ProvenanceEvidence(
                arguments.principal,
                arguments.provenance_binding,
            ),
        )
        sys.stdout.buffer.write(canonical_bytes(accepted.accepted_document) + b"\n")
        return 0
    except DispatchRefusal as refusal:
        sys.stderr.buffer.write(
            canonical_bytes(
                {
                    "document_type": "glaeda-trusted-agent-dispatch-error",
                    "schema_version": 1,
                    "problem": refusal.code,
                    "authority": "none",
                }
            )
            + b"\n"
        )
        return 75


if __name__ == "__main__":
    raise SystemExit(main())
