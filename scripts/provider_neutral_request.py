#!/usr/bin/env python3
"""Provider-neutral semantic request/receipt contract for owner-local Glaeda work.

This module owns semantic identity and typed result projection only. It performs no Git, network,
filesystem, process, scheduler, credential, or host mutation. Transport adapters authenticate a
caller and compile into this contract; reviewed Glaeda adapters retain all physical authority.
"""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import re
from typing import NoReturn

import external_execution_request as external
import verify_focused_impl as focused


REQUEST_DOCUMENT_TYPE = "glaeda-semantic-request"
RECEIPT_DOCUMENT_TYPE = "glaeda-semantic-receipt"
CAPABILITIES_DOCUMENT_TYPE = "glaeda-semantic-capabilities"
REQUEST_SCHEMA_VERSION = 1
RECEIPT_SCHEMA_VERSION = 1
CAPABILITIES_SCHEMA_VERSION = 1
MAX_REQUEST_BYTES = 16 * 1024
MAX_RECEIPT_BYTES = 192 * 1024
MAX_REPO_QUERY_RESULT_BYTES = 128 * 1024
MAX_REFERENCE_BYTES = 128
MAX_REPO_QUERY_PATCH_BYTES = 8 * 1024
REPO_QUERY_MAX_PATCH_BYTES = 64 * 1024
REPO_QUERY_MAX_CHANGED_FILES = 64
REPO_QUERY_MAX_GIT_OUTPUT_BYTES = 1024 * 1024
REPO_QUERY_MAX_AUXILIARY_QUERIES = 8
REPO_QUERY_MAX_GREP_MATCHES = 64
REPO_QUERY_MAX_BLOB_BYTES = 16 * 1024
REPO_QUERY_MAX_HISTORY_COMMITS = 32
REPO_QUERY_MAX_PATH_BYTES = 1024
REPO_QUERY_MAX_LITERAL_BYTES = 512
REPO_QUERY_MAX_MATCH_TEXT_BYTES = 1024
REPO_QUERY_MAX_SUBJECT_BYTES = 512
REPO_QUERY_COMMAND_TIMEOUT_MILLIS = 10_000
REPO_QUERY_PROFILE_CONTRACT = (
    b"glaeda-repo-query-profile-v1\0"
    b"exact-commit-objects\0merge-base\0ancestry\0commit-count\0"
    b"numstat-no-renames\0bounded-complete-patch\0literal-tree-grep\0"
    b"bounded-tree-blobs\0path-history\0object-info\0object-format\0network-disabled\0"
    b"checkout-and-git-directory-identity-stable\0origin-reobserved\0"
)
REPO_QUERY_RESULT_DOCUMENT_TYPE = "glaeda-semantic-repo-query-result"

OP_CAPABILITIES = "capabilities"
OP_STATUS = "status"
OP_REPO_QUERY = "repo_query"
OP_VERIFY_NAMED = "verify_named"
OPERATIONS = {OP_CAPABILITIES, OP_STATUS, OP_REPO_QUERY, OP_VERIFY_NAMED}

REQUEST_ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{7,63}$")
REPOSITORY_PATTERN = re.compile(r"^[a-z0-9_.-]+/[a-z0-9_.-]+$")
OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
PROFILE_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,159}$")
REASON_PATTERN = re.compile(r"^[a-z0-9][a-z0-9_]{0,63}$")

REQUEST_KEYS = {"document_type", "schema_version", "request_id", "operation", "source", "parameters"}
SOURCE_KEYS = {"repository", "commit", "tree"}
RECEIPT_KEYS = {
    "document_type",
    "schema_version",
    "request_id",
    "request_sha256",
    "operation",
    "source",
    "state",
    "resolved_operation",
    "result",
    "result_sha256",
    "refusal_code",
    "authority",
}
AUTHORITY = {
    "authorizes_execution": False,
    "authorizes_redispatch": False,
    "authorizes_host_selection": False,
    "authorizes_cleanup": False,
}
RECEIPT_STATES = {
    "planned",
    "waiting",
    "refused",
    "ambiguous",
    "succeeded",
    "failed",
    "timed_out",
    "cleanup_incomplete",
}


