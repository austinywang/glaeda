#!/usr/bin/env python3
"""Caller-neutral external execution request compiler for one reviewed Glaeda workload.

The external document carries semantic source/work identity only. It cannot select commands,
environment, host paths, machine/backend, cgroup/systemd properties, workload generations, or
attempt identity. The first mapping is the existing credentialless `verify-focused/v1` adapter.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import re
import sys
from typing import NoReturn

import verify_focused_impl as focused

REQUEST_DOCUMENT_TYPE = "glaeda-external-execution-request"
RECEIPT_DOCUMENT_TYPE = "glaeda-external-execution-receipt"
REQUEST_SCHEMA_VERSION = 1
RECEIPT_SCHEMA_VERSION = 1
MAX_REQUEST_BYTES = 4 * 1024
MAX_RECEIPT_BYTES = 4 * 1024
MAX_REFERENCE_BYTES = 128
INTERNAL_BINDING_DOMAIN = "glaeda-external-verify-focused-binding-v1"
OPERATION_VERIFY_FOCUSED = "verify_focused"
REUSE_HINTS = {"no_preference", "prefer_valid_reuse"}
TOKEN_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,127}$")
SEMANTIC_REQUEST_ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{7,63}$")
REPOSITORY_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
REQUEST_KEYS = {
    "document_type",
    "schema_version",
    "external_request_ref",
    "source",
    "operation",
    "requested_capability_class",
    "reuse_hint",
    "correlation",
}
REQUIRED_REQUEST_KEYS = {
    "document_type",
    "schema_version",
    "external_request_ref",
    "source",
    "operation",
    "requested_capability_class",
}
SOURCE_KEYS = {"repository", "commit", "tree"}
CORRELATION_KEYS = {"work_ref"}
RECEIPT_KEYS = {
    "document_type",
    "schema_version",
    "external_request_ref",
    "request_sha256",
    "correlation",
    "operation",
    "source",
    "state",
    "resolved_workload",
    "workload_receipt_sha256",
    "refusal_code",
    "authority",
}
AUTHORITY = {
    "authorizes_execution": False,
    "authorizes_redispatch": False,
    "authorizes_host_selection": False,
    "authorizes_cleanup": False,
}


class ContractRefusal(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class ExternalRequest:
    external_request_ref: str
    repository: str
    commit: str
    tree: str
    operation: str
    requested_capability_class: str
    reuse_hint: str | None = None
    work_ref: str | None = None


@dataclass(frozen=True)
class CompiledRequest:
    external: ExternalRequest
    request_sha256: str
    internal: focused.Request


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def _token(value: object, label: str) -> str:
    if (
        not isinstance(value, str)
        or len(value.encode("utf-8")) > MAX_REFERENCE_BYTES
        or not TOKEN_PATTERN.fullmatch(value)
    ):
        raise ContractRefusal("invalid_request", f"{label} is invalid")
    return value


def request_document(request: ExternalRequest) -> dict[str, object]:
    document: dict[str, object] = {
        "document_type": REQUEST_DOCUMENT_TYPE,
        "schema_version": REQUEST_SCHEMA_VERSION,
        "external_request_ref": request.external_request_ref,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "operation": request.operation,
        "requested_capability_class": request.requested_capability_class,
    }
    if request.reuse_hint is not None:
        document["reuse_hint"] = request.reuse_hint
    if request.work_ref is not None:
        document["correlation"] = {"work_ref": request.work_ref}
    return document


def decode_request(raw: bytes) -> ExternalRequest:
    if len(raw) > MAX_REQUEST_BYTES:
        raise ContractRefusal("invalid_request", "external request exceeds its fixed ceiling")
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise ContractRefusal("invalid_request", "external request is not valid JSON") from error
    if not isinstance(value, dict) or not REQUIRED_REQUEST_KEYS <= set(value) <= REQUEST_KEYS:
        raise ContractRefusal("invalid_request", "external request has unsupported fields")
    if (
        value["document_type"] != REQUEST_DOCUMENT_TYPE
        or type(value["schema_version"]) is not int
        or value["schema_version"] != REQUEST_SCHEMA_VERSION
    ):
        raise ContractRefusal("unsupported_schema", "external request schema is unsupported")
    source = value["source"]
    if not isinstance(source, dict) or set(source) != SOURCE_KEYS:
        raise ContractRefusal("invalid_request", "external source identity is invalid")
    repository = source["repository"]
    commit = source["commit"]
    tree = source["tree"]
    if not isinstance(repository, str) or not REPOSITORY_PATTERN.fullmatch(repository):
        raise ContractRefusal("invalid_request", "source repository identity is invalid")
    if (
        not isinstance(commit, str)
        or not OID_PATTERN.fullmatch(commit)
        or not isinstance(tree, str)
        or not OID_PATTERN.fullmatch(tree)
    ):
        raise ContractRefusal("invalid_request", "source commit/tree identity is invalid")
    external_request_ref = _token(value["external_request_ref"], "external request reference")
    operation = _token(value["operation"], "operation")
    capability = _token(value["requested_capability_class"], "requested capability class")
    reuse_hint = value.get("reuse_hint")
    if reuse_hint is not None and (
        not isinstance(reuse_hint, str) or reuse_hint not in REUSE_HINTS
    ):
        raise ContractRefusal("invalid_request", "reuse hint is invalid")
    work_ref = None
    correlation = value.get("correlation")
    if correlation is not None:
        if not isinstance(correlation, dict) or set(correlation) != CORRELATION_KEYS:
            raise ContractRefusal("invalid_request", "caller correlation is invalid")
        work_ref = _token(correlation["work_ref"], "caller work reference")
    return ExternalRequest(
        external_request_ref,
        repository,
        commit,
        tree,
        operation,
        capability,
        reuse_hint,
        work_ref,
    )


def request_sha256(request: ExternalRequest) -> str:
    return sha256(canonical_bytes(request_document(request)))


def _internal_fingerprint(
    request: ExternalRequest,
    profile_generation: str,
    semantic_request_id: str | None = None,
) -> str:
    # Caller refs, correlation, and reuse hints are deliberately absent: they cannot mint
    # physical execution identity.
    binding = {
        "domain": INTERNAL_BINDING_DOMAIN,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "operation": OPERATION_VERIFY_FOCUSED,
        "requested_capability_class": focused.EXECUTION_IDENTITY_CLASS,
        "resolved_workload": {
            "id": focused.FOCUSED_PROFILE.profile_id,
            "generation": profile_generation,
        },
    }
    if semantic_request_id is not None:
        binding["semantic_request_id"] = semantic_request_id
    return sha256(canonical_bytes(binding))


def compile_request(
    request: ExternalRequest,
    *,
    semantic_request_id: str | None = None,
) -> CompiledRequest:
    if semantic_request_id is not None and (
        not isinstance(semantic_request_id, str)
        or SEMANTIC_REQUEST_ID_PATTERN.fullmatch(semantic_request_id) is None
    ):
        raise ContractRefusal(
            "invalid_request",
            "accepted semantic request identity is invalid",
        )
    if request.operation != OPERATION_VERIFY_FOCUSED:
        raise ContractRefusal(
            "unsupported_operation", "operation is not admitted by this adapter"
        )
    if request.requested_capability_class != focused.EXECUTION_IDENTITY_CLASS:
        raise ContractRefusal(
            "unsupported_capability", "requested capability is not admitted by this adapter"
        )
    profile = focused.FOCUSED_PROFILE
    generation = focused.profile_generation(profile)
    if not SHA256_PATTERN.fullmatch(generation):
        raise ContractRefusal(
            "internal_contract_error", "resolved workload generation is invalid"
        )
    internal = focused.Request(
        repository=request.repository,
        commit=request.commit,
        tree=request.tree,
        profile_generation=generation,
        command_fingerprint=_internal_fingerprint(
            request,
            generation,
            semantic_request_id,
        ),
        profile=profile,
    )
    return CompiledRequest(request, request_sha256(request), internal)


def _resolved_workload(compiled: CompiledRequest) -> dict[str, object]:
    return {
        "id": compiled.internal.profile.profile_id,
        "generation": compiled.internal.profile_generation,
        "capability_class": focused.EXECUTION_IDENTITY_CLASS,
    }


def _receipt(
    request: ExternalRequest,
    digest: str,
    *,
    state: str,
    resolved_workload: dict[str, object] | None,
    workload_receipt_sha256: str | None = None,
    refusal_code: str | None = None,
) -> dict[str, object]:
    value = {
        "document_type": RECEIPT_DOCUMENT_TYPE,
        "schema_version": RECEIPT_SCHEMA_VERSION,
        "external_request_ref": request.external_request_ref,
        "request_sha256": digest,
        "correlation": {"work_ref": request.work_ref}
        if request.work_ref is not None
        else None,
        "operation": request.operation,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "state": state,
        "resolved_workload": resolved_workload,
        "workload_receipt_sha256": workload_receipt_sha256,
        "refusal_code": refusal_code,
        "authority": dict(AUTHORITY),
    }
    encoded = canonical_bytes(value) + b"\n"
    if len(encoded) > MAX_RECEIPT_BYTES:
        raise ContractRefusal(
            "internal_contract_error", "external receipt exceeds its fixed ceiling"
        )
    return value


def planned_receipt(compiled: CompiledRequest) -> dict[str, object]:
    return _receipt(
        compiled.external,
        compiled.request_sha256,
        state="planned",
        resolved_workload=_resolved_workload(compiled),
    )


def refused_receipt(
    request: ExternalRequest, refusal: ContractRefusal
) -> dict[str, object]:
    return _receipt(
        request,
        request_sha256(request),
        state="refused",
        resolved_workload=None,
        refusal_code=refusal.code,
    )


def ambiguous_receipt(compiled: CompiledRequest) -> dict[str, object]:
    return _receipt(
        compiled.external,
        compiled.request_sha256,
        state="ambiguous",
        resolved_workload=_resolved_workload(compiled),
        refusal_code="ambiguous_execution",
    )


def terminal_receipt(
    compiled: CompiledRequest, workload_receipt: dict[str, object]
) -> dict[str, object]:
    if not focused.valid_terminal_receipt(workload_receipt, compiled.internal):
        raise ContractRefusal(
            "invalid_workload_receipt",
            "workload receipt does not match the compiled request",
        )
    terminal = workload_receipt["result"]["terminal_class"]
    raw = canonical_bytes(workload_receipt) + b"\n"
    return _receipt(
        compiled.external,
        compiled.request_sha256,
        state=terminal,
        resolved_workload=_resolved_workload(compiled),
        workload_receipt_sha256=sha256(raw),
    )


def validate_receipt(value: dict[str, object]) -> dict[str, object]:
    if len(canonical_bytes(value) + b"\n") > MAX_RECEIPT_BYTES:
        raise ContractRefusal(
            "invalid_existing_receipt",
            "existing external receipt exceeds its fixed ceiling",
        )
    expected_source_keys = {"repository", "commit", "tree"}
    if (
        not isinstance(value, dict)
        or set(value) != RECEIPT_KEYS
        or value.get("document_type") != RECEIPT_DOCUMENT_TYPE
        or type(value.get("schema_version")) is not int
        or value.get("schema_version") != RECEIPT_SCHEMA_VERSION
        or value.get("authority") != AUTHORITY
    ):
        raise ContractRefusal(
            "invalid_existing_receipt", "existing external receipt is invalid"
        )
    external_ref = value.get("external_request_ref")
    operation = value.get("operation")
    request_digest = value.get("request_sha256")
    source = value.get("source")
    correlation = value.get("correlation")
    if (
        not isinstance(external_ref, str)
        or len(external_ref.encode("utf-8")) > MAX_REFERENCE_BYTES
        or TOKEN_PATTERN.fullmatch(external_ref) is None
        or not isinstance(operation, str)
        or len(operation.encode("utf-8")) > MAX_REFERENCE_BYTES
        or TOKEN_PATTERN.fullmatch(operation) is None
        or not isinstance(request_digest, str)
        or SHA256_PATTERN.fullmatch(request_digest) is None
        or not isinstance(source, dict)
        or set(source) != expected_source_keys
        or not isinstance(source.get("repository"), str)
        or REPOSITORY_PATTERN.fullmatch(source["repository"]) is None
        or not isinstance(source.get("commit"), str)
        or OID_PATTERN.fullmatch(source["commit"]) is None
        or not isinstance(source.get("tree"), str)
        or OID_PATTERN.fullmatch(source["tree"]) is None
    ):
        raise ContractRefusal(
            "invalid_existing_receipt", "existing external receipt is invalid"
        )
    if correlation is not None:
        if (
            not isinstance(correlation, dict)
            or set(correlation) != CORRELATION_KEYS
            or not isinstance(correlation.get("work_ref"), str)
            or len(correlation["work_ref"].encode("utf-8")) > MAX_REFERENCE_BYTES
            or TOKEN_PATTERN.fullmatch(correlation["work_ref"]) is None
        ):
            raise ContractRefusal(
                "invalid_existing_receipt", "existing external receipt is invalid"
            )

    state = value.get("state")
    resolved = value.get("resolved_workload")
    workload_digest = value.get("workload_receipt_sha256")
    refusal = value.get("refusal_code")
    terminal_states = {"succeeded", "failed", "timed_out", "cleanup_incomplete"}
    supported_states = {"planned", "refused", "ambiguous", *terminal_states}
    if state not in supported_states:
        raise ContractRefusal(
            "invalid_existing_receipt", "existing external receipt state is invalid"
        )

    expected_resolved = {
        "id": focused.FOCUSED_PROFILE.profile_id,
        "generation": focused.profile_generation(focused.FOCUSED_PROFILE),
        "capability_class": focused.EXECUTION_IDENTITY_CLASS,
    }
    if state == "refused":
        valid = (
            resolved is None
            and workload_digest is None
            and isinstance(refusal, str)
            and len(refusal.encode("utf-8")) <= MAX_REFERENCE_BYTES
            and TOKEN_PATTERN.fullmatch(refusal) is not None
        )
    elif state == "planned":
        valid = (
            operation == OPERATION_VERIFY_FOCUSED
            and resolved == expected_resolved
            and workload_digest is None
            and refusal is None
        )
    elif state == "ambiguous":
        valid = (
            operation == OPERATION_VERIFY_FOCUSED
            and resolved == expected_resolved
            and workload_digest is None
            and refusal == "ambiguous_execution"
        )
    else:
        valid = (
            operation == OPERATION_VERIFY_FOCUSED
            and resolved == expected_resolved
            and isinstance(workload_digest, str)
            and SHA256_PATTERN.fullmatch(workload_digest) is not None
            and refusal is None
        )
    if not valid:
        raise ContractRefusal(
            "invalid_existing_receipt",
            "existing external receipt state fields are inconsistent",
        )
    return value


def validate_replay(
    existing_receipt: dict[str, object], request: ExternalRequest
) -> dict[str, object]:
    expected_source = {
        "repository": request.repository,
        "commit": request.commit,
        "tree": request.tree,
    }
    expected_correlation = (
        {"work_ref": request.work_ref} if request.work_ref is not None else None
    )
    validate_receipt(existing_receipt)
    if existing_receipt.get("external_request_ref") != request.external_request_ref:
        raise ContractRefusal(
            "external_request_mismatch",
            "existing receipt belongs to another external request",
        )
    if existing_receipt.get("request_sha256") != request_sha256(request):
        raise ContractRefusal(
            "external_request_conflict",
            "external request reference was reused with different semantics",
        )
    if (
        existing_receipt.get("operation") != request.operation
        or existing_receipt.get("source") != expected_source
        or existing_receipt.get("correlation") != expected_correlation
    ):
        raise ContractRefusal(
            "invalid_existing_receipt", "existing external receipt is invalid"
        )
    return existing_receipt


def plan_from_bytes(raw: bytes) -> dict[str, object]:
    request = decode_request(raw)
    try:
        return planned_receipt(compile_request(request))
    except ContractRefusal as refusal:
        return refused_receipt(request, refusal)


def emit(value: dict[str, object]) -> None:
    sys.stdout.buffer.write(canonical_bytes(value) + b"\n")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("command", choices=("plan",))
    return root


def main() -> int:
    parser().parse_args()
    raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    try:
        emit(plan_from_bytes(raw))
        return 0
    except ContractRefusal as refusal:
        print(
            json.dumps(
                {
                    "document_type": "glaeda-external-execution-error",
                    "schema_version": 1,
                    "authority": "none",
                    "problem": refusal.code,
                },
                sort_keys=True,
                separators=(",", ":"),
            ),
            file=sys.stderr,
        )
        return 75


if __name__ == "__main__":
    raise SystemExit(main())
