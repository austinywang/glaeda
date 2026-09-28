#!/usr/bin/env python3
"""Caller-neutral Glaeda adapter for CMUX-owned workload profiles."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn

REQUEST_TYPE = "glaeda-cmux-workload-request"
PLAN_TYPE = "glaeda-cmux-workload-plan"
OBSERVATION_TYPE = "glaeda-cmux-workload-observation"
SCHEMA_VERSION = 1
MAX_DOCUMENT_BYTES = 16 * 1024
CMUX_REPOSITORY = "manaflow-ai/cmux"
CMUX_RUNNER = "scripts/ci/cmux_workload_profile.py"
CMUX_RESULT_CONTRACT = "cmux-workload-result/v1"
OID_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
PROFILE_RE = re.compile(r"^cmux\.[a-z0-9][a-z0-9.-]{1,80}$")
TOKEN_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,127}$")
PARAM_RE = re.compile(r"^[a-z][a-z0-9_]{0,31}$")
STATE_CLASSES = {
    "cold",
    "dependency-warm",
    "compiler-warm",
    "exact-product-reuse",
    "resident-hot",
}
SEMANTIC_RESULTS = {"passed", "failed", "timed_out", "ambiguous"}
OUTER_STATES = {
    "passed": "succeeded",
    "failed": "failed",
    "timed_out": "timed_out",
    "ambiguous": "ambiguous",
}
AUTHORITY = {
    "authorizes_execution": False,
    "authorizes_host_selection": False,
    "authorizes_resource_override": False,
    "authorizes_redispatch": False,
}


class ContractRefusal(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class Request:
    external_request_ref: str
    repository: str
    commit: str
    tree: str
    profile_id: str
    profile_generation: int
    benchmark_state_class: str
    parameters: dict[str, int]
    reuse_hint: str | None
    work_ref: str | None


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")


def sha256(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(value)


def token(value: object, label: str) -> str:
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise ContractRefusal("invalid_request", f"{label} is invalid")
    return value


def decode_json(raw: bytes, ceiling: int = MAX_DOCUMENT_BYTES) -> object:
    if len(raw) > ceiling:
        raise ContractRefusal("invalid_document", "document exceeds its fixed ceiling")
    try:
        return json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise ContractRefusal("invalid_document", "document is not valid JSON") from error


def decode_request(raw: bytes) -> Request:
    value = decode_json(raw)
    required = {
        "document_type",
        "schema_version",
        "external_request_ref",
        "source",
        "profile",
        "benchmark_state_class",
    }
    optional = {"parameters", "reuse_hint", "correlation"}
    if not isinstance(value, dict) or not required <= set(value) <= required | optional:
        raise ContractRefusal("invalid_request", "request fields are invalid")
    if (
        value["document_type"] != REQUEST_TYPE
        or type(value["schema_version"]) is not int
        or value["schema_version"] != SCHEMA_VERSION
    ):
        raise ContractRefusal("unsupported_schema", "request schema is unsupported")
    source = value["source"]
    if not isinstance(source, dict) or set(source) != {"repository", "commit", "tree"}:
        raise ContractRefusal("invalid_request", "source identity is invalid")
    if source["repository"] != CMUX_REPOSITORY:
        raise ContractRefusal("unsupported_repository", "repository is unsupported")
    if (
        not isinstance(source["commit"], str)
        or OID_RE.fullmatch(source["commit"]) is None
        or not isinstance(source["tree"], str)
        or OID_RE.fullmatch(source["tree"]) is None
    ):
        raise ContractRefusal("invalid_request", "source commit/tree identity is invalid")
    profile = value["profile"]
    if not isinstance(profile, dict) or set(profile) != {"id", "generation"}:
        raise ContractRefusal("invalid_request", "profile identity is invalid")
    if not isinstance(profile["id"], str) or PROFILE_RE.fullmatch(profile["id"]) is None:
        raise ContractRefusal("invalid_request", "profile id is invalid")
    if (
        type(profile["generation"]) is not int
        or profile["generation"] < 1
        or profile["generation"] > 2**31 - 1
    ):
        raise ContractRefusal("invalid_request", "profile generation is invalid")
    state_class = value["benchmark_state_class"]
    if not isinstance(state_class, str) or state_class not in STATE_CLASSES:
        raise ContractRefusal("invalid_request", "benchmark state class is invalid")
    parameters_value = value.get("parameters", {})
    if not isinstance(parameters_value, dict) or len(parameters_value) > 8:
        raise ContractRefusal("invalid_request", "profile parameters are invalid")
    parameters: dict[str, int] = {}
    for name, parameter in parameters_value.items():
        if (
            not isinstance(name, str)
            or PARAM_RE.fullmatch(name) is None
            or type(parameter) is not int
            or parameter < -(2**31)
            or parameter > 2**31 - 1
        ):
            raise ContractRefusal("invalid_request", "profile parameter is invalid")
        parameters[name] = parameter
    reuse_hint = value.get("reuse_hint")
    if reuse_hint is not None and reuse_hint not in {"no_preference", "prefer_valid_reuse"}:
        raise ContractRefusal("invalid_request", "reuse hint is invalid")
    work_ref = None
    correlation = value.get("correlation")
    if correlation is not None:
        if not isinstance(correlation, dict) or set(correlation) != {"work_ref"}:
            raise ContractRefusal("invalid_request", "correlation is invalid")
        work_ref = token(correlation["work_ref"], "work reference")
    return Request(
        external_request_ref=token(
            value["external_request_ref"], "external request reference"
        ),
        repository=source["repository"],
        commit=source["commit"],
        tree=source["tree"],
        profile_id=profile["id"],
        profile_generation=profile["generation"],
        benchmark_state_class=state_class,
        parameters=parameters,
        reuse_hint=reuse_hint,
        work_ref=work_ref,
    )


def request_document(request: Request) -> dict[str, object]:
    document: dict[str, object] = {
        "document_type": REQUEST_TYPE,
        "schema_version": SCHEMA_VERSION,
        "external_request_ref": request.external_request_ref,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "profile": {
            "id": request.profile_id,
            "generation": request.profile_generation,
        },
        "benchmark_state_class": request.benchmark_state_class,
    }
    if request.parameters:
        document["parameters"] = request.parameters
    if request.reuse_hint is not None:
        document["reuse_hint"] = request.reuse_hint
    if request.work_ref is not None:
        document["correlation"] = {"work_ref": request.work_ref}
    return document


def execution_binding(request: Request) -> str:
    return sha256(
        canonical_bytes(
            {
                "domain": "glaeda-cmux-repository-profile-binding-v1",
                "source": {
                    "repository": request.repository,
                    "commit": request.commit,
                    "tree": request.tree,
                },
                "profile": {
                    "id": request.profile_id,
                    "generation": request.profile_generation,
                },
                "benchmark_state_class": request.benchmark_state_class,
                "parameters": request.parameters,
                "adapter": {
                    "kind": "cmux-repository-profile/v1",
                    "runner": CMUX_RUNNER,
                    "result_contract": CMUX_RESULT_CONTRACT,
                },
            }
        )
    )


def plan(request: Request) -> dict[str, object]:
    result: dict[str, object] = {
        "document_type": PLAN_TYPE,
        "schema_version": SCHEMA_VERSION,
        "external_request_ref": request.external_request_ref,
        "request_sha256": sha256(canonical_bytes(request_document(request))),
        "execution_binding_sha256": execution_binding(request),
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "profile": {
            "id": request.profile_id,
            "generation": request.profile_generation,
        },
        "benchmark_state_class": request.benchmark_state_class,
        "parameters": request.parameters,
        "state": "planned",
        "resolved_adapter": {
            "kind": "cmux-repository-profile/v1",
            "repository_runner": CMUX_RUNNER,
            "semantic_result_contract": CMUX_RESULT_CONTRACT,
        },
        "physical_preflight": [
            "materialize_exact_source",
            "cmux_runner_plan_matches_frozen_source_and_profile",
            "cmux_runner_owns_semantic_validation",
        ],
        "authority": AUTHORITY,
    }
    if request.work_ref is not None:
        result["correlation"] = {"work_ref": request.work_ref}
    return result


def _exact_keys(value: object, expected: set[str], label: str) -> dict[str, object]:
    if not isinstance(value, dict) or set(value) != expected:
        raise ContractRefusal("invalid_result", f"{label} fields are invalid")
    return value


def _cmux_semantic_key(result: dict[str, object]) -> str:
    source = result["source"]
    profile = result["profile"]
    runtime_inputs = result["runtime_input_identities"]
    assert isinstance(source, dict)
    assert isinstance(profile, dict)
    assert isinstance(runtime_inputs, list)
    return sha256(
        canonical_bytes(
            {
                "source": {
                    "repository": source["repository"],
                    "tree": source["tree"],
                },
                "profile": {
                    "id": profile["id"],
                    "generation": profile["generation"],
                },
                "semantic_validator": result["semantic_validator"],
                "environment_class": result["environment_class"],
                "parameters": result["parameters"],
                "runtime_inputs": [
                    {
                        "name": item["name"],
                        "class": item["class"],
                        "identity": item["identity"],
                        "sha256": item["sha256"],
                    }
                    for item in runtime_inputs
                    if isinstance(item, dict)
                ],
            }
        )
    )


def _cmux_context_key(semantic_key: str, state_class: str, toolchain: str) -> str:
    return sha256(
        canonical_bytes(
            {
                "semantic_key": semantic_key,
                "state_class": state_class,
                "toolchain_identity": toolchain,
            }
        )
    )


def validate_cmux_result(value: dict[str, object]) -> dict[str, object]:
    _exact_keys(
        value,
        {
            "document_type",
            "schema_version",
            "source",
            "profile",
            "semantic_validator",
            "environment_class",
            "expected_result_class",
            "result",
            "parameters",
            "runtime_input_identities",
            "artifact_identities",
            "validation",
            "stage_timings",
            "resource_summary",
            "toolchain",
            "benchmark",
            "network_class",
            "timeout_class",
            "cleanup",
            "exit_code",
            "started_at_unix_millis",
            "ended_at_unix_millis",
        },
        "CMUX semantic result",
    )
    source = _exact_keys(
        value["source"],
        {"repository", "commit", "tree"},
        "CMUX semantic result source",
    )
    profile = _exact_keys(
        value["profile"],
        {"id", "generation"},
        "CMUX semantic result profile",
    )
    validation = _exact_keys(
        value["validation"],
        {"missing_required_artifact_classes"},
        "CMUX semantic result validation",
    )
    benchmark = _exact_keys(
        value["benchmark"],
        {"state_class", "semantic_comparison_key", "comparison_context_key"},
        "CMUX semantic result benchmark",
    )
    cleanup = _exact_keys(
        value["cleanup"],
        {"state", "process_group_settled"},
        "CMUX semantic result cleanup",
    )
    toolchain = _exact_keys(
        value["toolchain"],
        {"identity", "observations"},
        "CMUX semantic result toolchain",
    )
    resource = _exact_keys(
        value["resource_summary"],
        {"resource_class", "cpu_count", "memory_bytes", "architecture"},
        "CMUX semantic result resources",
    )
    if (
        source.get("repository") != CMUX_REPOSITORY
        or not isinstance(source.get("commit"), str)
        or OID_RE.fullmatch(source["commit"]) is None
        or not isinstance(source.get("tree"), str)
        or OID_RE.fullmatch(source["tree"]) is None
        or not isinstance(profile.get("id"), str)
        or PROFILE_RE.fullmatch(profile["id"]) is None
        or type(profile.get("generation")) is not int
        or profile["generation"] < 1
        or profile["generation"] > 2**31 - 1
    ):
        raise ContractRefusal("invalid_result", "CMUX semantic source/profile is invalid")
    if (
        value.get("result") not in SEMANTIC_RESULTS
        or not isinstance(value.get("semantic_validator"), str)
        or not value["semantic_validator"]
        or not isinstance(value.get("environment_class"), str)
        or TOKEN_RE.fullmatch(value["environment_class"]) is None
        or not isinstance(value.get("expected_result_class"), str)
        or not value["expected_result_class"]
        or not isinstance(value.get("network_class"), str)
        or not value["network_class"]
        or not isinstance(value.get("timeout_class"), str)
        or not value["timeout_class"]
        or type(value.get("exit_code")) is not int
        or type(value.get("started_at_unix_millis")) is not int
        or type(value.get("ended_at_unix_millis")) is not int
        or not 0 <= value["started_at_unix_millis"] <= value["ended_at_unix_millis"] < 253402300800000
    ):
        raise ContractRefusal("invalid_result", "CMUX semantic result state is invalid")
    parameters = value["parameters"]
    if (
        not isinstance(parameters, dict)
        or len(parameters) > 8
        or any(
            not isinstance(name, str)
            or PARAM_RE.fullmatch(name) is None
            or type(parameter) is not int
            for name, parameter in parameters.items()
        )
    ):
        raise ContractRefusal("invalid_result", "CMUX semantic parameters are invalid")

    runtime_inputs = value["runtime_input_identities"]
    if not isinstance(runtime_inputs, list):
        raise ContractRefusal("invalid_result", "CMUX runtime inputs are invalid")
    seen_inputs: set[str] = set()
    for item in runtime_inputs:
        entry = _exact_keys(
            item,
            {"name", "class", "identity", "sha256", "bytes"},
            "CMUX runtime input",
        )
        if (
            not isinstance(entry.get("name"), str)
            or not entry["name"]
            or entry["name"] in seen_inputs
            or not isinstance(entry.get("class"), str)
            or not entry["class"]
            or entry.get("identity") not in {"file-sha256", "parent-tree-sha256"}
            or not isinstance(entry.get("sha256"), str)
            or DIGEST_RE.fullmatch(entry["sha256"]) is None
            or type(entry.get("bytes")) is not int
            or entry["bytes"] < 0
        ):
            raise ContractRefusal("invalid_result", "CMUX runtime input is invalid")
        seen_inputs.add(entry["name"])

    artifacts = value["artifact_identities"]
    if not isinstance(artifacts, list):
        raise ContractRefusal("invalid_result", "CMUX artifact identities are invalid")
    for item in artifacts:
        entry = _exact_keys(
            item,
            {"class", "path_class", "sha256", "bytes"},
            "CMUX artifact identity",
        )
        if (
            not isinstance(entry.get("class"), str)
            or not entry["class"]
            or entry.get("path_class") != "repository_output"
            or not isinstance(entry.get("sha256"), str)
            or DIGEST_RE.fullmatch(entry["sha256"]) is None
            or type(entry.get("bytes")) is not int
            or entry["bytes"] < 0
        ):
            raise ContractRefusal("invalid_result", "CMUX artifact identity is invalid")

    missing = validation["missing_required_artifact_classes"]
    if (
        not isinstance(missing, list)
        or any(not isinstance(item, str) or not item for item in missing)
    ):
        raise ContractRefusal("invalid_result", "CMUX artifact validation is invalid")
    observations = toolchain["observations"]
    if (
        not isinstance(toolchain.get("identity"), str)
        or DIGEST_RE.fullmatch(toolchain["identity"]) is None
        or not isinstance(observations, dict)
        or any(
            not isinstance(name, str)
            or not name
            or not isinstance(observation, str)
            for name, observation in observations.items()
        )
        or toolchain["identity"] != sha256(canonical_bytes(observations))
    ):
        raise ContractRefusal("invalid_result", "CMUX toolchain identity is inconsistent")
    if (
        benchmark.get("state_class") not in STATE_CLASSES
        or not isinstance(benchmark.get("semantic_comparison_key"), str)
        or DIGEST_RE.fullmatch(benchmark["semantic_comparison_key"]) is None
        or not isinstance(benchmark.get("comparison_context_key"), str)
        or DIGEST_RE.fullmatch(benchmark["comparison_context_key"]) is None
    ):
        raise ContractRefusal("invalid_result", "CMUX benchmark identity is invalid")
    semantic_key = _cmux_semantic_key(value)
    if benchmark["semantic_comparison_key"] != semantic_key:
        raise ContractRefusal("invalid_result", "CMUX semantic comparison identity is inconsistent")
    if benchmark["comparison_context_key"] != _cmux_context_key(
        semantic_key,
        benchmark["state_class"],
        toolchain["identity"],
    ):
        raise ContractRefusal("invalid_result", "CMUX comparison context is inconsistent")
    if (
        cleanup.get("state") not in {"complete", "forced", "incomplete"}
        or type(cleanup.get("process_group_settled")) is not bool
    ):
        raise ContractRefusal("invalid_result", "CMUX cleanup evidence is invalid")
    if value["result"] == "ambiguous":
        if cleanup != {"state": "forced", "process_group_settled": False}:
            raise ContractRefusal("invalid_result", "ambiguous CMUX result lacks forced cleanup evidence")
    elif cleanup != {"state": "complete", "process_group_settled": True}:
        raise ContractRefusal("invalid_result", "terminal CMUX result lacks complete cleanup")
    if value["result"] == "passed" and (
        value["exit_code"] != 0 or missing
    ):
        raise ContractRefusal("invalid_result", "passed CMUX result is inconsistent")
    if value["result"] == "failed" and value["exit_code"] == 0 and not missing:
        raise ContractRefusal("invalid_result", "failed CMUX result is inconsistent")
    timings = value["stage_timings"]
    if (
        not isinstance(timings, list)
        or not timings
        or any(
            not isinstance(item, dict)
            or set(item) != {"stage", "seconds"}
            or not isinstance(item["stage"], str)
            or not item["stage"]
            or isinstance(item["seconds"], bool)
            or not isinstance(item["seconds"], (int, float))
            or item["seconds"] < 0
            for item in timings
        )
    ):
        raise ContractRefusal("invalid_result", "CMUX stage timings are invalid")
    if (
        not isinstance(resource.get("resource_class"), str)
        or not resource["resource_class"]
        or type(resource.get("cpu_count")) is not int
        or resource["cpu_count"] <= 0
        or (resource.get("memory_bytes") is not None and (
            type(resource["memory_bytes"]) is not int or resource["memory_bytes"] <= 0
        ))
        or not isinstance(resource.get("architecture"), str)
        or not resource["architecture"]
    ):
        raise ContractRefusal("invalid_result", "CMUX resource summary is invalid")
    return value


def decode_cmux_result(raw: bytes) -> dict[str, object]:
    value = decode_json(raw, MAX_DOCUMENT_BYTES * 4)
    if not isinstance(value, dict):
        raise ContractRefusal("invalid_result", "CMUX semantic result must be an object")
    canonical = canonical_bytes(value) + b"\n"
    if raw != canonical:
        raise ContractRefusal("invalid_result", "CMUX semantic result is not canonical")
    if (
        value.get("document_type") != "cmux-workload-result"
        or type(value.get("schema_version")) is not int
        or value.get("schema_version") != 1
    ):
        raise ContractRefusal(
            "invalid_result", "CMUX semantic result contract is unsupported"
        )
    return validate_cmux_result(value)


def observe(request: Request, raw_result: bytes) -> dict[str, object]:
    result = decode_cmux_result(raw_result)
    expected_source = {
        "repository": request.repository,
        "commit": request.commit,
        "tree": request.tree,
    }
    expected_profile = {
        "id": request.profile_id,
        "generation": request.profile_generation,
    }
    if result.get("source") != expected_source or result.get("profile") != expected_profile:
        raise ContractRefusal(
            "result_mismatch", "CMUX result source/profile differs from request"
        )
    if result.get("parameters") != request.parameters:
        raise ContractRefusal(
            "result_mismatch", "CMUX result parameters differ from request"
        )
    benchmark = result.get("benchmark")
    if (
        not isinstance(benchmark, dict)
        or benchmark.get("state_class") != request.benchmark_state_class
    ):
        raise ContractRefusal(
            "result_mismatch", "CMUX result benchmark state differs from request"
        )
    validator = result.get("semantic_validator")
    artifacts = result.get("artifact_identities")
    cleanup = result.get("cleanup")
    if (
        not isinstance(validator, str)
        or not validator
        or not isinstance(artifacts, list)
        or not isinstance(cleanup, dict)
    ):
        raise ContractRefusal("invalid_result", "CMUX semantic evidence is incomplete")
    outer: dict[str, object] = {
        "document_type": OBSERVATION_TYPE,
        "schema_version": SCHEMA_VERSION,
        "external_request_ref": request.external_request_ref,
        "request_sha256": sha256(canonical_bytes(request_document(request))),
        "execution_binding_sha256": execution_binding(request),
        "source": expected_source,
        "profile": expected_profile,
        "state": OUTER_STATES[result["result"]],
        "cmux_semantic_result_sha256": sha256(raw_result),
        "cmux_semantic_validator": validator,
        "authority": AUTHORITY,
    }
    if request.work_ref is not None:
        outer["correlation"] = {"work_ref": request.work_ref}
    return outer


def write_document(value: object) -> None:
    raw = canonical_bytes(value) + b"\n"
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise ContractRefusal("invalid_document", "output exceeds its fixed ceiling")
    sys.stdout.buffer.write(raw)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    commands.add_parser("plan")
    observe_parser = commands.add_parser("observe")
    observe_parser.add_argument("--request", required=True)
    observe_parser.add_argument("--result", required=True)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "plan":
            request = decode_request(sys.stdin.buffer.read(MAX_DOCUMENT_BYTES + 1))
            write_document(plan(request))
            return 0
        if args.command == "observe":
            request = decode_request(Path(args.request).read_bytes())
            result = Path(args.result).read_bytes()
            write_document(observe(request, result))
            return 0
        raise AssertionError(args.command)
    except (ContractRefusal, OSError) as error:
        code = error.code if isinstance(error, ContractRefusal) else "io_error"
        print(f"cmux workload request refused [{code}]: {error}", file=sys.stderr)
        return 64


if __name__ == "__main__":
    raise SystemExit(main())