class ContractRefusal(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class SourceIdentity:
    repository: str
    commit: str
    tree: str


@dataclass(frozen=True)
class SemanticRequest:
    request_id: str
    operation: str
    source: SourceIdentity | None
    parameters: dict[str, object]


@dataclass(frozen=True)
class CompiledRequest:
    request: SemanticRequest
    request_sha256: str
    resolved_operation: dict[str, object]
    workload: external.CompiledRequest | None = None
    internal: focused.Request | None = None


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def _append_repo_query_field(buffer: bytearray, value: str) -> None:
    raw = value.encode("utf-8")
    buffer.extend(len(raw).to_bytes(8, "big"))
    buffer.extend(raw)


def repo_query_profile_generation() -> str:
    contract = bytearray(REPO_QUERY_PROFILE_CONTRACT)
    for value in (
        REPO_QUERY_MAX_PATCH_BYTES,
        REPO_QUERY_MAX_CHANGED_FILES,
        REPO_QUERY_MAX_GIT_OUTPUT_BYTES,
        MAX_REPO_QUERY_RESULT_BYTES,
        REPO_QUERY_MAX_AUXILIARY_QUERIES,
        REPO_QUERY_MAX_GREP_MATCHES,
        REPO_QUERY_MAX_BLOB_BYTES,
        REPO_QUERY_MAX_HISTORY_COMMITS,
        REPO_QUERY_MAX_PATH_BYTES,
        REPO_QUERY_MAX_LITERAL_BYTES,
        REPO_QUERY_MAX_MATCH_TEXT_BYTES,
        REPO_QUERY_MAX_SUBJECT_BYTES,
        REPO_QUERY_COMMAND_TIMEOUT_MILLIS,
    ):
        _append_repo_query_field(contract, str(value))
    return sha256(bytes(contract))


def repo_query_request_digest(request: SemanticRequest) -> str:
    if request.operation != OP_REPO_QUERY or request.source is None:
        raise ContractRefusal("internal_contract_error", "request is not repo_query")
    buffer = bytearray()
    for value in (
        f"github.com/{request.source.repository}",
        str(request.parameters["base_commit"]),
        request.source.commit,
        request.source.tree,
        str(request.parameters["max_patch_bytes"]),
        "grep",
        "blobs",
        str(REPO_QUERY_MAX_BLOB_BYTES),
        "history",
        str(REPO_QUERY_MAX_HISTORY_COMMITS),
        "objects",
        repo_query_profile_generation(),
    ):
        _append_repo_query_field(buffer, value)
    return sha256(bytes(buffer))


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def parse_request_id(value: object) -> str:
    if not isinstance(value, str) or not REQUEST_ID_PATTERN.fullmatch(value):
        raise ContractRefusal("invalid_request", "semantic request identity is invalid")
    return value


def _source(value: object) -> SourceIdentity:
    if not isinstance(value, dict) or set(value) != SOURCE_KEYS:
        raise ContractRefusal("invalid_request", "semantic source identity is invalid")
    repository = value["repository"]
    commit = value["commit"]
    tree = value["tree"]
    if not isinstance(repository, str) or not REPOSITORY_PATTERN.fullmatch(repository):
        raise ContractRefusal("invalid_request", "semantic source repository is invalid")
    if (
        not isinstance(commit, str)
        or not OID_PATTERN.fullmatch(commit)
        or not isinstance(tree, str)
        or not OID_PATTERN.fullmatch(tree)
    ):
        raise ContractRefusal("invalid_request", "semantic source commit/tree is invalid")
    return SourceIdentity(repository, commit, tree)


def _source_document(source: SourceIdentity | None) -> dict[str, str] | None:
    if source is None:
        return None
    return {"repository": source.repository, "commit": source.commit, "tree": source.tree}


def _profile_id(value: object) -> str:
    if not isinstance(value, str) or not PROFILE_PATTERN.fullmatch(value):
        raise ContractRefusal("invalid_request", "verification profile identity is invalid")
    return value


def request_document(request: SemanticRequest) -> dict[str, object]:
    return {
        "document_type": REQUEST_DOCUMENT_TYPE,
        "schema_version": REQUEST_SCHEMA_VERSION,
        "request_id": request.request_id,
        "operation": request.operation,
        "source": _source_document(request.source),
        "parameters": dict(request.parameters),
    }


def request_sha256(request: SemanticRequest) -> str:
    return sha256(canonical_bytes(request_document(request)))


def decode_request(raw: bytes) -> SemanticRequest:
    if len(raw) > MAX_REQUEST_BYTES:
        raise ContractRefusal("invalid_request", "semantic request exceeds its fixed ceiling")
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise ContractRefusal("invalid_request", "semantic request is not valid JSON") from error
    if (
        not isinstance(value, dict)
        or set(value) != REQUEST_KEYS
        or canonical_bytes(value) + b"\n" != raw
    ):
        raise ContractRefusal("invalid_request", "semantic request is not canonical closed-schema JSON")
    if (
        value["document_type"] != REQUEST_DOCUMENT_TYPE
        or type(value["schema_version"]) is not int
        or value["schema_version"] != REQUEST_SCHEMA_VERSION
    ):
        raise ContractRefusal("unsupported_schema", "semantic request schema is unsupported")
    request_id = parse_request_id(value["request_id"])
    operation = value["operation"]
    if not isinstance(operation, str) or operation not in OPERATIONS:
        raise ContractRefusal("unsupported_operation", "semantic operation is unsupported")
    parameters = value["parameters"]
    if not isinstance(parameters, dict):
        raise ContractRefusal("invalid_request", "semantic parameters must be an object")

    if operation in {OP_CAPABILITIES, OP_STATUS}:
        if value["source"] is not None or parameters:
            raise ContractRefusal("invalid_request", "read-only node operation carries unexpected source or parameters")
        return SemanticRequest(request_id, operation, None, {})

    source = _source(value["source"])
    if operation == OP_VERIFY_NAMED:
        if set(parameters) != {"profile_id"}:
            raise ContractRefusal("invalid_request", "verify_named parameters are invalid")
        return SemanticRequest(
            request_id,
            operation,
            source,
            {"profile_id": _profile_id(parameters["profile_id"])},
        )

    if set(parameters) != {"base_commit", "max_patch_bytes"}:
        raise ContractRefusal("invalid_request", "repo_query parameters are invalid")
    base_commit = parameters["base_commit"]
    patch_bytes = parameters["max_patch_bytes"]
    if not isinstance(base_commit, str) or not OID_PATTERN.fullmatch(base_commit):
        raise ContractRefusal("invalid_request", "repo_query base commit is invalid")
    if (
        isinstance(patch_bytes, bool)
        or not isinstance(patch_bytes, int)
        or not 0 <= patch_bytes <= MAX_REPO_QUERY_PATCH_BYTES
    ):
        raise ContractRefusal("invalid_request", "repo_query patch ceiling is invalid")
    return SemanticRequest(
        request_id,
        operation,
        source,
        {"base_commit": base_commit, "max_patch_bytes": patch_bytes},
    )


def make_verify_named_request(
    request_id: str,
    repository: str,
    commit: str,
    tree: str,
    profile_id: str,
) -> SemanticRequest:
    document = {
        "document_type": REQUEST_DOCUMENT_TYPE,
        "schema_version": REQUEST_SCHEMA_VERSION,
        "request_id": request_id,
        "operation": OP_VERIFY_NAMED,
        "source": {"repository": repository, "commit": commit, "tree": tree},
        "parameters": {"profile_id": profile_id},
    }
    return decode_request(canonical_bytes(document) + b"\n")


def _contract_spec() -> dict[str, object]:
    verify_generation = focused.profile_generation(focused.FOCUSED_PROFILE)
    return {
        "document_type": "glaeda-semantic-contract",
        "schema_version": 1,
        "operations": [
            {"kind": OP_CAPABILITIES, "authority": "observation_only"},
            {
                "kind": OP_STATUS,
                "authority": "observation_only",
                "observation_contract": "glaeda-owned-admission-observation/v1",
            },
            {
                "kind": OP_REPO_QUERY,
                "authority": "observation_only",
                "profile_id": "repo-query/v1",
                "profile_generation": repo_query_profile_generation(),
                "source_class": "resident_exact",
                "max_patch_bytes": MAX_REPO_QUERY_PATCH_BYTES,
            },
            {
                "kind": OP_VERIFY_NAMED,
                "authority": "bounded_execution_request",
                "profiles": [
                    {
                        "id": focused.FOCUSED_PROFILE.profile_id,
                        "generation": verify_generation,
                        "capability_class": focused.EXECUTION_IDENTITY_CLASS,
                    }
                ],
            },
        ],
        "command_argv": {
            "available": False,
            "reason": "no_reviewed_glaeda_command_profile",
        },
    }


def contract_generation() -> str:
    return sha256(canonical_bytes(_contract_spec()))


def capabilities_payload() -> dict[str, object]:
    return {
        "document_type": CAPABILITIES_DOCUMENT_TYPE,
        "schema_version": CAPABILITIES_SCHEMA_VERSION,
        "contract_generation": contract_generation(),
        **{key: value for key, value in _contract_spec().items() if key not in {"document_type", "schema_version"}},
        "authority": "observation_only",
    }


def compile_request(request: SemanticRequest) -> CompiledRequest:
    digest = request_sha256(request)
    if request.operation == OP_CAPABILITIES:
        return CompiledRequest(
            request,
            digest,
            {"kind": OP_CAPABILITIES, "contract_generation": contract_generation()},
        )
    if request.operation == OP_STATUS:
        return CompiledRequest(
            request,
            digest,
            {
                "kind": OP_STATUS,
                "observation_contract": "glaeda-owned-admission-observation/v1",
                "authority": "observation_only",
            },
        )
    if request.operation == OP_REPO_QUERY:
        assert request.source is not None
        return CompiledRequest(
            request,
            digest,
            {
                "kind": OP_REPO_QUERY,
                "profile_id": "repo-query/v1",
                "profile_generation": repo_query_profile_generation(),
                "source_class": "resident_exact",
                "base_commit": request.parameters["base_commit"],
                "max_patch_bytes": request.parameters["max_patch_bytes"],
                "request_digest": repo_query_request_digest(request),
                "result_ceiling_bytes": MAX_REPO_QUERY_RESULT_BYTES,
                "authority": "observation_only",
            },
        )

    profile_id = request.parameters["profile_id"]
    if profile_id != focused.FOCUSED_PROFILE.profile_id:
        raise ContractRefusal("unsupported_profile", "verification profile is not admitted by semantic v1")
    assert request.source is not None
    try:
        workload_request = external.ExternalRequest(
            external_request_ref=request.request_id,
            repository=request.source.repository,
            commit=request.source.commit,
            tree=request.source.tree,
            operation=external.OPERATION_VERIFY_FOCUSED,
            requested_capability_class=focused.EXECUTION_IDENTITY_CLASS,
        )
        workload = external.compile_request(
            workload_request,
            semantic_request_id=request.request_id,
        )
    except external.ContractRefusal as error:
        raise ContractRefusal(error.code, str(error)) from error
    internal = workload.internal
    generation = internal.profile_generation
    return CompiledRequest(
        request,
        digest,
        {
            "kind": OP_VERIFY_NAMED,
            "profile_id": internal.profile.profile_id,
            "profile_generation": generation,
            "capability_class": focused.EXECUTION_IDENTITY_CLASS,
            "resource_class": internal.profile.resource_class,
            "network_class": "none",
            "environment_class": "minimal",
            "deadline_seconds": internal.profile.deadline_seconds,
            "output_ceiling_bytes": focused.MAX_SOURCE_OUTPUT_BYTES,
            "workload_request_sha256": workload.request_sha256,
        },
        workload,
        internal,
    )


def _result_digest(result: object | None) -> str | None:
    if result is None:
        return None
    return sha256(canonical_bytes(result) + b"\n")


def _receipt(
    request: SemanticRequest,
    digest: str,
    *,
    state: str,
    resolved_operation: dict[str, object] | None,
    result: object | None = None,
    refusal_code: str | None = None,
) -> dict[str, object]:
    if state not in RECEIPT_STATES:
        raise ContractRefusal("internal_contract_error", "semantic receipt state is invalid")
    value = {
        "document_type": RECEIPT_DOCUMENT_TYPE,
        "schema_version": RECEIPT_SCHEMA_VERSION,
        "request_id": request.request_id,
        "request_sha256": digest,
        "operation": request.operation,
        "source": _source_document(request.source),
        "state": state,
        "resolved_operation": resolved_operation,
        "result": result,
        "result_sha256": _result_digest(result),
        "refusal_code": refusal_code,
        "authority": dict(AUTHORITY),
    }
    if len(canonical_bytes(value) + b"\n") > MAX_RECEIPT_BYTES:
        raise ContractRefusal("internal_contract_error", "semantic receipt exceeds its fixed ceiling")
    return value


def planned_receipt(compiled: CompiledRequest) -> dict[str, object]:
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="planned",
        resolved_operation=compiled.resolved_operation,
    )


