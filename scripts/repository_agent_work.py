#!/usr/bin/env python3
"""Pure contracts for repository-agent review, repair, and research workloads."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import NoReturn

REQUEST_TYPE = "glaeda-repository-agent-work-request"
PLAN_TYPE = "glaeda-repository-agent-work-plan"
RECEIPT_TYPE = "glaeda-repository-agent-work-receipt"
SCHEMA_VERSION = 1
SEMANTIC_GENERATION = 1
WORKLOAD_FAMILY = "repository_agent_work.v1"
MAX_DOCUMENT_BYTES = 64 * 1024
MAX_REQUEST_BYTES = 16 * 1024
MAX_SCOPE_PATHS = 32
MAX_PATH_BYTES = 256
MAX_VERIFICATION_PROFILES = 4
MAX_REPAIR_CHANGED_PATHS = 64
MAX_REPAIR_PATCH_BYTES = 1024 * 1024
MAX_REVIEW_FINDINGS = 32
MAX_FINDING_SUMMARY_BYTES = 320

OID_RE = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
REPOSITORY_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
IDENTIFIER_RE = re.compile(r"^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$")
OPERATIONS = {"review", "repair", "research"}
NETWORK_CLASSES = {"none", "repository_only", "public_web"}
TERMINAL_STATES = {"completed", "blocked", "failed", "cancelled", "ambiguous"}
REVIEW_CLASSES = {
    "correctness",
    "security",
    "reliability",
    "performance",
    "compatibility",
    "maintainability",
    "test_coverage",
    "contract",
}
REVIEW_SEVERITIES = {"blocker", "high", "medium", "low"}
REPAIR_RESULTS = {"passed", "failed", "timed_out", "ambiguous"}
REPAIR_CHANGE_STATUSES = {"modified", "added", "deleted"}
RESEARCH_RESULTS = {"answered", "partial"}

AUTHORITY = {
    "authorizes_publication": False,
    "authorizes_merge": False,
    "authorizes_release": False,
    "authorizes_deploy": False,
    "authorizes_canonical_source_mutation": False,
    "authorizes_host_selection": False,
    "authorizes_redispatch": False,
}


class ContractRefusal(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")


def sha256(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(value)


def decode_document(raw: bytes, *, ceiling: int) -> object:
    if not raw or len(raw) > ceiling:
        raise ContractRefusal("invalid_document_size", "document exceeds its fixed byte bounds")
    try:
        return json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise ContractRefusal("invalid_json", "document is not valid JSON") from error


def exact_keys(value: dict[str, object], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise ContractRefusal("invalid_fields", f"{label} has unknown or missing fields")


def optional_keys(
    value: dict[str, object], required: set[str], optional: set[str], label: str
) -> None:
    if not required <= set(value) <= required | optional:
        raise ContractRefusal("invalid_fields", f"{label} has unknown or missing fields")


def require_sha256(value: object, label: str) -> str:
    if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
        raise ContractRefusal("invalid_digest", f"{label} must be canonical SHA-256")
    return value


def require_oid(value: object, label: str) -> str:
    if not isinstance(value, str) or OID_RE.fullmatch(value) is None:
        raise ContractRefusal("invalid_source", f"{label} must be an exact Git object id")
    return value


def require_repository(value: object) -> str:
    if not isinstance(value, str) or REPOSITORY_RE.fullmatch(value) is None:
        raise ContractRefusal(
            "invalid_repository", "repository must use canonical owner/name identity"
        )
    return value


def require_identifier(value: object, label: str) -> str:
    if not isinstance(value, str) or IDENTIFIER_RE.fullmatch(value) is None:
        raise ContractRefusal("invalid_identifier", f"{label} is invalid")
    return value


def exact_source(value: object, label: str) -> dict[str, str]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_source", f"{label} must be an object")
    exact_keys(value, {"commit", "tree"}, label)
    return {
        "commit": require_oid(value["commit"], f"{label}.commit"),
        "tree": require_oid(value["tree"], f"{label}.tree"),
    }


def source_identity(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_source", "source must be an object")
    optional_keys(value, {"repository", "head"}, {"base"}, "source")
    source: dict[str, object] = {
        "repository": require_repository(value["repository"]),
        "head": exact_source(value["head"], "source.head"),
    }
    if "base" in value:
        source["base"] = exact_source(value["base"], "source.base")
    return source


def repository_path(value: object) -> str:
    if not isinstance(value, str):
        raise ContractRefusal("invalid_path", "repository path must be text")
    if (
        not value
        or len(value.encode("utf-8")) > MAX_PATH_BYTES
        or not value.isascii()
        or value.startswith("/")
        or value.endswith("/")
        or "\\" in value
        or any(ord(char) < 32 or ord(char) == 127 for char in value)
    ):
        raise ContractRefusal("invalid_path", "repository path is not canonical")
    parts = value.split("/")
    # Case-folded, because the control plane runs on a case-insensitive
    # filesystem where `.GIT/config` resolves to `.git/config`.
    if any(part.lower() in {"", ".", "..", ".git"} for part in parts):
        raise ContractRefusal("invalid_path", "repository path contains an unsafe component")
    return value


def path_within(scope_path: str, candidate: str) -> bool:
    return candidate == scope_path or candidate.startswith(scope_path + "/")


def scope_contract(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_scope", "scope must be an object")
    kind = value.get("class")
    if kind in {"whole_repository", "changed_paths"}:
        exact_keys(value, {"class"}, "scope")
        return {"class": kind}
    if kind != "bounded_paths":
        raise ContractRefusal("invalid_scope", "scope class is unsupported")
    exact_keys(value, {"class", "paths"}, "scope")
    paths = value["paths"]
    if not isinstance(paths, list) or not 1 <= len(paths) <= MAX_SCOPE_PATHS:
        raise ContractRefusal("invalid_scope", "bounded path scope has invalid count")
    canonical = sorted(repository_path(path) for path in paths)
    if len(set(canonical)) != len(canonical):
        raise ContractRefusal("invalid_scope", "bounded path scope has duplicates")
    return {"class": "bounded_paths", "paths": canonical}


def mutation_contract(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_mutation", "mutation must be an object")
    kind = value.get("class")
    if kind == "read_only":
        exact_keys(value, {"class"}, "mutation")
        return {"class": "read_only"}
    if kind != "task_private_patch":
        raise ContractRefusal("invalid_mutation", "mutation class is unsupported")
    exact_keys(
        value,
        {
            "class",
            "max_changed_paths",
            "max_patch_bytes",
            "allow_new_files",
            "allow_deletes",
        },
        "mutation",
    )
    max_changed_paths = value["max_changed_paths"]
    max_patch_bytes = value["max_patch_bytes"]
    if (
        type(max_changed_paths) is not int
        or not 1 <= max_changed_paths <= MAX_REPAIR_CHANGED_PATHS
        or type(max_patch_bytes) is not int
        or not 1 <= max_patch_bytes <= MAX_REPAIR_PATCH_BYTES
        or type(value["allow_new_files"]) is not bool
        or type(value["allow_deletes"]) is not bool
    ):
        raise ContractRefusal("invalid_mutation", "task-private patch bounds are invalid")
    return {
        "class": "task_private_patch",
        "max_changed_paths": max_changed_paths,
        "max_patch_bytes": max_patch_bytes,
        "allow_new_files": value["allow_new_files"],
        "allow_deletes": value["allow_deletes"],
    }


def verification_profile(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_verification", "verification profile must be an object")
    exact_keys(value, {"id", "generation"}, "verification profile")
    profile_id = require_identifier(value["id"], "verification profile id")
    generation = value["generation"]
    if type(generation) is not int or not 1 <= generation <= 1_000_000_000_000:
        raise ContractRefusal(
            "invalid_verification", "verification generation is outside the fixed range"
        )
    return {"id": profile_id, "generation": generation}


def verification_set(value: object) -> list[dict[str, object]]:
    if not isinstance(value, list) or len(value) > MAX_VERIFICATION_PROFILES:
        raise ContractRefusal("invalid_verification", "verification set exceeds its bound")
    profiles = [verification_profile(item) for item in value]
    profiles.sort(key=lambda item: (item["id"], item["generation"]))
    keys = [(item["id"], item["generation"]) for item in profiles]
    if len(set(keys)) != len(keys):
        raise ContractRefusal("invalid_verification", "verification set contains duplicates")
    return profiles


def normalize_request(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_request", "request must be an object")
    exact_keys(
        value,
        {
            "document_type",
            "schema_version",
            "operation",
            "source",
            "task_contract_sha256",
            "scope",
            "network_class",
            "mutation",
            "verification",
        },
        "request",
    )
    if value["document_type"] != REQUEST_TYPE or value["schema_version"] != SCHEMA_VERSION:
        raise ContractRefusal("unsupported_schema", "request schema is unsupported")
    operation = value["operation"]
    if operation not in OPERATIONS:
        raise ContractRefusal("unsupported_operation", "repository agent operation is unsupported")
    network = value["network_class"]
    if network not in NETWORK_CLASSES:
        raise ContractRefusal("invalid_network", "network class is unsupported")

    source = source_identity(value["source"])
    scope = scope_contract(value["scope"])
    mutation = mutation_contract(value["mutation"])
    verification = verification_set(value["verification"])
    task_digest = require_sha256(value["task_contract_sha256"], "task contract")

    if operation == "review":
        if "base" not in source:
            raise ContractRefusal("review_base_required", "review requires an exact comparison base")
        if mutation["class"] != "read_only":
            raise ContractRefusal("read_only_required", "review is read-only")
        if network == "public_web":
            raise ContractRefusal("review_public_web_refused", "review v1 has no public-web access")
        if verification:
            raise ContractRefusal(
                "review_verification_refused",
                "review may suggest verification but cannot request execution",
            )
    elif operation == "repair":
        if scope["class"] != "bounded_paths":
            raise ContractRefusal("repair_scope_required", "repair requires bounded path scope")
        if mutation["class"] != "task_private_patch":
            raise ContractRefusal(
                "repair_mutation_required", "repair requires task-private patch authority"
            )
        if network == "public_web":
            raise ContractRefusal(
                "repair_public_web_refused",
                "repair v1 does not combine mutation with public-web access",
            )
        if not verification:
            raise ContractRefusal(
                "repair_verification_required",
                "repair requires exact repository-owned verification profiles",
            )
    else:
        if mutation["class"] != "read_only":
            raise ContractRefusal("read_only_required", "research is read-only")
        if verification:
            raise ContractRefusal(
                "research_verification_refused", "research has no verification execution authority"
            )
        if scope["class"] == "changed_paths" and "base" not in source:
            raise ContractRefusal(
                "research_base_required", "changed-path research requires exact comparison base"
            )

    return {
        "document_type": REQUEST_TYPE,
        "schema_version": SCHEMA_VERSION,
        "operation": operation,
        "source": source,
        "task_contract_sha256": task_digest,
        "scope": scope,
        "network_class": network,
        "mutation": mutation,
        "verification": verification,
    }


def decode_request(raw: bytes) -> dict[str, object]:
    return normalize_request(decode_document(raw, ceiling=MAX_REQUEST_BYTES))


def request_sha256(request: dict[str, object]) -> str:
    return sha256(canonical_bytes(request))


def output_contract_sha256(operation: str) -> str:
    names = {
        "review": "repository-agent-review-result/v1",
        "repair": "repository-agent-repair-result/v1",
        "research": "repository-agent-research-result/v1",
    }
    return sha256(names[operation].encode("ascii"))


def compute_workload(request: dict[str, object]) -> dict[str, object]:
    operation = request["operation"]
    network = request["network_class"]
    capabilities = {"agent.harness", "repository.read", "repository.query"}
    trust_class = "trusted"
    if operation == "repair":
        trust_class = "ultra_trusted"
        capabilities.update({"repository.task_private_write", "verification.repository_owned"})
    if network == "public_web":
        capabilities.add("network.public")
    digest = request_sha256(request)
    return {
        "family": WORKLOAD_FAMILY,
        "semantic_generation": SEMANTIC_GENERATION,
        "input_identity": digest,
        "trust_class": trust_class,
        "required_capabilities": sorted(capabilities),
        "output_contract_sha256": output_contract_sha256(operation),
    }


def _count_class(value: int) -> str:
    if value <= 1:
        return "one"
    if value <= 4:
        return "small"
    if value <= 16:
        return "medium"
    return "large"


def _patch_bytes_class(value: int) -> str:
    if value <= 16 * 1024:
        return "tiny"
    if value <= 64 * 1024:
        return "small"
    if value <= 256 * 1024:
        return "medium"
    return "large"


def routing_classification(request: dict[str, object]) -> dict[str, object]:
    compute = compute_workload(request)
    scope = request["scope"]
    mutation = request["mutation"]
    scope_size_class = (
        _count_class(len(scope["paths"]))
        if scope["class"] == "bounded_paths"
        else scope["class"]
    )
    mutation_budget = (
        {
            "changed_paths": _count_class(mutation["max_changed_paths"]),
            "patch_bytes": _patch_bytes_class(mutation["max_patch_bytes"]),
            "allow_new_files": mutation["allow_new_files"],
            "allow_deletes": mutation["allow_deletes"],
        }
        if mutation["class"] == "task_private_patch"
        else {"class": "read_only"}
    )
    return {
        "family": compute["family"],
        "semantic_generation": compute["semantic_generation"],
        "repository": request["source"]["repository"],
        "operation": request["operation"],
        "trust_class": compute["trust_class"],
        "network_class": request["network_class"],
        "scope_class": scope["class"],
        "scope_size_class": scope_size_class,
        "mutation_class": mutation["class"],
        "mutation_budget_class": mutation_budget,
        "required_capabilities": compute["required_capabilities"],
        "verification_profiles": request["verification"],
        "output_contract_sha256": compute["output_contract_sha256"],
    }


def routing_classification_sha256(request: dict[str, object]) -> str:
    return sha256(canonical_bytes(routing_classification(request)))


def plan(request: dict[str, object]) -> dict[str, object]:
    return {
        "document_type": PLAN_TYPE,
        "schema_version": SCHEMA_VERSION,
        "request_sha256": request_sha256(request),
        "operation": request["operation"],
        "source": dict(request["source"]),
        "compute_workload": compute_workload(request),
        "routing_classification": routing_classification(request),
        "routing_classification_sha256": routing_classification_sha256(request),
        "execution_boundary": {
            "task_contract": "caller_owned_digest_bound",
            "agent_harness": "reviewed_adapter_required",
            "repository_expansion": "bounded_exact_source",
            "machine_selection": "glaeda_owned",
            "physical_attempt": "separate_receipt",
        },
        "authority": dict(AUTHORITY),
    }


def path_admitted(scope: dict[str, object], path: str) -> bool:
    if scope["class"] in {"whole_repository", "changed_paths"}:
        return True
    return any(path_within(prefix, path) for prefix in scope["paths"])


def normalize_finding(value: object, request: dict[str, object]) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_finding", "review finding must be an object")
    required = {"finding_sha256", "class", "severity", "path", "summary", "evidence_sha256"}
    optional = {"line_start", "line_end", "suggested_validation"}
    optional_keys(value, required, optional, "review finding")
    if value["class"] not in REVIEW_CLASSES or value["severity"] not in REVIEW_SEVERITIES:
        raise ContractRefusal("invalid_finding", "review finding class or severity is invalid")
    path = repository_path(value["path"])
    if not path_admitted(request["scope"], path):
        raise ContractRefusal("finding_outside_scope", "review finding lies outside request scope")
    summary = value["summary"]
    if (
        not isinstance(summary, str)
        or not summary
        or len(summary.encode("utf-8")) > MAX_FINDING_SUMMARY_BYTES
        or any(ord(char) < 32 and char != "	" for char in summary)
    ):
        raise ContractRefusal("invalid_finding", "review finding summary is invalid")
    line_start = value.get("line_start")
    line_end = value.get("line_end")
    if (line_start is None) != (line_end is None):
        raise ContractRefusal("invalid_finding", "review line range must be paired")
    if line_start is not None and (
        type(line_start) is not int
        or type(line_end) is not int
        or line_start < 1
        or line_end < line_start
    ):
        raise ContractRefusal("invalid_finding", "review line range is invalid")
    suggested = value.get("suggested_validation")
    if suggested is not None:
        suggested = verification_profile(suggested)
    identity = {
        "class": value["class"],
        "severity": value["severity"],
        "path": path,
        "summary": summary,
        "evidence_sha256": require_sha256(value["evidence_sha256"], "finding evidence"),
        "suggested_validation": suggested,
    }
    if line_start is not None:
        identity["line_start"] = line_start
        identity["line_end"] = line_end
    expected_digest = sha256(canonical_bytes(identity))
    if value["finding_sha256"] != expected_digest:
        raise ContractRefusal("finding_digest_mismatch", "review finding digest is invalid")
    return {"finding_sha256": expected_digest, **identity}


def normalize_review_result(value: object, request: dict[str, object]) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_result", "review result must be an object")
    exact_keys(value, {"kind", "findings"}, "review result")
    if value["kind"] != "review" or not isinstance(value["findings"], list):
        raise ContractRefusal("invalid_result", "review result kind/findings are invalid")
    if len(value["findings"]) > MAX_REVIEW_FINDINGS:
        raise ContractRefusal("invalid_result", "review finding count exceeds its bound")
    findings = [normalize_finding(item, request) for item in value["findings"]]
    findings.sort(key=lambda item: item["finding_sha256"])
    if len({item["finding_sha256"] for item in findings}) != len(findings):
        raise ContractRefusal("invalid_result", "review result contains duplicate findings")
    return {"kind": "review", "findings": findings}


def normalize_verification_result(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_result", "verification result must be an object")
    exact_keys(
        value,
        {"profile", "source", "semantic_result", "result_sha256"},
        "verification result",
    )
    semantic_result = value["semantic_result"]
    if semantic_result not in REPAIR_RESULTS:
        raise ContractRefusal("invalid_result", "repair verification result is unsupported")
    return {
        "profile": verification_profile(value["profile"]),
        "source": exact_source(value["source"], "verification result source"),
        "semantic_result": semantic_result,
        "result_sha256": require_sha256(value["result_sha256"], "verification result"),
    }


def normalize_repair_change(
    value: object, request: dict[str, object]
) -> dict[str, str]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_result", "repair change must be an object")
    exact_keys(value, {"path", "status"}, "repair change")
    path = repository_path(value["path"])
    if not path_admitted(request["scope"], path):
        raise ContractRefusal("repair_path_outside_scope", "repair changed a path outside its scope")
    status = value["status"]
    if status not in REPAIR_CHANGE_STATUSES:
        raise ContractRefusal("invalid_result", "repair change status is unsupported")
    mutation = request["mutation"]
    if status == "added" and not mutation["allow_new_files"]:
        raise ContractRefusal("repair_new_file_refused", "repair added a file without authority")
    if status == "deleted" and not mutation["allow_deletes"]:
        raise ContractRefusal("repair_delete_refused", "repair deleted a file without authority")
    return {"path": path, "status": status}


def normalize_repair_result(value: object, request: dict[str, object]) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_result", "repair result must be an object")
    exact_keys(
        value,
        {
            "kind",
            "result_source",
            "changes",
            "patch_sha256",
            "patch_bytes",
            "verification_results",
            "working_copy",
        },
        "repair result",
    )
    if value["kind"] != "repair" or value["working_copy"] != "clean":
        raise ContractRefusal("invalid_result", "completed repair must end with a clean task workspace")
    result_source = exact_source(value["result_source"], "result_source")
    if (
        result_source["commit"] == request["source"]["head"]["commit"]
        or result_source["tree"] == request["source"]["head"]["tree"]
    ):
        raise ContractRefusal(
            "repair_source_unchanged",
            "completed repair must produce a different exact source",
        )
    changes = value["changes"]
    mutation = request["mutation"]
    if (
        not isinstance(changes, list)
        or not changes
        or len(changes) > mutation["max_changed_paths"]
    ):
        raise ContractRefusal("invalid_result", "repair changed-path count is invalid")
    normalized_changes = [normalize_repair_change(item, request) for item in changes]
    normalized_changes.sort(key=lambda item: item["path"])
    if len({item["path"] for item in normalized_changes}) != len(normalized_changes):
        raise ContractRefusal("invalid_result", "repair result contains duplicate paths")
    patch_bytes = value["patch_bytes"]
    if (
        type(patch_bytes) is not int
        or patch_bytes < 1
        or patch_bytes > mutation["max_patch_bytes"]
    ):
        raise ContractRefusal("invalid_result", "repair patch byte count exceeds its authority")
    patch_sha256 = require_sha256(value["patch_sha256"], "repair patch")
    results = value["verification_results"]
    if not isinstance(results, list):
        raise ContractRefusal("invalid_result", "repair verification results must be a list")
    verification_results = [normalize_verification_result(item) for item in results]
    verification_results.sort(key=lambda item: (item["profile"]["id"], item["profile"]["generation"]))
    actual_profiles = [item["profile"] for item in verification_results]
    if actual_profiles != request["verification"]:
        raise ContractRefusal(
            "repair_verification_mismatch",
            "completed repair must bind exactly the requested verification set",
        )
    if any(item["source"] != result_source for item in verification_results):
        raise ContractRefusal(
            "repair_verification_source_mismatch",
            "completed repair verification must bind the exact resulting source",
        )
    if any(item["semantic_result"] != "passed" for item in verification_results):
        raise ContractRefusal(
            "repair_verification_failed", "completed repair requires all requested verification to pass"
        )
    return {
        "kind": "repair",
        "result_source": result_source,
        "changes": normalized_changes,
        "patch_sha256": patch_sha256,
        "patch_bytes": patch_bytes,
        "verification_results": verification_results,
        "working_copy": "clean",
    }

def normalize_research_result(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_result", "research result must be an object")
    exact_keys(
        value,
        {
            "kind",
            "result_class",
            "artifact_sha256",
            "citation_manifest_sha256",
            "citation_count",
            "evidence_source_count",
        },
        "research result",
    )
    if value["kind"] != "research" or value["result_class"] not in RESEARCH_RESULTS:
        raise ContractRefusal("invalid_result", "research result class is invalid")
    if (
        type(value["citation_count"]) is not int
        or value["citation_count"] < 1
        or type(value["evidence_source_count"]) is not int
        or value["evidence_source_count"] < 1
    ):
        raise ContractRefusal("invalid_result", "completed research requires cited evidence")
    return {
        "kind": "research",
        "result_class": value["result_class"],
        "artifact_sha256": require_sha256(value["artifact_sha256"], "research artifact"),
        "citation_manifest_sha256": require_sha256(
            value["citation_manifest_sha256"], "citation manifest"
        ),
        "citation_count": value["citation_count"],
        "evidence_source_count": value["evidence_source_count"],
    }


def normalize_receipt(value: object, request: dict[str, object]) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_receipt", "receipt must be an object")
    exact_keys(
        value,
        {
            "document_type",
            "schema_version",
            "request_sha256",
            "operation",
            "source",
            "state",
            "result",
            "authority",
        },
        "receipt",
    )
    if value["document_type"] != RECEIPT_TYPE or value["schema_version"] != SCHEMA_VERSION:
        raise ContractRefusal("unsupported_schema", "receipt schema is unsupported")
    if value["request_sha256"] != request_sha256(request):
        raise ContractRefusal("request_mismatch", "receipt request digest differs from request")
    if value["operation"] != request["operation"] or value["source"] != request["source"]:
        raise ContractRefusal("request_mismatch", "receipt operation/source differs from request")
    if value["state"] not in TERMINAL_STATES:
        raise ContractRefusal("invalid_receipt", "receipt terminal state is unsupported")
    if value["authority"] != AUTHORITY:
        raise ContractRefusal("invalid_authority", "repository agent receipt grants unsupported authority")
    result = value["result"]
    if value["state"] != "completed":
        if result is not None:
            raise ContractRefusal("invalid_receipt", "non-completed receipt must not claim a result")
        normalized_result = None
    else:
        if request["operation"] == "review":
            normalized_result = normalize_review_result(result, request)
        elif request["operation"] == "repair":
            normalized_result = normalize_repair_result(result, request)
        else:
            normalized_result = normalize_research_result(result)
    return {
        "document_type": RECEIPT_TYPE,
        "schema_version": SCHEMA_VERSION,
        "request_sha256": request_sha256(request),
        "operation": request["operation"],
        "source": dict(request["source"]),
        "state": value["state"],
        "result": normalized_result,
        "authority": dict(AUTHORITY),
    }


def decode_receipt(raw: bytes, request: dict[str, object]) -> dict[str, object]:
    return normalize_receipt(decode_document(raw, ceiling=MAX_DOCUMENT_BYTES), request)


def cli_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("plan")
    validate = commands.add_parser("validate-receipt")
    validate.add_argument("--request", required=True)
    return parser


def main() -> int:
    args = cli_parser().parse_args()
    try:
        if args.command == "plan":
            request = decode_request(sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1))
            sys.stdout.buffer.write(canonical_bytes(plan(request)) + b"\n")
            return 0
        if args.command == "validate-receipt":
            request = decode_request(Path(args.request).read_bytes())
            receipt = decode_receipt(sys.stdin.buffer.read(MAX_DOCUMENT_BYTES + 1), request)
            sys.stdout.buffer.write(canonical_bytes(receipt) + b"\n")
            return 0
        raise AssertionError(args.command)
    except (ContractRefusal, OSError) as error:
        code = error.code if isinstance(error, ContractRefusal) else "io_error"
        print(f"repository agent work refused [{code}]: {error}", file=sys.stderr)
        return 64


if __name__ == "__main__":
    raise SystemExit(main())