def refused_receipt(request: SemanticRequest, refusal: ContractRefusal) -> dict[str, object]:
    return _receipt(
        request,
        request_sha256(request),
        state="refused",
        resolved_operation=None,
        refusal_code=refusal.code,
    )


def waiting_receipt(compiled: CompiledRequest, reason: str) -> dict[str, object]:
    if not REASON_PATTERN.fullmatch(reason):
        raise ContractRefusal("internal_contract_error", "semantic waiting reason is invalid")
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="waiting",
        resolved_operation=compiled.resolved_operation,
        result={"reason": reason},
    )


def capabilities_receipt(compiled: CompiledRequest) -> dict[str, object]:
    if compiled.request.operation != OP_CAPABILITIES:
        raise ContractRefusal("internal_contract_error", "request is not capabilities")
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="succeeded",
        resolved_operation=compiled.resolved_operation,
        result=capabilities_payload(),
    )


def validate_status_observation(
    observation: dict[str, object],
) -> dict[str, object]:
    expected = {
        "document_type",
        "schema_version",
        "outcome",
        "reason",
        "grants_authority",
        "authorizes_execution",
        "authorizes_redispatch",
    }
    pairs = {
        ("ready", "compatible"),
        ("refused", "observation_unavailable"),
        ("wait", "node_held"),
        ("wait", "node_draining"),
        ("wait", "pressure_high"),
        ("wait", "capacity_unavailable"),
        ("wait", "reserved"),
    }
    if (
        not isinstance(observation, dict)
        or set(observation) != expected
        or observation.get("document_type") != "glaeda-owned-admission-observation"
        or type(observation.get("schema_version")) is not int
        or observation.get("schema_version") != 1
        or not isinstance(observation.get("outcome"), str)
        or not isinstance(observation.get("reason"), str)
        or (observation["outcome"], observation["reason"]) not in pairs
        or observation.get("grants_authority") is not False
        or observation.get("authorizes_execution") is not False
        or observation.get("authorizes_redispatch") is not False
    ):
        raise ContractRefusal("invalid_status_observation", "status observation is outside the reviewed contract")
    return observation


def status_receipt(compiled: CompiledRequest, observation: dict[str, object]) -> dict[str, object]:
    if compiled.request.operation != OP_STATUS:
        raise ContractRefusal("internal_contract_error", "request is not status")
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="succeeded",
        resolved_operation=compiled.resolved_operation,
        result=validate_status_observation(observation),
    )


def _bounded_nonnegative_integer(value: object) -> bool:
    return type(value) is int and 0 <= value <= 2**63 - 1


def _repo_relative_path(value: object) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > REPO_QUERY_MAX_PATH_BYTES
        or value.startswith("/")
        or "\\" in value
        or any(part in {"", ".", ".."} for part in value.split("/"))
        or any(ord(character) < 32 or ord(character) == 127 for character in value)
    ):
        raise ContractRefusal("invalid_repo_query_result", "repo query path is invalid")
    return value


def _project_repo_query_result(
    compiled: CompiledRequest,
    report: dict[str, object],
) -> dict[str, object]:
    if compiled.request.operation != OP_REPO_QUERY or compiled.request.source is None:
        raise ContractRefusal("internal_contract_error", "request is not repo_query")
    raw = canonical_bytes(report) + b"\n"
    source = compiled.request.source
    resolved = compiled.resolved_operation
    expected_keys = {
        "document_type",
        "schema_version",
        "profile_id",
        "profile_generation",
        "authority",
        "request_digest",
        "repository",
        "object_format",
        "requested_base",
        "head",
        "head_tree",
        "merge_base",
        "base_is_ancestor",
        "commits_since_merge_base",
        "changed_files",
        "changed_files_status",
        "changed_files_observed",
        "changed_files_omitted",
        "diff_summary",
        "patch",
        "blobs",
        "path_history",
        "objects",
        "metrics",
    }
    if (
        len(raw) > MAX_REPO_QUERY_RESULT_BYTES
        or not isinstance(report, dict)
        or set(report) != expected_keys
        or report.get("document_type") != "glaeda-resident-repo-query"
        or type(report.get("schema_version")) is not int
        or report.get("schema_version") != 1
        or report.get("profile_id") != "repo-query/v1"
        or report.get("profile_generation") != resolved["profile_generation"]
        or report.get("authority") != "observation_only"
        or report.get("request_digest") != resolved["request_digest"]
        or report.get("repository") != f"github.com/{source.repository}"
        or report.get("object_format") != "sha1"
        or report.get("requested_base") != resolved["base_commit"]
        or report.get("head") != source.commit
        or report.get("head_tree") != source.tree
        or not isinstance(report.get("merge_base"), str)
        or OID_PATTERN.fullmatch(report["merge_base"]) is None
        or type(report.get("base_is_ancestor")) is not bool
        or not _bounded_nonnegative_integer(report.get("commits_since_merge_base"))
        or report.get("changed_files_status") not in {"complete", "truncated", "unknown"}
        or not _bounded_nonnegative_integer(report.get("changed_files_observed"))
        or not _bounded_nonnegative_integer(report.get("changed_files_omitted"))
        or report.get("blobs") != []
        or report.get("path_history") != []
        or report.get("objects") != []
    ):
        raise ContractRefusal("invalid_repo_query_result", "repo query result does not match the semantic request")

    changed = report["changed_files"]
    if not isinstance(changed, list) or len(changed) > REPO_QUERY_MAX_CHANGED_FILES:
        raise ContractRefusal("invalid_repo_query_result", "repo query changed-file evidence is invalid")
    projected_changed: list[dict[str, object]] = []
    for item in changed:
        if not isinstance(item, dict) or set(item) not in (
            {"path", "binary"},
            {"path", "insertions", "deletions", "binary"},
        ):
            raise ContractRefusal("invalid_repo_query_result", "repo query changed-file evidence is invalid")
        binary = item.get("binary")
        if type(binary) is not bool:
            raise ContractRefusal("invalid_repo_query_result", "repo query changed-file evidence is invalid")
        projected: dict[str, object] = {
            "path": _repo_relative_path(item.get("path")),
            "binary": binary,
        }
        if binary:
            if set(item) != {"path", "binary"}:
                raise ContractRefusal("invalid_repo_query_result", "binary changed-file evidence is invalid")
        else:
            if (
                set(item) != {"path", "insertions", "deletions", "binary"}
                or not _bounded_nonnegative_integer(item.get("insertions"))
                or not _bounded_nonnegative_integer(item.get("deletions"))
            ):
                raise ContractRefusal("invalid_repo_query_result", "text changed-file evidence is invalid")
            projected["insertions"] = item["insertions"]
            projected["deletions"] = item["deletions"]
        projected_changed.append(projected)
    if (
        report["changed_files_observed"]
        != len(projected_changed) + report["changed_files_omitted"]
    ):
        raise ContractRefusal("invalid_repo_query_result", "repo query changed-file counts are inconsistent")

    summary = report["diff_summary"]
    if (
        not isinstance(summary, dict)
        or set(summary)
        != {"files_changed", "text_files", "binary_files", "insertions", "deletions"}
        or any(not _bounded_nonnegative_integer(summary.get(key)) for key in summary)
        or summary["files_changed"] != report["changed_files_observed"]
        or summary["text_files"] + summary["binary_files"] != summary["files_changed"]
    ):
        raise ContractRefusal("invalid_repo_query_result", "repo query diff summary is invalid")
    projected_summary = dict(summary)

    patch = report["patch"]
    if not isinstance(patch, dict):
        raise ContractRefusal("invalid_repo_query_result", "repo query patch evidence is invalid")
    allowed_patch = {"bytes", "sha256", "included", "omitted_bytes", "reason", "text"}
    if not {"bytes", "sha256", "included", "omitted_bytes"} <= set(patch) <= allowed_patch:
        raise ContractRefusal("invalid_repo_query_result", "repo query patch evidence is invalid")
    patch_bytes = patch.get("bytes")
    included = patch.get("included")
    omitted_bytes = patch.get("omitted_bytes")
    if (
        not _bounded_nonnegative_integer(patch_bytes)
        or patch_bytes > REPO_QUERY_MAX_GIT_OUTPUT_BYTES
        or not isinstance(patch.get("sha256"), str)
        or SHA256_PATTERN.fullmatch(patch["sha256"]) is None
        or type(included) is not bool
        or not _bounded_nonnegative_integer(omitted_bytes)
    ):
        raise ContractRefusal("invalid_repo_query_result", "repo query patch evidence is invalid")
    projected_patch: dict[str, object] = {
        "bytes": patch_bytes,
        "sha256": patch["sha256"],
        "included": included,
        "omitted_bytes": omitted_bytes,
    }
    if included:
        text = patch.get("text")
        if (
            set(patch) != {"bytes", "sha256", "included", "omitted_bytes", "text"}
            or not isinstance(text, str)
            or len(text.encode("utf-8")) != patch_bytes
            or patch_bytes > resolved["max_patch_bytes"]
            or omitted_bytes != 0
            or sha256(text.encode("utf-8")) != patch["sha256"]
        ):
            raise ContractRefusal("invalid_repo_query_result", "included repo query patch is invalid")
        projected_patch["text"] = text
    else:
        if (
            set(patch) != {"bytes", "sha256", "included", "omitted_bytes", "reason"}
            or patch.get("reason") not in {"patch_byte_limit", "aggregate_response_limit"}
            or omitted_bytes != patch_bytes
        ):
            raise ContractRefusal("invalid_repo_query_result", "omitted repo query patch is invalid")
        projected_patch["reason"] = patch["reason"]

    metrics = report["metrics"]
    if (
        not isinstance(metrics, dict)
        or set(metrics)
        != {
            "git_processes",
            "git_stdout_bytes",
            "git_wall_microseconds",
            "complete_wall_microseconds",
        }
        or any(not _bounded_nonnegative_integer(metrics.get(key)) for key in metrics)
    ):
        raise ContractRefusal("invalid_repo_query_result", "repo query metrics are invalid")

    return {
        "document_type": REPO_QUERY_RESULT_DOCUMENT_TYPE,
        "schema_version": 1,
        "profile_id": "repo-query/v1",
        "profile_generation": resolved["profile_generation"],
        "request_digest": resolved["request_digest"],
        "repository": source.repository,
        "object_format": "sha1",
        "requested_base": resolved["base_commit"],
        "head": source.commit,
        "head_tree": source.tree,
        "merge_base": report["merge_base"],
        "base_is_ancestor": report["base_is_ancestor"],
        "commits_since_merge_base": report["commits_since_merge_base"],
        "changed_files": projected_changed,
        "changed_files_status": report["changed_files_status"],
        "changed_files_observed": report["changed_files_observed"],
        "changed_files_omitted": report["changed_files_omitted"],
        "diff_summary": projected_summary,
        "patch": projected_patch,
        "metrics": dict(metrics),
    }


def repo_query_receipt(compiled: CompiledRequest, report: dict[str, object]) -> dict[str, object]:
    projection = _project_repo_query_result(compiled, report)
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="succeeded",
        resolved_operation=compiled.resolved_operation,
        result=projection,
    )


def ambiguous_receipt(compiled: CompiledRequest) -> dict[str, object]:
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="ambiguous",
        resolved_operation=compiled.resolved_operation,
        refusal_code="ambiguous_execution",
    )


def terminal_verify_receipt(
    compiled: CompiledRequest, workload_receipt: dict[str, object]
) -> dict[str, object]:
    if compiled.request.operation != OP_VERIFY_NAMED or compiled.internal is None:
        raise ContractRefusal("internal_contract_error", "request is not verify_named")
    if not focused.valid_terminal_receipt(workload_receipt, compiled.internal):
        raise ContractRefusal("invalid_workload_receipt", "verification receipt does not match the semantic request")
    result = workload_receipt["result"]
    terminal = result["terminal_class"]
    projection = {
        "workload_receipt_sha256": sha256(canonical_bytes(workload_receipt) + b"\n"),
        "terminal_class": terminal,
        "started_at_unix_millis": result["started_at_unix_millis"],
        "settled_at_unix_millis": result["settled_at_unix_millis"],
        "output_bytes": result["output_bytes"],
        "output_sha256": result["output_sha256"],
        "process_tree_settled": result["process_tree_settled"],
        "task_cleanup_complete": result["task_cleanup_complete"],
    }
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state=terminal,
        resolved_operation=compiled.resolved_operation,
        result=projection,
    )


def _inspect_source(operation: str, value: object) -> None:
    if operation in {OP_CAPABILITIES, OP_STATUS}:
        if value is not None:
            raise ContractRefusal("invalid_receipt", "node receipt carries an unexpected source")
        return
    try:
        _source(value)
    except ContractRefusal as error:
        raise ContractRefusal("invalid_receipt", "semantic receipt source is invalid") from error


def _inspect_resolved(operation: str, resolved: object) -> dict[str, object]:
    if not isinstance(resolved, dict) or resolved.get("kind") != operation:
        raise ContractRefusal("invalid_receipt", "semantic receipt resolution is inconsistent")
    if operation == OP_CAPABILITIES:
        if (
            set(resolved) != {"kind", "contract_generation"}
            or not isinstance(resolved.get("contract_generation"), str)
            or not SHA256_PATTERN.fullmatch(resolved["contract_generation"])
        ):
            raise ContractRefusal("invalid_receipt", "capabilities resolution is invalid")
    elif operation == OP_STATUS:
        if resolved != {
            "kind": OP_STATUS,
            "observation_contract": "glaeda-owned-admission-observation/v1",
            "authority": "observation_only",
        }:
            raise ContractRefusal("invalid_receipt", "status resolution is invalid")
    elif operation == OP_REPO_QUERY:
        if (
            set(resolved)
            != {
                "kind",
                "profile_id",
                "profile_generation",
                "source_class",
                "base_commit",
                "max_patch_bytes",
                "request_digest",
                "result_ceiling_bytes",
                "authority",
            }
            or resolved.get("profile_id") != "repo-query/v1"
            or resolved.get("profile_generation") != repo_query_profile_generation()
            or resolved.get("source_class") != "resident_exact"
            or resolved.get("authority") != "observation_only"
            or not isinstance(resolved.get("base_commit"), str)
            or not OID_PATTERN.fullmatch(resolved["base_commit"])
            or isinstance(resolved.get("max_patch_bytes"), bool)
            or not isinstance(resolved.get("max_patch_bytes"), int)
            or not 0 <= resolved["max_patch_bytes"] <= MAX_REPO_QUERY_PATCH_BYTES
            or not isinstance(resolved.get("request_digest"), str)
            or not SHA256_PATTERN.fullmatch(resolved["request_digest"])
            or resolved.get("result_ceiling_bytes") != MAX_REPO_QUERY_RESULT_BYTES
        ):
            raise ContractRefusal("invalid_receipt", "repo query resolution is invalid")
    elif operation == OP_VERIFY_NAMED:
        if (
            set(resolved)
            != {
                "kind",
                "profile_id",
                "profile_generation",
                "capability_class",
                "resource_class",
                "network_class",
                "environment_class",
                "deadline_seconds",
                "output_ceiling_bytes",
                "workload_request_sha256",
            }
            or resolved.get("profile_id") != focused.FOCUSED_PROFILE.profile_id
            or not isinstance(resolved.get("profile_generation"), str)
            or not SHA256_PATTERN.fullmatch(resolved["profile_generation"])
            or resolved.get("capability_class") != focused.EXECUTION_IDENTITY_CLASS
            or resolved.get("resource_class") != focused.FOCUSED_PROFILE.resource_class
            or resolved.get("network_class") != "none"
            or resolved.get("environment_class") != "minimal"
            or resolved.get("deadline_seconds") != focused.FOCUSED_PROFILE.deadline_seconds
            or resolved.get("output_ceiling_bytes") != focused.MAX_SOURCE_OUTPUT_BYTES
            or not isinstance(resolved.get("workload_request_sha256"), str)
            or not SHA256_PATTERN.fullmatch(resolved["workload_request_sha256"])
        ):
            raise ContractRefusal("invalid_receipt", "verification resolution is invalid")
    return resolved


def _inspect_capabilities_result(result: object, resolved: dict[str, object]) -> None:
    if not isinstance(result, dict) or set(result) != {
        "document_type",
        "schema_version",
        "contract_generation",
        "operations",
        "command_argv",
        "authority",
    }:
        raise ContractRefusal("invalid_receipt", "capabilities result is invalid")
    operations = result.get("operations")
    command = result.get("command_argv")
    if (
        result.get("document_type") != CAPABILITIES_DOCUMENT_TYPE
        or type(result.get("schema_version")) is not int
        or result.get("schema_version") != CAPABILITIES_SCHEMA_VERSION
        or result.get("authority") != "observation_only"
        or not isinstance(result.get("contract_generation"), str)
        or not SHA256_PATTERN.fullmatch(result["contract_generation"])
        or result["contract_generation"] != resolved["contract_generation"]
        or not isinstance(operations, list)
        or len(operations) != 4
        or command
        != {
            "available": False,
            "reason": "no_reviewed_glaeda_command_profile",
        }
    ):
        raise ContractRefusal("invalid_receipt", "capabilities result is invalid")
    by_kind = {
        item.get("kind"): item
        for item in operations
        if isinstance(item, dict) and isinstance(item.get("kind"), str)
    }
    if set(by_kind) != OPERATIONS or len(by_kind) != len(operations):
        raise ContractRefusal("invalid_receipt", "capabilities operation set is invalid")
    if by_kind[OP_CAPABILITIES] != {
        "kind": OP_CAPABILITIES,
        "authority": "observation_only",
    }:
        raise ContractRefusal("invalid_receipt", "capabilities operation entry is invalid")
    if by_kind[OP_STATUS] != {
        "kind": OP_STATUS,
        "authority": "observation_only",
        "observation_contract": "glaeda-owned-admission-observation/v1",
    }:
        raise ContractRefusal("invalid_receipt", "status capability entry is invalid")
    repo = by_kind[OP_REPO_QUERY]
    if repo != {
        "kind": OP_REPO_QUERY,
        "authority": "observation_only",
        "profile_id": "repo-query/v1",
        "profile_generation": repo_query_profile_generation(),
        "source_class": "resident_exact",
        "max_patch_bytes": MAX_REPO_QUERY_PATCH_BYTES,
    }:
        raise ContractRefusal("invalid_receipt", "repo query capability entry is invalid")
    verify = by_kind[OP_VERIFY_NAMED]
    profiles = verify.get("profiles") if isinstance(verify, dict) else None
    if (
        not isinstance(verify, dict)
        or set(verify) != {"kind", "authority", "profiles"}
        or verify.get("authority") != "bounded_execution_request"
        or not isinstance(profiles, list)
        or len(profiles) != 1
        or not isinstance(profiles[0], dict)
        or set(profiles[0]) != {"id", "generation", "capability_class"}
        or profiles[0].get("id") != focused.FOCUSED_PROFILE.profile_id
        or profiles[0].get("capability_class") != focused.EXECUTION_IDENTITY_CLASS
        or not isinstance(profiles[0].get("generation"), str)
        or not SHA256_PATTERN.fullmatch(profiles[0]["generation"])
    ):
        raise ContractRefusal("invalid_receipt", "verification capability entry is invalid")
    contract = {
        "document_type": "glaeda-semantic-contract",
        "schema_version": 1,
        "operations": operations,
        "command_argv": command,
    }
    if sha256(canonical_bytes(contract)) != result["contract_generation"]:
        raise ContractRefusal("invalid_receipt", "capabilities contract generation is invalid")


def _inspect_repo_query_result(
    result: object,
    source: SourceIdentity,
    resolved: dict[str, object],
) -> None:
    expected_keys = {
        "document_type",
        "schema_version",
        "profile_id",
        "profile_generation",
        "request_digest",
        "repository",
        "object_format",
        "requested_base",
        "head",
        "head_tree",
        "merge_base",
        "base_is_ancestor",
        "commits_since_merge_base",
        "changed_files",
        "changed_files_status",
        "changed_files_observed",
        "changed_files_omitted",
        "diff_summary",
        "patch",
        "metrics",
    }
    if (
        not isinstance(result, dict)
        or set(result) != expected_keys
        or result.get("document_type") != REPO_QUERY_RESULT_DOCUMENT_TYPE
        or type(result.get("schema_version")) is not int
        or result.get("schema_version") != 1
        or result.get("profile_id") != "repo-query/v1"
        or result.get("profile_generation") != resolved["profile_generation"]
        or result.get("request_digest") != resolved["request_digest"]
        or result.get("repository") != source.repository
        or result.get("object_format") != "sha1"
        or result.get("requested_base") != resolved["base_commit"]
        or result.get("head") != source.commit
        or result.get("head_tree") != source.tree
        or not isinstance(result.get("merge_base"), str)
        or OID_PATTERN.fullmatch(result["merge_base"]) is None
        or type(result.get("base_is_ancestor")) is not bool
        or not _bounded_nonnegative_integer(result.get("commits_since_merge_base"))
        or result.get("changed_files_status") not in {"complete", "truncated", "unknown"}
        or not _bounded_nonnegative_integer(result.get("changed_files_observed"))
        or not _bounded_nonnegative_integer(result.get("changed_files_omitted"))
    ):
        raise ContractRefusal("invalid_receipt", "repo query result is invalid")
    changed = result["changed_files"]
    if not isinstance(changed, list) or len(changed) > REPO_QUERY_MAX_CHANGED_FILES:
        raise ContractRefusal("invalid_receipt", "repo query changed-file result is invalid")
    for item in changed:
        if not isinstance(item, dict) or set(item) not in (
            {"path", "binary"},
            {"path", "insertions", "deletions", "binary"},
        ):
            raise ContractRefusal("invalid_receipt", "repo query changed-file result is invalid")
        _repo_relative_path(item.get("path"))
        binary = item.get("binary")
        if type(binary) is not bool:
            raise ContractRefusal("invalid_receipt", "repo query changed-file result is invalid")
        if binary:
            if set(item) != {"path", "binary"}:
                raise ContractRefusal("invalid_receipt", "repo query changed-file result is invalid")
        elif (
            set(item) != {"path", "insertions", "deletions", "binary"}
            or not _bounded_nonnegative_integer(item.get("insertions"))
            or not _bounded_nonnegative_integer(item.get("deletions"))
        ):
            raise ContractRefusal("invalid_receipt", "repo query changed-file result is invalid")
    if result["changed_files_observed"] != len(changed) + result["changed_files_omitted"]:
        raise ContractRefusal("invalid_receipt", "repo query changed-file counts are inconsistent")
    summary = result["diff_summary"]
    if (
        not isinstance(summary, dict)
        or set(summary)
        != {"files_changed", "text_files", "binary_files", "insertions", "deletions"}
        or any(not _bounded_nonnegative_integer(summary.get(key)) for key in summary)
        or summary["files_changed"] != result["changed_files_observed"]
        or summary["text_files"] + summary["binary_files"] != summary["files_changed"]
    ):
        raise ContractRefusal("invalid_receipt", "repo query diff summary is invalid")
    patch = result["patch"]
    if not isinstance(patch, dict):
        raise ContractRefusal("invalid_receipt", "repo query patch result is invalid")
    patch_bytes = patch.get("bytes")
    included = patch.get("included")
    omitted_bytes = patch.get("omitted_bytes")
    if (
        not {"bytes", "sha256", "included", "omitted_bytes"} <= set(patch)
        or not set(patch) <= {"bytes", "sha256", "included", "omitted_bytes", "reason", "text"}
        or not _bounded_nonnegative_integer(patch_bytes)
        or patch_bytes > REPO_QUERY_MAX_GIT_OUTPUT_BYTES
        or not isinstance(patch.get("sha256"), str)
        or SHA256_PATTERN.fullmatch(patch["sha256"]) is None
        or type(included) is not bool
        or not _bounded_nonnegative_integer(omitted_bytes)
    ):
        raise ContractRefusal("invalid_receipt", "repo query patch result is invalid")
    if included:
        text = patch.get("text")
        if (
            set(patch) != {"bytes", "sha256", "included", "omitted_bytes", "text"}
            or not isinstance(text, str)
            or len(text.encode("utf-8")) != patch_bytes
            or patch_bytes > resolved["max_patch_bytes"]
            or omitted_bytes != 0
            or sha256(text.encode("utf-8")) != patch["sha256"]
        ):
            raise ContractRefusal("invalid_receipt", "repo query included patch result is invalid")
    elif (
        set(patch) != {"bytes", "sha256", "included", "omitted_bytes", "reason"}
        or patch.get("reason") not in {"patch_byte_limit", "aggregate_response_limit"}
        or omitted_bytes != patch_bytes
    ):
        raise ContractRefusal("invalid_receipt", "repo query omitted patch result is invalid")
    metrics = result["metrics"]
    if (
        not isinstance(metrics, dict)
        or set(metrics)
        != {
            "git_processes",
            "git_stdout_bytes",
            "git_wall_microseconds",
            "complete_wall_microseconds",
        }
        or any(not _bounded_nonnegative_integer(metrics.get(key)) for key in metrics)
    ):
        raise ContractRefusal("invalid_receipt", "repo query metrics result is invalid")


def _inspect_verify_result(result: object, state: str) -> None:
    if (
        not isinstance(result, dict)
        or set(result)
        != {
            "workload_receipt_sha256",
            "terminal_class",
            "started_at_unix_millis",
            "settled_at_unix_millis",
            "output_bytes",
            "output_sha256",
            "process_tree_settled",
            "task_cleanup_complete",
        }
        or result.get("terminal_class") != state
        or not isinstance(result.get("workload_receipt_sha256"), str)
        or not SHA256_PATTERN.fullmatch(result["workload_receipt_sha256"])
        or type(result.get("started_at_unix_millis")) is not int
        or type(result.get("settled_at_unix_millis")) is not int
        or not 0
        <= result["started_at_unix_millis"]
        <= result["settled_at_unix_millis"]
        < 253402300800000
        or type(result.get("output_bytes")) is not int
        or result["output_bytes"] < 0
        or not isinstance(result.get("output_sha256"), str)
        or not SHA256_PATTERN.fullmatch(result["output_sha256"])
        or result.get("process_tree_settled") is not True
        or type(result.get("task_cleanup_complete")) is not bool
        or (state == "cleanup_incomplete") != (result["task_cleanup_complete"] is False)
    ):
        raise ContractRefusal("invalid_receipt", "verification result is invalid")


def _request_from_receipt(value: dict[str, object]) -> SemanticRequest:
    operation = value["operation"]
    resolved = _inspect_resolved(operation, value["resolved_operation"])
    source = None if value["source"] is None else _source(value["source"])
    if operation in {OP_CAPABILITIES, OP_STATUS}:
        parameters: dict[str, object] = {}
    elif operation == OP_REPO_QUERY:
        parameters = {
            "base_commit": resolved["base_commit"],
            "max_patch_bytes": resolved["max_patch_bytes"],
        }
    else:
        parameters = {"profile_id": resolved["profile_id"]}
    return SemanticRequest(value["request_id"], operation, source, parameters)


def _inspect_state(value: dict[str, object]) -> None:
    operation = value["operation"]
    state = value["state"]
    result = value["result"]
    refusal = value["refusal_code"]
    resolved = value["resolved_operation"]

    if state == "refused":
        if resolved is not None or result is not None or not isinstance(refusal, str):
            raise ContractRefusal("invalid_receipt", "refused semantic receipt is inconsistent")
        return

    resolved = _inspect_resolved(operation, resolved)
    if state == "planned":
        if result is not None or refusal is not None:
            raise ContractRefusal("invalid_receipt", "planned semantic receipt is inconsistent")
        return
    if state == "waiting":
        if (
            operation != OP_VERIFY_NAMED
            or not isinstance(result, dict)
            or set(result) != {"reason"}
            or not isinstance(result.get("reason"), str)
            or not REASON_PATTERN.fullmatch(result["reason"])
            or refusal is not None
        ):
            raise ContractRefusal("invalid_receipt", "waiting semantic receipt is inconsistent")
        return
    if state == "ambiguous":
        if operation != OP_VERIFY_NAMED or result is not None or refusal != "ambiguous_execution":
            raise ContractRefusal("invalid_receipt", "ambiguous semantic receipt is inconsistent")
        return

    if refusal is not None or result is None:
        raise ContractRefusal("invalid_receipt", "terminal semantic receipt is inconsistent")
    if operation in {OP_CAPABILITIES, OP_STATUS, OP_REPO_QUERY} and state != "succeeded":
        raise ContractRefusal("invalid_receipt", "read-only semantic receipt has an invalid terminal state")
    if operation == OP_VERIFY_NAMED and state not in {
        "succeeded",
        "failed",
        "timed_out",
        "cleanup_incomplete",
    }:
        raise ContractRefusal("invalid_receipt", "verification semantic receipt has an invalid terminal state")

    if operation == OP_CAPABILITIES:
        _inspect_capabilities_result(result, resolved)
    elif operation == OP_STATUS:
        try:
            validate_status_observation(result)
        except ContractRefusal as error:
            raise ContractRefusal("invalid_receipt", "status result is invalid") from error
    elif operation == OP_REPO_QUERY:
        if value["source"] is None:
            raise ContractRefusal("invalid_receipt", "repo query source is invalid")
        _inspect_repo_query_result(result, _source(value["source"]), resolved)
    else:
        _inspect_verify_result(result, state)


def inspect_receipt(raw: bytes) -> dict[str, object]:
    if len(raw) > MAX_RECEIPT_BYTES:
        raise ContractRefusal("invalid_receipt", "semantic receipt exceeds its fixed ceiling")
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise ContractRefusal("invalid_receipt", "semantic receipt is not valid JSON") from error
    if (
        not isinstance(value, dict)
        or set(value) != RECEIPT_KEYS
        or canonical_bytes(value) + b"\n" != raw
        or value.get("document_type") != RECEIPT_DOCUMENT_TYPE
        or type(value.get("schema_version")) is not int
        or value.get("schema_version") != RECEIPT_SCHEMA_VERSION
        or value.get("authority") != AUTHORITY
        or value.get("state") not in RECEIPT_STATES
        or value.get("operation") not in OPERATIONS
    ):
        raise ContractRefusal("invalid_receipt", "semantic receipt is outside the closed schema")
    parse_request_id(value.get("request_id"))
    _inspect_source(value["operation"], value["source"])
    digest = value.get("request_sha256")
    if not isinstance(digest, str) or not SHA256_PATTERN.fullmatch(digest):
        raise ContractRefusal("invalid_receipt", "semantic receipt request digest is invalid")
    if value["state"] != "refused":
        reconstructed = _request_from_receipt(value)
        if request_sha256(reconstructed) != digest:
            raise ContractRefusal("invalid_receipt", "semantic receipt request digest does not match its request")
        expected_resolution = compile_request(reconstructed).resolved_operation
        if value["resolved_operation"] != expected_resolution:
            raise ContractRefusal(
                "invalid_receipt",
                "semantic receipt resolution does not match its request",
            )
    result = value.get("result")
    expected_result_digest = _result_digest(result)
    if value.get("result_sha256") != expected_result_digest:
        raise ContractRefusal("invalid_receipt", "semantic receipt result digest is invalid")
    _inspect_state(value)
    refusal = value.get("refusal_code")
    if refusal is not None and (
        not isinstance(refusal, str) or not REASON_PATTERN.fullmatch(refusal)
    ):
        raise ContractRefusal("invalid_receipt", "semantic receipt refusal code is invalid")
    return value


def validate_replay(
    existing_receipt: dict[str, object], request: SemanticRequest
) -> dict[str, object]:
    validated = inspect_receipt(canonical_bytes(existing_receipt) + b"\n")
    if validated["request_id"] != request.request_id:
        raise ContractRefusal("request_mismatch", "semantic receipt belongs to another request identity")
    if validated["request_sha256"] != request_sha256(request):
        raise ContractRefusal(
            "request_conflict",
            "semantic request identity was reused with different semantics",
        )
    if (
        validated["operation"] != request.operation
        or validated["source"] != _source_document(request.source)
    ):
        raise ContractRefusal("invalid_receipt", "semantic receipt request binding is inconsistent")
    return existing_receipt
