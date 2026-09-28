#!/usr/bin/env python3
"""CMUX-owned fleet enrollment, role acceptance, and bounded status projection."""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import sys
from pathlib import Path
from typing import Any

ENROLLMENT_SCHEMA = "glaeda-cmux-fleet-enrollment/v1"
ACCEPTANCE_SCHEMA = "glaeda-cmux-fleet-acceptance/v2"
CLASS_ACCEPTANCE_SCHEMA = "glaeda-cmux-fleet-class-acceptance/v1"
CANDIDATE_MANIFEST_SCHEMA = "glaeda-fleet-candidate/v1"
STATUS_SCHEMA = "glaeda-cmux-fleet-node-status/v1"
BOOTSTRAP_SCHEMA = "glaeda-cmux-fleet-bootstrap/v1"
MAX_DOCUMENT_BYTES = 64 * 1024
MAX_STATUS_BYTES = 16 * 1024
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9._-]{0,79}\Z")
NODE_RE = re.compile(r"cmux-[a-z0-9][a-z0-9-]{2,59}\Z")
FLEET_CLASS_RE = re.compile(r"[a-z][a-z0-9-]{0,31}\Z")
HARDWARE_TEXT_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9 ,.()_@-]{0,79}\Z")
REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}/[A-Za-z0-9_.-]{1,100}\Z")

ROLES = (
    "artifact_cache",
    "background_replay",
    "benchmark",
    "cmux_linux_agent",
    "cmux_linux_ci",
    "cmux_macos_native_build",
    "cmux_macos_test",
    "diagnostic",
)
ENROLLABLE_ROLES = {
    "cmux_linux_ci",
    "cmux_macos_native_build",
}
CMUX_REPOSITORY = "manaflow-ai/cmux"
CMUX_RESULT_DOCUMENT_TYPE = "cmux-workload-result"
CMUX_RESULT_SCHEMA_VERSION = 1
CMUX_RESULT_STATES = {"passed", "failed", "timed_out", "ambiguous"}
CMUX_PROFILE_RUNNER = "scripts/ci/cmux_workload_profile.py"
ATTEMPT_PREFIX = ".acceptance-run."
RETAINED_ATTEMPT_PREFIX = "rejected-attempt."
RETAINED_ATTEMPT_LIMIT = 3
# What tempfile.mkdtemp appends to ATTEMPT_PREFIX. Retention renames keep it,
# so this is also exactly the set of names Glaeda can have retained.
ATTEMPT_SUFFIX = re.compile(r"[a-z0-9_]{8}")
LOCAL_EXECUTION_CLASS = "glaeda-local-profile/v1"
EXTERNAL_EVIDENCE_CLASS = "external-evidence/v1"
# A node receipt minted from a class acceptance: no build ran on this node; it
# re-observed itself and matched the class receipt exactly (adopt_class_acceptance).
CLASS_EXECUTION_CLASS = "glaeda-class-acceptance/v1"
ACCEPTANCE_CHILD_ENV_KEYS = (
    "PATH",
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "DEVELOPER_DIR",
)
ROLE_PROFILES = {
    "cmux_linux_ci": {"id": "cmux.ci.guard", "generation": 1},
    "cmux_macos_native_build": {"id": "cmux.macos.dev-check", "generation": 1},
}
STATES = ("discovered", "enrolling", "eligible", "draining", "quarantined", "retired")
QUARANTINE_REASONS = (
    "dirty_canonical_checkout",
    "disk_pressure",
    "failed_acceptance",
    "hardware_failure",
    "service_mismatch",
    "stale_glaeda_generation",
    "toolchain_mismatch",
    "unexplained_process_settlement",
)
ROLE_OS = {
    "cmux_macos_native_build": "macos",
    "cmux_macos_test": "macos",
    "cmux_linux_ci": "linux",
    "cmux_linux_agent": "linux",
}
TRANSITIONS = {
    "discovered": {"enrolling", "retired"},
    "enrolling": {"eligible", "quarantined", "retired"},
    "eligible": {"draining", "quarantined", "retired"},
    "draining": {"eligible", "quarantined", "retired"},
    "quarantined": {"enrolling", "retired"},
    "retired": set(),
}
ENROLLMENT_KEYS = {
    "schema", "nodeId", "architecture", "os", "hardwareCapabilityClass",
    "supportedToolchainGenerations", "roleProfiles",
    "allowedExecutionRoles", "operatorFleetScope",
    "enrollmentGeneration", "glaedaGeneration", "state", "quarantineReason",
}
# Present only on a node that became eligible on a class acceptance: the
# receiptSha256 of the class receipt it relied on. Absent keeps older
# enrollments valid byte for byte.
ENROLLMENT_OPTIONAL_KEYS = {"classAcceptanceSha256"}

class FleetError(RuntimeError):
    """A closed fleet enrollment/status refusal."""


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(canonical(value)).hexdigest()


def cmux_canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode()


def cmux_digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(cmux_canonical_bytes(value)).hexdigest()


def _file_sha256(path: Path) -> str:
    digest_value = hashlib.sha256()
    try:
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest_value.update(chunk)
    except OSError as error:
        raise FleetError("Glaeda fleet contract file is unavailable") from error
    return "sha256:" + digest_value.hexdigest()


def fleet_contract_generation() -> str:
    root = Path(__file__).resolve().parent
    files = {
        "cmux_fleet.py": _file_sha256(root / "cmux_fleet.py"),
        "cmux_fleet_bootstrap.py": _file_sha256(root / "cmux_fleet_bootstrap.py"),
    }
    return digest(files)


def exact_keys(value: object, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise FleetError(f"{label} has unknown or missing fields")
    return value


def token(value: object, label: str) -> str:
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise FleetError(f"{label} is invalid")
    return value


def sha256(value: object, label: str, *, optional: bool = False) -> str | None:
    if value is None and optional:
        return None
    if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
        raise FleetError(f"{label} is invalid")
    return value


def positive_int(value: object, label: str) -> int:
    if type(value) is not int or not 1 <= value <= 2**31 - 1:
        raise FleetError(f"{label} is invalid")
    return value


def sorted_unique_strings(value: object, label: str, *, allowed: set[str] | None = None) -> list[str]:
    if not isinstance(value, list) or len(value) > 32 or any(not isinstance(v, str) for v in value):
        raise FleetError(f"{label} is invalid")
    if value != sorted(set(value)):
        raise FleetError(f"{label} must be sorted and unique")
    if allowed is not None and any(v not in allowed for v in value):
        raise FleetError(f"{label} contains an unsupported value")
    return value


def validate_enrollment(value: object) -> dict[str, Any]:
    if isinstance(value, dict) and set(value) == ENROLLMENT_KEYS | ENROLLMENT_OPTIONAL_KEYS:
        sha256(value["classAcceptanceSha256"], "enrollment class acceptance digest")
        doc = value
    else:
        doc = exact_keys(value, ENROLLMENT_KEYS, "enrollment")
    if doc["schema"] != ENROLLMENT_SCHEMA:
        raise FleetError("enrollment schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise FleetError("nodeId must be an opaque cmux-* identifier")
    if doc["architecture"] not in {"arm64", "x86_64"}:
        raise FleetError("architecture is unsupported")
    os_doc = exact_keys(doc["os"], {"family", "versionClass"}, "os")
    if os_doc["family"] not in {"macos", "linux"}:
        raise FleetError("OS family is unsupported")
    token(os_doc["versionClass"], "OS version class")
    token(doc["hardwareCapabilityClass"], "hardware capability class")
    token(doc["operatorFleetScope"], "operator/fleet scope")
    generations = sorted_unique_strings(doc["supportedToolchainGenerations"], "supported toolchain generations")
    if not generations or any(SHA256_RE.fullmatch(v) is None for v in generations):
        raise FleetError("supported toolchain generations are invalid")
    roles = sorted_unique_strings(doc["allowedExecutionRoles"], "allowed execution roles", allowed=set(ROLES))
    if not roles:
        raise FleetError("at least one execution role is required")
    role_profiles = doc["roleProfiles"]
    if not isinstance(role_profiles, dict) or set(role_profiles) != set(roles):
        raise FleetError("role profiles must exactly cover allowed roles")
    for role in roles:
        profile = exact_keys(role_profiles[role], {"id", "generation"}, "role profile")
        token(profile["id"], "role profile id")
        positive_int(profile["generation"], "role profile generation")
        if profile != ROLE_PROFILES.get(role):
            raise FleetError(f"role {role} profile is not the reviewed v1 profile")
    unreviewed = [role for role in roles if role not in ENROLLABLE_ROLES]
    if unreviewed:
        raise FleetError(
            "execution role lacks a reviewed v1 acceptance workload: "
            + ",".join(unreviewed)
        )
    for role in roles:
        required_os = ROLE_OS.get(role)
        if required_os is not None and required_os != os_doc["family"]:
            raise FleetError(f"role {role} is incompatible with OS family {os_doc['family']}")
    positive_int(doc["enrollmentGeneration"], "enrollment generation")
    sha256(doc["glaedaGeneration"], "Glaeda generation")
    if doc["state"] not in STATES:
        raise FleetError("node state is unsupported")
    reason = doc["quarantineReason"]
    if doc["state"] == "quarantined":
        if reason not in QUARANTINE_REASONS:
            raise FleetError("quarantine reason is required and must be reviewed")
    elif reason is not None:
        raise FleetError("quarantine reason is only valid for quarantined nodes")
    return doc


def enrollment_from_bootstrap(
    bootstrap_value: object,
    *,
    node_id: str,
    operator_fleet_scope: str,
    enrollment_generation: int,
) -> dict[str, Any]:
    if (
        not isinstance(bootstrap_value, dict)
        or bootstrap_value.get("schema") != BOOTSTRAP_SCHEMA
    ):
        raise FleetError("bootstrap receipt schema is unsupported")
    if bootstrap_value.get("authority") != "observation_only":
        raise FleetError("bootstrap receipt authority is invalid")
    if bootstrap_value.get("eligibleForEnrollment") is not True:
        raise FleetError("bootstrap receipt has blocking checks")
    enrollment = {
        "schema": ENROLLMENT_SCHEMA,
        "nodeId": node_id,
        "architecture": bootstrap_value.get("architecture"),
        "os": {
            "family": bootstrap_value.get("platform"),
            "versionClass": bootstrap_value.get("osVersionClass"),
        },
        "hardwareCapabilityClass": bootstrap_value.get("hardwareCapabilityClass"),
        "supportedToolchainGenerations": [
            bootstrap_value.get("toolchainGeneration")
        ],
        "roleProfiles": bootstrap_value.get("roleProfiles"),
        "allowedExecutionRoles": bootstrap_value.get("roles"),
        "operatorFleetScope": operator_fleet_scope,
        "enrollmentGeneration": enrollment_generation,
        "glaedaGeneration": bootstrap_value.get("glaedaGeneration"),
        "state": "enrolling",
        "quarantineReason": None,
    }
    return validate_enrollment(enrollment)


def renewal_plan(enrollment_value: object, bootstrap_value: object) -> dict[str, Any]:
    """Plan a new acceptance generation without granting execution authority."""
    current = validate_enrollment(enrollment_value)
    if current["state"] != "quarantined":
        raise FleetError("enrollment renewal requires a quarantined node")
    if current["enrollmentGeneration"] == 2**31 - 1:
        raise FleetError("enrollment generation is exhausted")
    replacement = enrollment_from_bootstrap(
        bootstrap_value,
        node_id=current["nodeId"],
        operator_fleet_scope=current["operatorFleetScope"],
        enrollment_generation=current["enrollmentGeneration"] + 1,
    )
    plan = {
        "schema": "glaeda-cmux-enrollment-renewal/v1",
        "currentEnrollmentSha256": digest(current),
        "bootstrapSha256": digest(bootstrap_value),
        "replacement": replacement,
    }
    return {**plan, "planSha256": digest(plan)}


def _cmux_semantic_key(value: dict[str, Any]) -> str:
    source = value["source"]
    profile = value["profile"]
    runtime_inputs = value["runtime_input_identities"]
    return cmux_digest(
        {
            "source": {
                "repository": source["repository"],
                "tree": source["tree"],
            },
            "profile": {
                "id": profile["id"],
                "generation": profile["generation"],
            },
            "semantic_validator": value["semantic_validator"],
            "environment_class": value["environment_class"],
            "parameters": value["parameters"],
            "runtime_inputs": [
                {
                    "name": item["name"],
                    "class": item["class"],
                    "identity": item["identity"],
                    "sha256": item["sha256"],
                }
                for item in runtime_inputs
            ],
        }
    )


def _cmux_context_key(
    semantic_key: str,
    state_class: str,
    toolchain_identity: str,
) -> str:
    return cmux_digest(
        {
            "semantic_key": semantic_key,
            "state_class": state_class,
            "toolchain_identity": toolchain_identity,
        }
    )


def validate_cmux_semantic_result(
    value: object,
    expected_profile: dict[str, object],
) -> dict[str, Any]:
    doc = exact_keys(
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
    if (
        doc["document_type"] != CMUX_RESULT_DOCUMENT_TYPE
        or type(doc["schema_version"]) is not int
        or doc["schema_version"] != CMUX_RESULT_SCHEMA_VERSION
        or doc["result"] not in CMUX_RESULT_STATES
    ):
        raise FleetError("CMUX semantic result contract is unsupported")

    source = exact_keys(
        doc["source"],
        {"repository", "commit", "tree"},
        "CMUX semantic source",
    )
    if (
        source["repository"] != CMUX_REPOSITORY
        or not isinstance(source["commit"], str)
        or COMMIT_RE.fullmatch(source["commit"]) is None
        or not isinstance(source["tree"], str)
        or COMMIT_RE.fullmatch(source["tree"]) is None
    ):
        raise FleetError("CMUX semantic source commit/tree is invalid")

    profile = exact_keys(
        doc["profile"],
        {"id", "generation"},
        "CMUX semantic profile",
    )
    token(profile["id"], "CMUX semantic profile id")
    positive_int(profile["generation"], "CMUX semantic profile generation")
    if profile != expected_profile:
        raise FleetError("CMUX semantic profile differs from enrolled role profile")
    if doc["parameters"] != {}:
        raise FleetError("CMUX fleet acceptance profile parameters must be empty")
    if doc["runtime_input_identities"] != []:
        raise FleetError("CMUX fleet acceptance profile runtime inputs must be empty")

    if (
        not isinstance(doc["semantic_validator"], str)
        or not doc["semantic_validator"]
        or not isinstance(doc["environment_class"], str)
        or TOKEN_RE.fullmatch(doc["environment_class"]) is None
        or not isinstance(doc["expected_result_class"], str)
        or not doc["expected_result_class"]
        or not isinstance(doc["network_class"], str)
        or not doc["network_class"]
        or not isinstance(doc["timeout_class"], str)
        or not doc["timeout_class"]
        or type(doc["exit_code"]) is not int
        or type(doc["started_at_unix_millis"]) is not int
        or type(doc["ended_at_unix_millis"]) is not int
        or not 0
        <= doc["started_at_unix_millis"]
        <= doc["ended_at_unix_millis"]
        < 253402300800000
    ):
        raise FleetError("CMUX semantic result state is invalid")

    validation = exact_keys(
        doc["validation"],
        {"missing_required_artifact_classes"},
        "CMUX semantic validation",
    )
    missing = validation["missing_required_artifact_classes"]
    if (
        not isinstance(missing, list)
        or any(not isinstance(item, str) or not item for item in missing)
    ):
        raise FleetError("CMUX semantic artifact validation is invalid")

    artifacts = doc["artifact_identities"]
    if not isinstance(artifacts, list):
        raise FleetError("CMUX semantic artifact identities are invalid")
    for item in artifacts:
        artifact = exact_keys(
            item,
            {"class", "path_class", "sha256", "bytes"},
            "CMUX semantic artifact identity",
        )
        if (
            not isinstance(artifact["class"], str)
            or not artifact["class"]
            or artifact["path_class"] != "repository_output"
            or not isinstance(artifact["sha256"], str)
            or SHA256_RE.fullmatch(artifact["sha256"]) is None
            or type(artifact["bytes"]) is not int
            or artifact["bytes"] < 0
        ):
            raise FleetError("CMUX semantic artifact identity is invalid")

    benchmark = exact_keys(
        doc["benchmark"],
        {"state_class", "semantic_comparison_key", "comparison_context_key"},
        "CMUX semantic benchmark",
    )
    if (
        benchmark["state_class"] != "cold"
        or not isinstance(benchmark["semantic_comparison_key"], str)
        or SHA256_RE.fullmatch(benchmark["semantic_comparison_key"]) is None
        or not isinstance(benchmark["comparison_context_key"], str)
        or SHA256_RE.fullmatch(benchmark["comparison_context_key"]) is None
    ):
        raise FleetError("CMUX fleet acceptance benchmark identity is invalid")

    toolchain = exact_keys(
        doc["toolchain"],
        {"identity", "observations"},
        "CMUX semantic toolchain",
    )
    observations = toolchain["observations"]
    if (
        not isinstance(toolchain["identity"], str)
        or SHA256_RE.fullmatch(toolchain["identity"]) is None
        or not isinstance(observations, dict)
        or any(
            not isinstance(name, str)
            or not name
            or not isinstance(observation, str)
            for name, observation in observations.items()
        )
        or toolchain["identity"] != cmux_digest(observations)
    ):
        raise FleetError("CMUX semantic toolchain identity is inconsistent")

    semantic_key = _cmux_semantic_key(doc)
    if benchmark["semantic_comparison_key"] != semantic_key:
        raise FleetError("CMUX semantic comparison identity is inconsistent")
    if benchmark["comparison_context_key"] != _cmux_context_key(
        semantic_key,
        benchmark["state_class"],
        toolchain["identity"],
    ):
        raise FleetError("CMUX comparison context identity is inconsistent")

    cleanup = exact_keys(
        doc["cleanup"],
        {"state", "process_group_settled"},
        "CMUX semantic cleanup",
    )
    if (
        cleanup["state"] not in {"complete", "forced", "incomplete"}
        or type(cleanup["process_group_settled"]) is not bool
    ):
        raise FleetError("CMUX semantic cleanup evidence is invalid")
    if doc["result"] == "ambiguous":
        if cleanup != {"state": "forced", "process_group_settled": False}:
            raise FleetError("ambiguous CMUX result lacks forced cleanup evidence")
    elif cleanup != {"state": "complete", "process_group_settled": True}:
        raise FleetError("terminal CMUX result lacks complete cleanup")

    if doc["result"] == "passed" and (doc["exit_code"] != 0 or missing):
        raise FleetError("passed CMUX result is inconsistent")
    if doc["result"] == "failed" and doc["exit_code"] == 0 and not missing:
        raise FleetError("failed CMUX result is inconsistent")
    timings = doc["stage_timings"]
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
        raise FleetError("CMUX semantic stage timings are invalid")

    resource = exact_keys(
        doc["resource_summary"],
        {"resource_class", "cpu_count", "memory_bytes", "architecture"},
        "CMUX semantic resources",
    )
    if (
        not isinstance(resource["resource_class"], str)
        or not resource["resource_class"]
        or type(resource["cpu_count"]) is not int
        or resource["cpu_count"] <= 0
        or (
            resource["memory_bytes"] is not None
            and (
                type(resource["memory_bytes"]) is not int
                or resource["memory_bytes"] <= 0
            )
        )
        or not isinstance(resource["architecture"], str)
        or not resource["architecture"]
    ):
        raise FleetError("CMUX semantic resource summary is invalid")
    return doc


ACCEPTANCE_RECEIPT_KEYS = {
    "schema", "nodeId", "enrollmentGeneration", "role", "source", "profile",
    "toolchainGeneration", "glaedaGeneration", "glaedaFleetContractGeneration",
    "cmuxSemanticResultSha256",
    "cmuxSemanticResultState", "cmuxEnvironmentClass", "cmuxToolchainIdentity",
    "postBootstrapSha256", "executionClass", "localExecutionAttemptSha256",
    "processSettlement", "result",
}


def validate_acceptance_receipt(value: object) -> dict[str, Any]:
    class_derived = (
        isinstance(value, dict)
        and value.get("executionClass") == CLASS_EXECUTION_CLASS
    )
    doc = exact_keys(
        value,
        ACCEPTANCE_RECEIPT_KEYS | ({"classAcceptanceSha256"} if class_derived else set()),
        "acceptance receipt",
    )
    if doc["schema"] != ACCEPTANCE_SCHEMA:
        raise FleetError("acceptance receipt schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise FleetError("acceptance receipt nodeId is invalid")
    positive_int(doc["enrollmentGeneration"], "acceptance receipt enrollment generation")
    role = doc["role"]
    if role not in ENROLLABLE_ROLES:
        raise FleetError("acceptance receipt role lacks a reviewed v1 profile")
    source = exact_keys(
        doc["source"],
        {"repository", "commit", "tree"},
        "acceptance receipt source",
    )
    if source["repository"] != CMUX_REPOSITORY:
        raise FleetError("acceptance receipt source repository is invalid")
    if (
        not isinstance(source["commit"], str)
        or COMMIT_RE.fullmatch(source["commit"]) is None
        or not isinstance(source["tree"], str)
        or COMMIT_RE.fullmatch(source["tree"]) is None
    ):
        raise FleetError("acceptance receipt source commit/tree is invalid")
    profile = exact_keys(doc["profile"], {"id", "generation"}, "acceptance receipt profile")
    token(profile["id"], "acceptance receipt profile id")
    positive_int(profile["generation"], "acceptance receipt profile generation")
    if profile != ROLE_PROFILES.get(role):
        raise FleetError("acceptance receipt profile is not the reviewed role profile")
    sha256(doc["toolchainGeneration"], "acceptance receipt toolchain generation")
    sha256(doc["glaedaGeneration"], "acceptance receipt Glaeda generation")
    sha256(
        doc["glaedaFleetContractGeneration"],
        "acceptance receipt Glaeda fleet contract generation",
    )
    sha256(doc["cmuxSemanticResultSha256"], "CMUX semantic result digest")
    sha256(doc["cmuxToolchainIdentity"], "CMUX semantic toolchain identity")
    sha256(doc["postBootstrapSha256"], "post-acceptance bootstrap digest")
    token(doc["cmuxEnvironmentClass"], "CMUX environment class")
    if doc["cmuxSemanticResultState"] not in CMUX_RESULT_STATES:
        raise FleetError("CMUX semantic result state is invalid")
    if doc["executionClass"] not in {
        LOCAL_EXECUTION_CLASS,
        EXTERNAL_EVIDENCE_CLASS,
        CLASS_EXECUTION_CLASS,
    }:
        raise FleetError("acceptance execution class is invalid")
    if class_derived:
        sha256(doc["classAcceptanceSha256"], "class acceptance digest")
        if doc["localExecutionAttemptSha256"] is not None:
            raise FleetError("a class-derived receipt has no local execution attempt")
    local_attempt = sha256(
        doc["localExecutionAttemptSha256"],
        "local execution attempt",
        optional=True,
    )
    if (
        (doc["executionClass"] == LOCAL_EXECUTION_CLASS)
        != (local_attempt is not None)
    ):
        raise FleetError("acceptance local execution evidence is inconsistent")
    if doc["processSettlement"] not in {"complete", "incomplete"}:
        raise FleetError("acceptance process settlement is invalid")
    if doc["result"] not in {"accepted", "rejected"}:
        raise FleetError("acceptance receipt result is invalid")
    accepted = (
        doc["cmuxSemanticResultState"] == "passed"
        and doc["processSettlement"] == "complete"
        and (
            (doc["executionClass"] == LOCAL_EXECUTION_CLASS and local_attempt is not None)
            or class_derived
        )
    )
    if (doc["result"] == "accepted") != accepted:
        raise FleetError("acceptance receipt result disagrees with semantic result")
    return doc


def acceptance_matches_enrollment(enrollment: dict[str, Any], receipt: dict[str, Any], role: str) -> tuple[bool, str]:
    if receipt.get("schema") != ACCEPTANCE_SCHEMA or receipt.get("result") != "accepted":
        return False, "acceptance_missing_or_rejected"
    if receipt.get("nodeId") != enrollment["nodeId"] or receipt.get("role") != role:
        return False, "acceptance_identity_mismatch"
    if receipt.get("enrollmentGeneration") != enrollment["enrollmentGeneration"]:
        return False, "acceptance_enrollment_stale"
    if receipt.get("glaedaGeneration") != enrollment["glaedaGeneration"]:
        return False, "acceptance_glaeda_stale"
    if receipt.get("glaedaFleetContractGeneration") != fleet_contract_generation():
        return False, "acceptance_glaeda_contract_stale"
    if receipt.get("toolchainGeneration") not in enrollment["supportedToolchainGenerations"]:
        return False, "acceptance_toolchain_stale"
    if receipt.get("profile") != enrollment["roleProfiles"].get(role):
        return False, "acceptance_profile_stale"
    if (
        receipt.get("executionClass") == CLASS_EXECUTION_CLASS
        and receipt.get("classAcceptanceSha256") != enrollment.get("classAcceptanceSha256")
    ):
        # The enrollment names the class receipt its eligibility rests on; a
        # class-derived receipt counts only while it is that one.
        return False, "acceptance_class_stale"
    return True, "accepted"


def validate_post_acceptance_bootstrap(
    enrollment: dict[str, Any],
    bootstrap_value: object,
    toolchain_generation: str,
) -> str:
    observed = enrollment_from_bootstrap(
        bootstrap_value,
        node_id=enrollment["nodeId"],
        operator_fleet_scope=enrollment["operatorFleetScope"],
        enrollment_generation=enrollment["enrollmentGeneration"],
    )
    stable_fields = ENROLLMENT_KEYS - {
        "state",
        "quarantineReason",
        "supportedToolchainGenerations",
    }
    expected_stable = {name: enrollment[name] for name in stable_fields}
    observed_stable = {name: observed[name] for name in stable_fields}
    if observed_stable != expected_stable:
        raise FleetError(
            "post-acceptance bootstrap differs from enrolled machine capability"
        )
    if observed["supportedToolchainGenerations"] != [toolchain_generation]:
        raise FleetError(
            "post-acceptance bootstrap toolchain differs from acceptance"
        )
    return digest(bootstrap_value)


def finalize_acceptance(
    enrollment_value: object,
    role: str,
    toolchain_generation: str,
    cmux_result_value: object,
    post_bootstrap_value: object,
    cmux_result_sha256: str | None = None,
    *,
    execution_class: str = EXTERNAL_EVIDENCE_CLASS,
    local_execution_attempt_sha256: str | None = None,
) -> dict[str, Any]:
    enrollment = validate_enrollment(enrollment_value)
    if role not in enrollment["allowedExecutionRoles"]:
        raise FleetError("acceptance role is outside the enrollment allowlist")
    if toolchain_generation not in enrollment["supportedToolchainGenerations"]:
        raise FleetError("acceptance toolchain generation is outside the enrollment allowlist")
    post_bootstrap_sha256 = validate_post_acceptance_bootstrap(
        enrollment,
        post_bootstrap_value,
        toolchain_generation,
    )
    expected_profile = enrollment["roleProfiles"][role]
    semantic = validate_cmux_semantic_result(cmux_result_value, expected_profile)
    actual_cmux_result_sha256 = digest(semantic)
    if cmux_result_sha256 is None:
        cmux_result_sha256 = actual_cmux_result_sha256
    sha256(cmux_result_sha256, "CMUX semantic result digest")
    if cmux_result_sha256 != actual_cmux_result_sha256:
        raise FleetError("CMUX semantic result digest does not match validated result")
    cleanup = semantic["cleanup"]
    settlement = (
        "complete"
        if cleanup["state"] == "complete" and cleanup["process_group_settled"] is True
        else "incomplete"
    )
    locally_bound = (
        execution_class == LOCAL_EXECUTION_CLASS
        and local_execution_attempt_sha256 is not None
    )
    if local_execution_attempt_sha256 is not None:
        sha256(local_execution_attempt_sha256, "local execution attempt")
    result = (
        "accepted"
        if semantic["result"] == "passed"
        and settlement == "complete"
        and locally_bound
        else "rejected"
    )
    receipt = {
        "schema": ACCEPTANCE_SCHEMA,
        "nodeId": enrollment["nodeId"],
        "enrollmentGeneration": enrollment["enrollmentGeneration"],
        "role": role,
        "source": semantic["source"],
        "profile": semantic["profile"],
        "toolchainGeneration": toolchain_generation,
        "glaedaGeneration": enrollment["glaedaGeneration"],
        "glaedaFleetContractGeneration": fleet_contract_generation(),
        "cmuxSemanticResultSha256": cmux_result_sha256,
        "cmuxSemanticResultState": semantic["result"],
        "cmuxEnvironmentClass": semantic["environment_class"],
        "cmuxToolchainIdentity": semantic["toolchain"]["identity"],
        "postBootstrapSha256": post_bootstrap_sha256,
        "executionClass": execution_class,
        "localExecutionAttemptSha256": local_execution_attempt_sha256,
        "processSettlement": settlement,
        "result": result,
    }
    validate_acceptance_receipt(receipt)
    if len(canonical(receipt)) > MAX_STATUS_BYTES:
        raise FleetError("acceptance receipt exceeds size ceiling")
    return receipt


def node_status(enrollment_value: object, acceptance_values: list[object]) -> dict[str, Any]:
    enrollment = validate_enrollment(enrollment_value)
    receipts: dict[str, dict[str, Any]] = {}
    for value in acceptance_values:
        receipt = validate_acceptance_receipt(value)
        role = receipt["role"]
        if role in receipts:
            raise FleetError("duplicate acceptance receipt for role")
        receipts[role] = receipt
    role_status = []
    for role in enrollment["allowedExecutionRoles"]:
        if enrollment["state"] != "eligible":
            eligible, reason = False, f"node_{enrollment['state']}"
        else:
            receipt = receipts.get(role)
            if receipt is None:
                eligible, reason = False, "acceptance_missing_or_rejected"
            else:
                eligible, reason = acceptance_matches_enrollment(enrollment, receipt, role)
        role_status.append({"role": role, "eligible": eligible, "reason": reason})
    status = {
        "schema": STATUS_SCHEMA,
        "nodeId": enrollment["nodeId"],
        "enrollmentGeneration": enrollment["enrollmentGeneration"],
        "state": enrollment["state"],
        "routingCandidateEligible": any(r["eligible"] for r in role_status),
        "automaticDispatchAuthorized": False,
        "capability": {
            "architecture": enrollment["architecture"],
            "osFamily": enrollment["os"]["family"],
            "osVersionClass": enrollment["os"]["versionClass"],
            "hardwareCapabilityClass": enrollment["hardwareCapabilityClass"],
            "supportedToolchainGenerations": enrollment[
                "supportedToolchainGenerations"
            ],
            "roleProfiles": enrollment["roleProfiles"],
            "glaedaGeneration": enrollment["glaedaGeneration"],
            "operatorFleetScope": enrollment["operatorFleetScope"],
        },
        "roles": role_status,
        "privacy": {
            "containsSerialNumber": False,
            "containsPrivateNetworking": False,
            "containsUsername": False,
        },
    }
    if len(canonical(status)) > MAX_STATUS_BYTES:
        raise FleetError("status document exceeds size ceiling")
    return status


def transition(
    enrollment_value: object,
    target: str,
    reason: str | None,
    acceptance_values: list[object] | None = None,
) -> dict[str, Any]:
    enrollment = dict(validate_enrollment(enrollment_value))
    current = enrollment["state"]
    if target not in STATES or target not in TRANSITIONS[current]:
        raise FleetError(f"transition {current}->{target} is unsupported")
    if target == "quarantined":
        if reason not in QUARANTINE_REASONS:
            raise FleetError("a reviewed quarantine reason is required")
        enrollment["quarantineReason"] = reason
    else:
        if reason is not None:
            raise FleetError("transition reason is only accepted for quarantine")
        enrollment["quarantineReason"] = None
    if current == "quarantined" and target == "enrolling":
        if enrollment["enrollmentGeneration"] == 2**31 - 1:
            raise FleetError("enrollment generation is exhausted")
        enrollment["enrollmentGeneration"] += 1
        # A new generation relies on no earlier acceptance, class or local.
        enrollment.pop("classAcceptanceSha256", None)
    enrollment["state"] = target
    if target == "eligible":
        # Record which class receipt, if any, the roles rest on, so the
        # enrollment says whether this node was built on or only matched.
        receipts = [validate_acceptance_receipt(v) for v in acceptance_values or []]
        offered = sorted({
            r["classAcceptanceSha256"]
            for r in receipts
            if r["executionClass"] == CLASS_EXECUTION_CLASS
        })
        if len(offered) > 1:
            raise FleetError("receipts rest on different class acceptances")
        enrollment.pop("classAcceptanceSha256", None)
        if offered:
            enrollment["classAcceptanceSha256"] = offered[0]
    enrollment = validate_enrollment(enrollment)
    if target == "eligible":
        status = node_status(enrollment, acceptance_values or [])
        if not status["routingCandidateEligible"]:
            raise FleetError(
                "eligible transition requires a current accepted role receipt"
            )
        eligible_roles = {r["role"] for r in status["roles"] if r["eligible"]}
        if "classAcceptanceSha256" in enrollment and not any(
            r["executionClass"] == CLASS_EXECUTION_CLASS and r["role"] in eligible_roles
            for r in receipts
        ):
            # The class receipt offered did not count; nothing rests on it.
            enrollment.pop("classAcceptanceSha256")
    return enrollment


def _read_private_document(descriptor: int) -> object:
    before = os.fstat(descriptor)
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_uid != os.geteuid()
        or before.st_nlink != 1
        or stat.S_IMODE(before.st_mode) != 0o600
        or before.st_size < 0
        or before.st_size > MAX_DOCUMENT_BYTES
    ):
        raise FleetError("fleet document ownership or mode is unsafe")
    chunks: list[bytes] = []
    total = 0
    while True:
        chunk = os.read(descriptor, min(8192, MAX_DOCUMENT_BYTES + 1 - total))
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
        if total > MAX_DOCUMENT_BYTES:
            raise FleetError("fleet document exceeds size ceiling")
    after = os.fstat(descriptor)
    if (
        before.st_dev != after.st_dev
        or before.st_ino != after.st_ino
        or before.st_uid != after.st_uid
        or before.st_gid != after.st_gid
        or before.st_mode != after.st_mode
        or before.st_nlink != after.st_nlink
        or before.st_size != after.st_size
        or before.st_mtime_ns != after.st_mtime_ns
        or before.st_ctime_ns != after.st_ctime_ns
        or total != after.st_size
    ):
        raise FleetError("fleet document changed while reading")
    try:
        return json.loads(b"".join(chunks))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("fleet document is invalid JSON") from error


def load(path: Path) -> object:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        raise FleetError("fleet document is unavailable") from error
    try:
        return _read_private_document(descriptor)
    finally:
        os.close(descriptor)


def load_cmux_semantic_result(path: Path) -> tuple[dict[str, Any], str]:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        raise FleetError("CMUX semantic result is unavailable") from error
    try:
        info = os.fstat(descriptor)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.geteuid()
            or info.st_nlink != 1
            or stat.S_IMODE(info.st_mode) != 0o600
            or info.st_size <= 0
            or info.st_size > MAX_DOCUMENT_BYTES
        ):
            raise FleetError("CMUX semantic result file is unsafe")
        raw = b""
        while len(raw) <= MAX_DOCUMENT_BYTES:
            chunk = os.read(descriptor, min(8192, MAX_DOCUMENT_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
        if len(raw) > MAX_DOCUMENT_BYTES:
            raise FleetError("CMUX semantic result exceeds size ceiling")
        after = os.fstat(descriptor)
        if (
            info.st_dev != after.st_dev
            or info.st_ino != after.st_ino
            or info.st_uid != after.st_uid
            or info.st_gid != after.st_gid
            or info.st_mode != after.st_mode
            or info.st_nlink != after.st_nlink
            or info.st_size != after.st_size
            or info.st_mtime_ns != after.st_mtime_ns
            or info.st_ctime_ns != after.st_ctime_ns
            or len(raw) != after.st_size
        ):
            raise FleetError("CMUX semantic result changed while reading")
    finally:
        os.close(descriptor)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("CMUX semantic result is invalid JSON") from error
    canonical_raw = canonical(value)
    if raw != canonical_raw:
        raise FleetError("CMUX semantic result is not canonical")
    if not isinstance(value, dict):
        raise FleetError("CMUX semantic result is not an object")
    return value, "sha256:" + hashlib.sha256(raw).hexdigest()


def load_at(parent_fd: int, name: str) -> object:
    if "/" in name or name in {"", ".", ".."}:
        raise FleetError("fleet document name is invalid")
    try:
        descriptor = os.open(
            name,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
            dir_fd=parent_fd,
        )
    except OSError as error:
        raise FleetError("fleet document is unavailable") from error
    try:
        return _read_private_document(descriptor)
    finally:
        os.close(descriptor)


class FleetMutationLock:
    def __init__(self, enrollment_path: Path):
        if not enrollment_path.is_absolute() or enrollment_path.name != "enrollment.json":
            raise FleetError("mutable enrollment path must be explicit and canonical")
        self.enrollment_path = enrollment_path
        self.parent_path = enrollment_path.parent
        try:
            resolved_parent = self.parent_path.resolve(strict=True)
        except OSError as error:
            raise FleetError("fleet state directory is unavailable") from error
        if resolved_parent != self.parent_path:
            raise FleetError("fleet state directory is not canonical")
        try:
            self.parent_fd = os.open(
                self.parent_path,
                os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW,
            )
        except OSError as error:
            raise FleetError("fleet state directory is unavailable") from error
        try:
            info = os.fstat(self.parent_fd)
            if (
                not stat.S_ISDIR(info.st_mode)
                or info.st_uid != os.geteuid()
                or stat.S_IMODE(info.st_mode) & 0o077
            ):
                raise FleetError("fleet state directory ownership or mode is unsafe")
            self.parent_identity = (info.st_dev, info.st_ino)
            self.lock_fd = os.open(
                ".mutation.lock",
                os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW,
                0o600,
                dir_fd=self.parent_fd,
            )
            lock_info = os.fstat(self.lock_fd)
            if (
                not stat.S_ISREG(lock_info.st_mode)
                or lock_info.st_uid != os.geteuid()
                or lock_info.st_nlink != 1
                or stat.S_IMODE(lock_info.st_mode) != 0o600
                or lock_info.st_size != 0
            ):
                raise FleetError("fleet mutation lock is unsafe")
            try:
                fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise FleetError("fleet state mutation is busy") from error
            self.revalidate_parent_path()
        except Exception:
            if hasattr(self, "lock_fd"):
                os.close(self.lock_fd)
            os.close(self.parent_fd)
            raise

    def revalidate_parent_path(self) -> None:
        try:
            current = os.stat(self.parent_path, follow_symlinks=False)
        except OSError as error:
            raise FleetError("fleet state directory identity changed") from error
        held = os.fstat(self.parent_fd)
        if (
            not stat.S_ISDIR(current.st_mode)
            or current.st_uid != os.geteuid()
            or stat.S_IMODE(current.st_mode) & 0o077
            or (current.st_dev, current.st_ino) != self.parent_identity
            or (held.st_dev, held.st_ino) != self.parent_identity
        ):
            raise FleetError("fleet state directory identity changed")
        try:
            named_lock = os.stat(
                ".mutation.lock", dir_fd=self.parent_fd, follow_symlinks=False
            )
            held_lock = os.fstat(self.lock_fd)
        except OSError as error:
            raise FleetError("fleet mutation lock identity changed") from error
        if (
            (named_lock.st_dev, named_lock.st_ino)
            != (held_lock.st_dev, held_lock.st_ino)
            or any(
                not stat.S_ISREG(info.st_mode)
                or info.st_uid != os.geteuid()
                or info.st_nlink != 1
                or stat.S_IMODE(info.st_mode) != 0o600
                or info.st_size != 0
                for info in (named_lock, held_lock)
            )
        ):
            raise FleetError("fleet mutation lock identity changed")

    def load_enrollment(self) -> object:
        self.revalidate_parent_path()
        return load_at(self.parent_fd, self.enrollment_path.name)

    def __enter__(self) -> "FleetMutationLock":
        return self

    def __exit__(self, exc_type, exc, traceback) -> None:
        del exc_type, exc, traceback
        fcntl.flock(self.lock_fd, fcntl.LOCK_UN)
        os.close(self.lock_fd)
        os.close(self.parent_fd)


def durable_replace_enrollment(
    expected: object,
    replacement: object,
    mutation: FleetMutationLock,
) -> None:
    raw = canonical(replacement)
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise FleetError("fleet document exceeds size ceiling")
    if canonical(mutation.load_enrollment()) != canonical(expected):
        raise FleetError("fleet enrollment changed before mutation")

    temporary = None
    descriptor = None
    try:
        for attempt in range(32):
            candidate = f".enrollment.next.{os.getpid()}.{attempt}"
            try:
                descriptor = os.open(
                    candidate,
                    os.O_WRONLY
                    | os.O_CREAT
                    | os.O_EXCL
                    | os.O_CLOEXEC
                    | os.O_NOFOLLOW,
                    0o600,
                    dir_fd=mutation.parent_fd,
                )
                temporary = candidate
                break
            except FileExistsError:
                continue
        if descriptor is None or temporary is None:
            raise FleetError("fleet enrollment staging namespace is exhausted")
        with os.fdopen(descriptor, "wb", closefd=True) as stream:
            descriptor = None
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())

        if canonical(mutation.load_enrollment()) != canonical(expected):
            raise FleetError("fleet enrollment changed before publication")
        mutation.revalidate_parent_path()
        os.replace(
            temporary,
            mutation.enrollment_path.name,
            src_dir_fd=mutation.parent_fd,
            dst_dir_fd=mutation.parent_fd,
        )
        temporary = None
        os.fsync(mutation.parent_fd)
        mutation.revalidate_parent_path()
    finally:
        if descriptor is not None:
            os.close(descriptor)
        if temporary is not None:
            try:
                os.unlink(temporary, dir_fd=mutation.parent_fd)
            except FileNotFoundError:
                pass

    if canonical(mutation.load_enrollment()) != raw:
        raise FleetError("published fleet enrollment did not revalidate")


def acceptance_child_environment(temporary_root: Path) -> dict[str, str]:
    temporary_root.mkdir(parents=True, exist_ok=True)
    temporary_root.chmod(0o700)
    environment = {
        "LC_ALL": "C",
        "LANG": "C",
        "TMPDIR": str(temporary_root),
        "PATH": os.environ.get(
            "PATH",
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        ),
    }
    for name in ACCEPTANCE_CHILD_ENV_KEYS:
        if name == "PATH":
            continue
        value = os.environ.get(name)
        if value:
            environment[name] = value
    return environment


def _notice(message: str) -> None:
    """Tell the operator something stdout cannot carry: stdout is the receipt.

    This runs from a `finally`, so it may not raise and it may not write to
    stdout. Both are reachable: a closed fd 2 leaves `sys.stderr` as None and
    `print(file=None)` then writes to stdout, putting a private path in front
    of the JSON and making the receipt unparseable; piping stderr into a
    short-lived reader raises BrokenPipeError. A lost notice is worse only
    than a lost receipt.
    """
    stream = sys.stderr
    if stream is None:
        return
    try:
        print(message, file=stream)
    except (OSError, ValueError):
        pass


def profile_runner_interpreter_ready() -> bool:
    """Report whether this interpreter can run CMUX's profile runner.

    The runner waits on its child with `os.waitid` to hold the child's PID and
    process group unreleased, and CPython exposes that call on macOS only from
    3.13. Without it the child dies of an AttributeError a fraction of a second
    into acceptance, so refuse before the attempt rather than after.
    """
    return hasattr(os, "waitid")


def _remove_tree(path: Path) -> bool:
    """Remove `path` and report whether it is actually gone.

    A build leaves directories behind that the owner cannot descend into —
    Xcode's DerivedData and Cargo's source cache both do — and `rmtree` cannot
    remove those. `ignore_errors` would swallow the failure and leave the tree,
    so restore the owner's bits on the way down and then answer plainly. A
    caller that has promised the operator a bound needs to know when it missed.

    Links are unlinked, never followed. `os.walk` does not traverse a symlink
    but `os.chmod` does, so chmod'ing one would reach out of the tree and
    relax the permissions of a directory this function does not own — a build
    that links its TMPDIR at a toolchain or a Cargo registry is enough.
    """
    # os.path, not Path: Path.is_symlink re-raises EACCES before 3.14, and this
    # runs from a finally.
    if os.path.islink(path):
        try:
            path.unlink()
        except OSError:
            return False
        return True
    if _absent(path):
        return True
    # The top entry is chmod'ed first: os.walk cannot list an unreadable
    # directory, so nothing below it would be repaired otherwise.
    try:
        os.chmod(path, 0o700)
    except OSError:
        pass
    for parent, directories, _files in os.walk(path):
        for name in directories:
            child = os.path.join(parent, name)
            if os.path.islink(child):
                continue
            try:
                os.chmod(child, 0o700)
            except OSError:
                pass
    shutil.rmtree(path, ignore_errors=True)
    return _absent(path)


def _absent(path: Path) -> bool:
    """Whether `path` is known to be gone.

    `os.path.lexists` answers False on any error, so an entry behind a
    directory we cannot search would read as removed while it survives.
    """
    try:
        os.lstat(path)
    except (FileNotFoundError, NotADirectoryError):
        return True
    except (OSError, ValueError):
        return False
    return False


def _retain_attempt(state_root: Path, fleet_root: Path) -> None:
    """Keep a failed attempt's evidence, and bound how much of it accumulates.

    A rejected receipt carries a verdict and no cause, so deleting the runner
    log along with the attempt leaves the operator nothing to read. Drop the
    child's scratch tree, which is the large part and reconstructible, and keep
    the runner log and semantic result next to the enrollment that refused.

    Nothing here may raise: the caller runs it from a `finally`, where an
    exception would replace either the receipt or the error that explains the
    rejection. Every failure becomes a notice instead, and no failure deletes
    evidence — a retained attempt that could not be moved stays where it is.
    """
    scratch_dropped = _remove_tree(state_root / "tmp")
    retained = fleet_root / (
        RETAINED_ATTEMPT_PREFIX + state_root.name[len(ATTEMPT_PREFIX):]
    )
    try:
        os.replace(state_root, retained)
    except OSError as error:
        # Something already owns that name. Keeping the attempt where it is
        # beats deleting the only record of why acceptance refused.
        _notice(f"kept rejected acceptance attempt in place ({error}): {state_root}")
        return
    _notice(f"retained rejected acceptance attempt: {retained}")
    if not scratch_dropped:
        _notice(f"attempt scratch tree could not be removed: {retained / 'tmp'}")
    _prune_retained_attempts(fleet_root)


def _prune_retained_attempts(fleet_root: Path) -> None:
    """Keep the newest retained attempts and drop the rest, quietly.

    Only directories this function could itself have created are ranked. The
    name is a namespace an operator also writes in — the enrollment doc tells
    them this evidence is theirs to keep — so an archive left as
    `rejected-attempt.2026-09-23.tar.gz`, a symlink onto another volume, or an
    attempt renamed `rejected-attempt.x.investigating` must neither occupy the
    budget nor be removed by it.
    """
    dated: list[tuple[float, Path]] = []
    try:
        candidates = sorted(fleet_root.glob(RETAINED_ATTEMPT_PREFIX + "*"))
    except OSError:
        return
    for path in candidates:
        if not ATTEMPT_SUFFIX.fullmatch(path.name[len(RETAINED_ATTEMPT_PREFIX):]):
            continue
        try:
            info = path.lstat()
        except OSError:
            # An entry a concurrent run just removed.
            continue
        if not stat.S_ISDIR(info.st_mode):
            continue
        dated.append((info.st_mtime, path))
    dated.sort(reverse=True)
    for _mtime, path in dated[RETAINED_ATTEMPT_LIMIT:]:
        _remove_tree(path)


def _bounded_tail(path: Path, ceiling: int = 4096) -> str:
    try:
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - ceiling))
            return stream.read(ceiling).decode("utf-8", errors="replace").strip()
    except OSError:
        return ""


def _git_oid(cmux_root: Path, expression: str) -> str:
    environment = {
        "LC_ALL": "C",
        "LANG": "C",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
    }
    try:
        completed = subprocess.run(
            ["/usr/bin/git", "-C", str(cmux_root), "rev-parse", expression],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            text=True,
            check=False,
            timeout=15,
        )
    except subprocess.TimeoutExpired as error:
        raise FleetError("CMUX source identity probe timed out") from error
    value = completed.stdout.strip()
    if completed.returncode != 0 or COMMIT_RE.fullmatch(value) is None:
        raise FleetError("CMUX source identity is unavailable")
    return value


def _decode_bootstrap_output(raw: bytes) -> object:
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise FleetError("post-acceptance bootstrap exceeds size ceiling")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("post-acceptance bootstrap is invalid JSON") from error
    if raw != canonical(value):
        raise FleetError("post-acceptance bootstrap is not canonical")
    return value


def _run_bootstrap(
    bootstrap_script: Path,
    enrollment: dict[str, Any],
    role: str,
    cmux_root: Path,
    glaeda: Path,
    cache_root: Path | None,
    min_free_gib: int | None,
    environment: dict[str, str],
) -> dict[str, Any]:
    """Run this Glaeda's read-only bootstrap for the enrolled role and parse it."""
    argv = [
        sys.executable,
        "-I",
        str(bootstrap_script),
        "--platform",
        enrollment["os"]["family"],
        "--cmux-root",
        str(cmux_root),
        "--glaeda",
        str(glaeda),
        "--hardware-class",
        enrollment["hardwareCapabilityClass"],
        "--role",
        role,
    ]
    if cache_root is not None:
        argv.extend(["--cache-root", str(cache_root)])
    if min_free_gib is not None:
        argv.extend(["--min-free-gib", str(min_free_gib)])
    try:
        bootstrap = subprocess.run(
            argv,
            cwd=Path(__file__).resolve().parents[1],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            check=False,
            timeout=180,
        )
    except subprocess.TimeoutExpired as error:
        raise FleetError("post-acceptance bootstrap timed out") from error
    if bootstrap.returncode != 0:
        detail = bootstrap.stderr.decode("utf-8", errors="replace").strip()
        raise FleetError(
            "post-acceptance bootstrap refused"
            + (f": {detail[:1024]}" if detail else "")
        )
    value = _decode_bootstrap_output(bootstrap.stdout)
    if not isinstance(value, dict):
        raise FleetError("post-acceptance bootstrap is not an object")
    return value


def accept_local(
    enrollment_path: Path,
    cmux_root_value: Path,
    glaeda: Path,
    role: str,
    cache_root_value: Path | None = None,
    min_free_gib: int | None = None,
) -> dict[str, Any]:
    enrollment = validate_enrollment(load(enrollment_path))
    if enrollment["state"] != "enrolling":
        raise FleetError("local acceptance requires an enrolling node")
    if role not in enrollment["allowedExecutionRoles"]:
        raise FleetError("local acceptance role is outside the enrollment allowlist")

    cmux_root = cmux_root_value.resolve(strict=True)
    runner = cmux_root / CMUX_PROFILE_RUNNER
    if not runner.is_file() or runner.is_symlink():
        raise FleetError("CMUX workload profile runner is unavailable")
    if not profile_runner_interpreter_ready():
        raise FleetError(
            "this interpreter lacks os.waitid, which the CMUX profile runner "
            "requires; use CPython 3.13 or newer on macOS"
        )
    glaeda = glaeda.resolve(strict=True)
    if not glaeda.is_file() or not os.access(glaeda, os.X_OK):
        raise FleetError("Glaeda executable is unavailable")

    family = enrollment["os"]["family"]
    cache_root = (
        cache_root_value.resolve(strict=True)
        if cache_root_value is not None
        else None
    )
    if family == "macos" and cache_root is None:
        raise FleetError("macOS local acceptance requires --cache-root")
    if min_free_gib is not None and min_free_gib <= 0:
        raise FleetError("minimum free disk must be positive")

    source_commit = _git_oid(cmux_root, "HEAD^{commit}")
    source_tree = _git_oid(cmux_root, "HEAD^{tree}")
    profile = enrollment["roleProfiles"][role]
    fleet_contract_before = fleet_contract_generation()

    fleet_root = enrollment_path.resolve(strict=True).parent
    root_info = fleet_root.stat(follow_symlinks=False)
    if (
        not stat.S_ISDIR(root_info.st_mode)
        or root_info.st_uid != os.geteuid()
        or stat.S_IMODE(root_info.st_mode) & 0o077
    ):
        raise FleetError("fleet state directory ownership or mode is unsafe")

    bootstrap_script = Path(__file__).with_name("cmux_fleet_bootstrap.py")
    if not bootstrap_script.is_file() or bootstrap_script.is_symlink():
        raise FleetError("fleet bootstrap implementation is unavailable")

    state_root = Path(
        tempfile.mkdtemp(prefix=ATTEMPT_PREFIX, dir=fleet_root)
    ).resolve(strict=True)
    state_root.chmod(0o700)
    accepted = False
    try:
        result_path = state_root / "result.json"
        log_path = state_root / "cmux-runner.log"
        child_environment = acceptance_child_environment(state_root / "tmp")
        log_fd = os.open(
            log_path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
            0o600,
        )
        try:
            with os.fdopen(log_fd, "wb", closefd=True) as log:
                log_fd = -1
                completed = subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        str(runner),
                        "run",
                        profile["id"],
                        "--generation",
                        str(profile["generation"]),
                        "--commit",
                        source_commit,
                        "--tree",
                        source_tree,
                        "--state-class",
                        "cold",
                        "--result",
                        str(result_path),
                    ],
                    cwd=cmux_root,
                    stdin=subprocess.DEVNULL,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    env=child_environment,
                    check=False,
                )
        finally:
            if log_fd >= 0:
                os.close(log_fd)

        if not result_path.is_file():
            detail = _bounded_tail(log_path)
            suffix = f": {detail}" if detail else ""
            raise FleetError(
                f"CMUX local acceptance produced no semantic result{suffix}"
            )
        cmux_result, cmux_result_sha256 = load_cmux_semantic_result(result_path)
        expected_source = {
            "repository": CMUX_REPOSITORY,
            "commit": source_commit,
            "tree": source_tree,
        }
        if cmux_result.get("source") != expected_source:
            raise FleetError("CMUX acceptance result does not match requested source")

        post_bootstrap = _run_bootstrap(
            bootstrap_script,
            enrollment,
            role,
            cmux_root,
            glaeda,
            cache_root,
            min_free_gib,
            child_environment,
        )
        toolchain_generation = post_bootstrap.get("toolchainGeneration")
        sha256(toolchain_generation, "post-acceptance toolchain generation")
        if toolchain_generation not in enrollment["supportedToolchainGenerations"]:
            raise FleetError(
                "post-acceptance toolchain is outside enrollment allowlist"
            )

        if fleet_contract_generation() != fleet_contract_before:
            raise FleetError(
                "Glaeda fleet contract changed during local acceptance"
            )
        if (
            _git_oid(cmux_root, "HEAD^{commit}") != source_commit
            or _git_oid(cmux_root, "HEAD^{tree}") != source_tree
        ):
            raise FleetError("CMUX source changed during local acceptance")
        local_attempt = "sha256:" + hashlib.sha256(os.urandom(32)).hexdigest()
        receipt = finalize_acceptance(
            enrollment,
            role,
            toolchain_generation,
            cmux_result,
            post_bootstrap,
            cmux_result_sha256,
            execution_class=LOCAL_EXECUTION_CLASS,
            local_execution_attempt_sha256=local_attempt,
        )
        if completed.returncode == 0 and receipt["result"] != "accepted":
            raise FleetError("successful CMUX local run did not produce acceptance")
        if completed.returncode != 0 and receipt["result"] == "accepted":
            raise FleetError("failed CMUX local run produced acceptance")
        accepted = receipt["result"] == "accepted"
        return receipt
    finally:
        if accepted:
            if not _remove_tree(state_root):
                _notice(f"acceptance attempt directory remains: {state_root}")
        else:
            _retain_attempt(state_root, fleet_root)


CLASS_ACCEPTANCE_KEYS = {
    "schema", "fleetClass", "role", "profile", "architecture", "osVersionClass",
    "hardware", "toolchain", "toolchainGeneration", "glaedaGeneration",
    "glaedaFleetContractGeneration", "glaedaCandidate", "source",
    "cmuxSemanticResultSha256", "cmuxEnvironmentClass", "cmuxToolchainIdentity",
    "acceptingNodeId", "acceptingEnrollmentGeneration", "acceptingReceiptSha256",
    "receiptSha256",
}
# Compared field by field, in this order, when a node adopts a class receipt.
CLASS_IDENTITY_FIELDS = ("architecture", "osVersionClass", "hardware", "toolchain")


def fleet_class(value: object) -> str:
    if not isinstance(value, str) or FLEET_CLASS_RE.fullmatch(value) is None:
        raise FleetError("fleet class is invalid (lowercase, such as std or light)")
    return value


def toolchain_generation_of(toolchain: dict[str, Any]) -> str:
    """cmux_fleet_bootstrap's toolchainGeneration for these observations."""
    return "sha256:" + hashlib.sha256(canonical(toolchain)).hexdigest()


def _validate_toolchain_observations(value: object, label: str) -> dict[str, Any]:
    if (
        not isinstance(value, dict)
        or not value
        or len(value) > 32
    ):
        raise FleetError(f"{label} toolchain observations are invalid")
    for name, item in value.items():
        if not isinstance(name, str) or TOKEN_RE.fullmatch(name.lower()) is None:
            raise FleetError(f"{label} toolchain observation name {name!r} is invalid")
        if not ((isinstance(item, str) and len(item) <= 512) or type(item) is int):
            raise FleetError(f"{label} toolchain.{name} is not a short string or integer")
    return value


def _validate_hardware(value: object, label: str) -> dict[str, Any]:
    hardware = exact_keys(value, {"model", "chip", "memoryGiB"}, f"{label} hardware")
    for name in ("model", "chip"):
        if (
            not isinstance(hardware[name], str)
            or HARDWARE_TEXT_RE.fullmatch(hardware[name]) is None
        ):
            raise FleetError(f"{label} hardware {name} is invalid")
    positive_int(hardware["memoryGiB"], f"{label} hardware memory")
    return hardware


def bootstrap_class_identity(bootstrap_value: dict[str, Any]) -> dict[str, Any]:
    """What a class receipt binds, as this node's bootstrap reports it."""
    observed = bootstrap_value.get("observed")
    if not isinstance(observed, dict) or "hardware" not in observed or "toolchain" not in observed:
        raise FleetError(
            "bootstrap reports no hardware or toolchain identity; class acceptance "
            "needs a macOS bootstrap from a candidate that reports them"
        )
    toolchain = _validate_toolchain_observations(observed["toolchain"], "bootstrap")
    if toolchain_generation_of(toolchain) != bootstrap_value.get("toolchainGeneration"):
        raise FleetError("bootstrap toolchain observations disagree with its toolchain generation")
    return {
        "architecture": bootstrap_value.get("architecture"),
        "osVersionClass": bootstrap_value.get("osVersionClass"),
        "hardware": _validate_hardware(observed["hardware"], "bootstrap"),
        "toolchain": toolchain,
        "toolchainGeneration": bootstrap_value["toolchainGeneration"],
        "glaedaGeneration": bootstrap_value.get("glaedaGeneration"),
    }


def candidate_identity(glaeda_generation: str, root: Path | None = None) -> dict[str, str]:
    """The staged Glaeda candidate this code and binary came from.

    Read from the generation's manifest.json, and believed only where it names
    the bytes actually running: this file, the bootstrap, and the glaeda binary
    whose digest is the Glaeda generation. A checkout has no manifest, so class
    acceptance is a candidate-only path.
    """
    root = root or Path(__file__).resolve().parents[1]
    manifest_path = root / "manifest.json"
    try:
        raw = manifest_path.read_bytes()
    except OSError as error:
        raise FleetError(
            "class acceptance needs a staged Glaeda candidate "
            "(docs/FLEET_DISTRIBUTION.md); this cmux_fleet.py has no manifest.json"
        ) from error
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise FleetError("Glaeda candidate manifest exceeds size ceiling")
    try:
        manifest = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("Glaeda candidate manifest is invalid JSON") from error
    if not isinstance(manifest, dict) or manifest.get("schema") != CANDIDATE_MANIFEST_SCHEMA:
        raise FleetError("Glaeda candidate manifest schema is unsupported")
    files = manifest.get("files")
    if not isinstance(files, dict):
        raise FleetError("Glaeda candidate manifest lists no files")

    def listed(name: str) -> str | None:
        entry = files.get(name)
        value = entry.get("sha256") if isinstance(entry, dict) else None
        return "sha256:" + value if isinstance(value, str) else None

    running = {
        "scripts/cmux_fleet.py": _file_sha256(root / "scripts/cmux_fleet.py"),
        "scripts/cmux_fleet_bootstrap.py": _file_sha256(root / "scripts/cmux_fleet_bootstrap.py"),
        "bin/glaeda": glaeda_generation,
    }
    for name, actual in running.items():
        if listed(name) != actual:
            raise FleetError(
                f"Glaeda candidate manifest does not describe the running {name}"
            )
    source = exact_keys(
        manifest.get("source"), {"repository", "commit", "tree"}, "Glaeda candidate source"
    )
    if (
        not isinstance(source["repository"], str)
        or REPOSITORY_RE.fullmatch(source["repository"]) is None
        or not isinstance(source["commit"], str)
        or COMMIT_RE.fullmatch(source["commit"]) is None
        or not isinstance(source["tree"], str)
        or COMMIT_RE.fullmatch(source["tree"]) is None
    ):
        raise FleetError("Glaeda candidate source is invalid")
    return dict(source)


def validate_class_acceptance(value: object) -> dict[str, Any]:
    doc = exact_keys(value, CLASS_ACCEPTANCE_KEYS, "class acceptance receipt")
    if doc["schema"] != CLASS_ACCEPTANCE_SCHEMA:
        raise FleetError("class acceptance receipt schema is unsupported")
    fleet_class(doc["fleetClass"])
    if doc["role"] not in ENROLLABLE_ROLES:
        raise FleetError("class acceptance role lacks a reviewed v1 profile")
    if doc["profile"] != ROLE_PROFILES[doc["role"]]:
        raise FleetError("class acceptance profile is not the reviewed role profile")
    if doc["architecture"] not in {"arm64", "x86_64"}:
        raise FleetError("class acceptance architecture is unsupported")
    token(doc["osVersionClass"], "class acceptance OS version class")
    _validate_hardware(doc["hardware"], "class acceptance")
    _validate_toolchain_observations(doc["toolchain"], "class acceptance")
    if doc["toolchainGeneration"] != toolchain_generation_of(doc["toolchain"]):
        raise FleetError("class acceptance toolchain generation disagrees with its observations")
    for name in (
        "glaedaGeneration",
        "glaedaFleetContractGeneration",
        "cmuxSemanticResultSha256",
        "cmuxToolchainIdentity",
        "acceptingReceiptSha256",
        "receiptSha256",
    ):
        sha256(doc[name], f"class acceptance {name}")
    candidate = exact_keys(
        doc["glaedaCandidate"], {"repository", "commit", "tree"}, "class acceptance candidate"
    )
    source = exact_keys(doc["source"], {"repository", "commit", "tree"}, "class acceptance source")
    if (
        source["repository"] != CMUX_REPOSITORY
        or not isinstance(candidate["repository"], str)
        or REPOSITORY_RE.fullmatch(candidate["repository"]) is None
        or any(
            not isinstance(item[key], str) or COMMIT_RE.fullmatch(item[key]) is None
            for item in (candidate, source)
            for key in ("commit", "tree")
        )
    ):
        raise FleetError("class acceptance source or candidate is invalid")
    token(doc["cmuxEnvironmentClass"], "class acceptance CMUX environment class")
    if not isinstance(doc["acceptingNodeId"], str) or NODE_RE.fullmatch(doc["acceptingNodeId"]) is None:
        raise FleetError("class acceptance accepting node is invalid")
    positive_int(doc["acceptingEnrollmentGeneration"], "class acceptance enrollment generation")
    body = {name: item for name, item in doc.items() if name != "receiptSha256"}
    if doc["receiptSha256"] != digest(body):
        raise FleetError("class acceptance receipt digest does not match its content")
    if len(canonical(doc)) > MAX_STATUS_BYTES:
        raise FleetError("class acceptance receipt exceeds size ceiling")
    return doc


def build_class_acceptance(
    enrollment_value: object,
    receipt_value: object,
    bootstrap_value: object,
    fleet_class_value: str,
    candidate: dict[str, str],
) -> dict[str, Any]:
    """Promote one node's own local acceptance to a class receipt. Pure."""
    enrollment = validate_enrollment(enrollment_value)
    receipt = validate_acceptance_receipt(receipt_value)
    role = receipt["role"]
    if receipt["executionClass"] != LOCAL_EXECUTION_CLASS or receipt["result"] != "accepted":
        raise FleetError(
            "only an accepted local acceptance (a build on this node) can seed a class; "
            "a class-derived receipt does not chain"
        )
    if enrollment["state"] != "eligible":
        raise FleetError("export a class acceptance from an eligible node")
    current, reason = acceptance_matches_enrollment(enrollment, receipt, role)
    if not current:
        raise FleetError(f"the node's acceptance is not current ({reason})")
    validate_post_acceptance_bootstrap(enrollment, bootstrap_value, receipt["toolchainGeneration"])
    identity = bootstrap_class_identity(bootstrap_value)  # type: ignore[arg-type]
    body = {
        "schema": CLASS_ACCEPTANCE_SCHEMA,
        "fleetClass": fleet_class(fleet_class_value),
        "role": role,
        "profile": receipt["profile"],
        "architecture": identity["architecture"],
        "osVersionClass": identity["osVersionClass"],
        "hardware": identity["hardware"],
        "toolchain": identity["toolchain"],
        "toolchainGeneration": receipt["toolchainGeneration"],
        "glaedaGeneration": receipt["glaedaGeneration"],
        "glaedaFleetContractGeneration": receipt["glaedaFleetContractGeneration"],
        "glaedaCandidate": dict(candidate),
        "source": receipt["source"],
        "cmuxSemanticResultSha256": receipt["cmuxSemanticResultSha256"],
        "cmuxEnvironmentClass": receipt["cmuxEnvironmentClass"],
        "cmuxToolchainIdentity": receipt["cmuxToolchainIdentity"],
        "acceptingNodeId": receipt["nodeId"],
        "acceptingEnrollmentGeneration": receipt["enrollmentGeneration"],
        "acceptingReceiptSha256": digest(receipt),
    }
    return validate_class_acceptance({**body, "receiptSha256": digest(body)})


def _describe(value: object) -> str:
    text = json.dumps(value, sort_keys=True)
    return text if len(text) <= 80 else text[:77] + "..."


def class_acceptance_mismatches(
    class_receipt: dict[str, Any],
    identity: dict[str, Any],
    contract_generation: str,
    candidate: dict[str, str],
) -> list[str]:
    """Every field in which this node differs from the class receipt, named."""
    found: list[str] = []

    def compare(name: str, here: object, there: object) -> None:
        if here != there:
            found.append(f"{name} (here {_describe(here)}, class {_describe(there)})")

    for field in CLASS_IDENTITY_FIELDS:
        here, there = identity[field], class_receipt[field]
        if isinstance(here, dict) and isinstance(there, dict):
            for key in sorted(set(here) | set(there)):
                compare(f"{field}.{key}", here.get(key), there.get(key))
        else:
            compare(field, here, there)
    compare("toolchainGeneration", identity["toolchainGeneration"], class_receipt["toolchainGeneration"])
    compare("glaedaGeneration", identity["glaedaGeneration"], class_receipt["glaedaGeneration"])
    compare(
        "glaedaFleetContractGeneration",
        contract_generation,
        class_receipt["glaedaFleetContractGeneration"],
    )
    compare("glaedaCandidate", candidate, class_receipt["glaedaCandidate"])
    return found


def adopt_class_receipt(
    enrollment_value: object,
    class_value: object,
    expected_sha256: str,
    fleet_class_value: str,
    bootstrap_value: object,
    contract_generation: str,
    candidate: dict[str, str],
) -> dict[str, Any]:
    """This node's receipt from a class acceptance, or a refusal naming what differs. Pure."""
    sha256(expected_sha256, "expected class acceptance digest")
    enrollment = validate_enrollment(enrollment_value)
    if enrollment["state"] != "enrolling":
        raise FleetError("class acceptance adoption requires an enrolling node")
    klass = validate_class_acceptance(class_value)
    if klass["receiptSha256"] != expected_sha256:
        raise FleetError(
            f"class acceptance receipt is {klass['receiptSha256']}, "
            f"the operator expected {expected_sha256}"
        )
    if klass["fleetClass"] != fleet_class(fleet_class_value):
        raise FleetError(
            f"class acceptance is for class {klass['fleetClass']}, "
            f"this node is class {fleet_class_value}"
        )
    role = klass["role"]
    if role not in enrollment["allowedExecutionRoles"]:
        raise FleetError("class acceptance role is outside the enrollment allowlist")
    if klass["profile"] != enrollment["roleProfiles"][role]:
        raise FleetError("class acceptance profile differs from the enrolled role profile")
    if not isinstance(bootstrap_value, dict):
        raise FleetError("bootstrap is not an object")
    toolchain = sha256(bootstrap_value.get("toolchainGeneration"), "bootstrap toolchain generation")
    if toolchain not in enrollment["supportedToolchainGenerations"]:
        raise FleetError("bootstrap toolchain is outside the enrollment allowlist; renew the enrollment")
    post_bootstrap_sha256 = validate_post_acceptance_bootstrap(enrollment, bootstrap_value, toolchain)
    identity = bootstrap_class_identity(bootstrap_value)
    mismatches = class_acceptance_mismatches(klass, identity, contract_generation, candidate)
    if mismatches:
        raise FleetError(
            f"this node is not covered by the class {klass['fleetClass']} acceptance; "
            "run accept-local on it instead. Differs in: " + "; ".join(mismatches)
        )
    receipt = {
        "schema": ACCEPTANCE_SCHEMA,
        "nodeId": enrollment["nodeId"],
        "enrollmentGeneration": enrollment["enrollmentGeneration"],
        "role": role,
        "source": klass["source"],
        "profile": klass["profile"],
        "toolchainGeneration": toolchain,
        "glaedaGeneration": enrollment["glaedaGeneration"],
        "glaedaFleetContractGeneration": contract_generation,
        "cmuxSemanticResultSha256": klass["cmuxSemanticResultSha256"],
        "cmuxSemanticResultState": "passed",
        "cmuxEnvironmentClass": klass["cmuxEnvironmentClass"],
        "cmuxToolchainIdentity": klass["cmuxToolchainIdentity"],
        "postBootstrapSha256": post_bootstrap_sha256,
        "executionClass": CLASS_EXECUTION_CLASS,
        "localExecutionAttemptSha256": None,
        "classAcceptanceSha256": klass["receiptSha256"],
        "processSettlement": "complete",
        "result": "accepted",
    }
    return validate_acceptance_receipt(receipt)


def bootstrap_environment() -> dict[str, str]:
    """The closed environment accept-local gives its bootstrap, without a scratch TMPDIR."""
    environment = {
        "LC_ALL": "C",
        "LANG": "C",
        "PATH": os.environ.get(
            "PATH",
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        ),
    }
    for name in ACCEPTANCE_CHILD_ENV_KEYS:
        value = os.environ.get(name)
        if name != "PATH" and value:
            environment[name] = value
    return environment


def _observe_for_class(
    enrollment: dict[str, Any],
    role: str,
    cmux_root_value: Path,
    glaeda_value: Path,
    cache_root_value: Path | None,
    min_free_gib: int | None,
) -> tuple[dict[str, Any], str, dict[str, str]]:
    """A fresh read-only bootstrap, the running contract, and the candidate it came from."""
    if role not in enrollment["allowedExecutionRoles"]:
        raise FleetError("class acceptance role is outside the enrollment allowlist")
    cmux_root = cmux_root_value.resolve(strict=True)
    glaeda = glaeda_value.resolve(strict=True)
    if not glaeda.is_file() or not os.access(glaeda, os.X_OK):
        raise FleetError("Glaeda executable is unavailable")
    cache_root = cache_root_value.resolve(strict=True) if cache_root_value is not None else None
    if enrollment["os"]["family"] == "macos" and cache_root is None:
        raise FleetError("macOS class acceptance requires --cache-root")
    bootstrap_script = Path(__file__).with_name("cmux_fleet_bootstrap.py")
    if not bootstrap_script.is_file() or bootstrap_script.is_symlink():
        raise FleetError("fleet bootstrap implementation is unavailable")
    contract = fleet_contract_generation()
    observed = _run_bootstrap(
        bootstrap_script,
        enrollment,
        role,
        cmux_root,
        glaeda,
        cache_root,
        min_free_gib,
        bootstrap_environment(),
    )
    if fleet_contract_generation() != contract:
        raise FleetError("Glaeda fleet contract changed during the bootstrap")
    generation = sha256(observed.get("glaedaGeneration"), "bootstrap Glaeda generation")
    return observed, contract, candidate_identity(generation)  # type: ignore[arg-type]


def export_class_acceptance(
    enrollment_path: Path,
    acceptance_path: Path,
    fleet_class_value: str,
    cmux_root: Path,
    glaeda: Path,
    cache_root: Path | None = None,
    min_free_gib: int | None = None,
) -> dict[str, Any]:
    enrollment = validate_enrollment(load(enrollment_path))
    receipt = validate_acceptance_receipt(load(acceptance_path))
    observed, _contract, candidate = _observe_for_class(
        enrollment, receipt["role"], cmux_root, glaeda, cache_root, min_free_gib,
    )
    return build_class_acceptance(enrollment, receipt, observed, fleet_class_value, candidate)


def adopt_class_acceptance(
    enrollment_path: Path,
    class_path: Path,
    expected_sha256: str,
    fleet_class_value: str,
    cmux_root: Path,
    glaeda: Path,
    cache_root: Path | None = None,
    min_free_gib: int | None = None,
) -> dict[str, Any]:
    enrollment = validate_enrollment(load(enrollment_path))
    if enrollment["state"] != "enrolling":
        raise FleetError("class acceptance adoption requires an enrolling node")
    klass = validate_class_acceptance(load(class_path))
    observed, contract, candidate = _observe_for_class(
        enrollment, klass["role"], cmux_root, glaeda, cache_root, min_free_gib,
    )
    return adopt_class_receipt(
        enrollment, klass, expected_sha256, fleet_class_value, observed, contract, candidate,
    )


def apply_transition(
    enrollment_path: Path,
    target: str,
    reason: str | None,
    acceptance_paths: list[Path],
) -> dict[str, Any]:
    with FleetMutationLock(enrollment_path) as mutation:
        current = validate_enrollment(mutation.load_enrollment())
        acceptances = [load(path) for path in acceptance_paths]
        replacement = transition(current, target, reason, acceptances)
        durable_replace_enrollment(
            current,
            replacement,
            mutation,
        )
        return replacement

def apply_renewal(
    enrollment_path: Path, bootstrap_path: Path, expected_plan_sha256: str,
) -> dict[str, Any]:
    sha256(expected_plan_sha256, "expected renewal plan digest")
    with FleetMutationLock(enrollment_path) as mutation:
        current = validate_enrollment(mutation.load_enrollment())
        plan = renewal_plan(current, load(bootstrap_path))
        if plan["planSha256"] != expected_plan_sha256:
            raise FleetError("enrollment renewal plan changed; preview again")
        replacement = plan["replacement"]
        durable_replace_enrollment(current, replacement, mutation)
        return replacement


def emit(value: object) -> None:
    sys.stdout.buffer.write(canonical(value))


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="command", required=True)
    e = sub.add_parser("enroll")
    e.add_argument("bootstrap", type=Path)
    e.add_argument("--node-id", required=True)
    e.add_argument("--scope", required=True)
    e.add_argument("--generation", required=True, type=int)
    v = sub.add_parser("validate")
    v.add_argument("enrollment", type=Path)
    f = sub.add_parser("fingerprint")
    f.add_argument("enrollment", type=Path)
    s = sub.add_parser("status")
    s.add_argument("enrollment", type=Path)
    s.add_argument("--acceptance", action="append", type=Path, default=[])
    t = sub.add_parser("transition")
    t.add_argument("enrollment", type=Path)
    t.add_argument("--to", required=True, choices=STATES)
    t.add_argument("--reason", choices=QUARANTINE_REASONS)
    t.add_argument("--acceptance", action="append", type=Path, default=[])
    ta = sub.add_parser("transition-apply")
    ta.add_argument("enrollment", type=Path)
    ta.add_argument("--to", required=True, choices=STATES)
    ta.add_argument("--reason", choices=QUARANTINE_REASONS)
    ta.add_argument("--acceptance", action="append", type=Path, default=[])
    for command in ("renew-enrollment", "renew-enrollment-apply"):
        renewal = sub.add_parser(command)
        renewal.add_argument("enrollment", type=Path)
        renewal.add_argument("bootstrap", type=Path)
        if command.endswith("-apply"):
            renewal.add_argument("--expected-plan-sha256", required=True)
    al = sub.add_parser("accept-local")
    al.add_argument("enrollment", type=Path)
    al.add_argument("--cmux-root", type=Path, required=True)
    al.add_argument("--glaeda", type=Path, required=True)
    al.add_argument("--role", required=True, choices=sorted(ENROLLABLE_ROLES))
    al.add_argument("--cache-root", type=Path)
    al.add_argument("--min-free-gib", type=int)
    for command in ("export-class-acceptance", "adopt-class-acceptance"):
        ca = sub.add_parser(command)
        ca.add_argument("enrollment", type=Path)
        if command == "export-class-acceptance":
            ca.add_argument("--acceptance", type=Path, required=True)
        else:
            ca.add_argument("--class-receipt", type=Path, required=True)
            ca.add_argument("--expected-sha256", required=True)
        ca.add_argument("--fleet-class", required=True)
        ca.add_argument("--cmux-root", type=Path, required=True)
        ca.add_argument("--glaeda", type=Path, required=True)
        ca.add_argument("--cache-root", type=Path)
        ca.add_argument("--min-free-gib", type=int)
    vc = sub.add_parser("verify-class-acceptance")
    vc.add_argument("class_receipt", type=Path)
    a = sub.add_parser("finalize-acceptance")
    a.add_argument("enrollment", type=Path)
    a.add_argument("cmux_result", type=Path)
    a.add_argument("post_bootstrap", type=Path)
    a.add_argument("--role", required=True, choices=sorted(ENROLLABLE_ROLES))
    a.add_argument("--toolchain-generation", required=True)
    return p


def main() -> int:
    try:
        args = parser().parse_args()
        if args.command == "enroll":
            emit(
                enrollment_from_bootstrap(
                    load(args.bootstrap),
                    node_id=args.node_id,
                    operator_fleet_scope=args.scope,
                    enrollment_generation=args.generation,
                )
            )
            return 0
        if args.command == "verify-class-acceptance":
            emit(validate_class_acceptance(load(args.class_receipt)))
            return 0
        enrollment = load(args.enrollment)
        if args.command == "validate":
            emit(validate_enrollment(enrollment))
        elif args.command == "fingerprint":
            emit({"schema": ENROLLMENT_SCHEMA, "enrollmentSha256": digest(validate_enrollment(enrollment))})
        elif args.command == "status":
            emit(node_status(enrollment, [load(path) for path in args.acceptance]))
        elif args.command == "transition":
            emit(
                transition(
                    enrollment,
                    args.to,
                    args.reason,
                    [load(path) for path in args.acceptance],
                )
            )
        elif args.command == "transition-apply":
            emit(
                apply_transition(
                    args.enrollment,
                    args.to,
                    args.reason,
                    args.acceptance,
                )
            )
        elif args.command == "renew-enrollment":
            emit(renewal_plan(enrollment, load(args.bootstrap)))
        elif args.command == "renew-enrollment-apply":
            emit(apply_renewal(
                args.enrollment, args.bootstrap, args.expected_plan_sha256,
            ))
        elif args.command == "accept-local":
            receipt = accept_local(
                args.enrollment,
                args.cmux_root,
                args.glaeda,
                args.role,
                args.cache_root,
                args.min_free_gib,
            )
            emit(receipt)
            return 0 if receipt["result"] == "accepted" else 1
        elif args.command == "export-class-acceptance":
            emit(export_class_acceptance(
                args.enrollment, args.acceptance, args.fleet_class, args.cmux_root,
                args.glaeda, args.cache_root, args.min_free_gib,
            ))
        elif args.command == "adopt-class-acceptance":
            emit(adopt_class_acceptance(
                args.enrollment, args.class_receipt, args.expected_sha256, args.fleet_class,
                args.cmux_root, args.glaeda, args.cache_root, args.min_free_gib,
            ))
        elif args.command == "finalize-acceptance":
            cmux_result, cmux_result_sha256 = load_cmux_semantic_result(args.cmux_result)
            post_bootstrap = load(args.post_bootstrap)
            emit(
                finalize_acceptance(
                    enrollment,
                    args.role,
                    args.toolchain_generation,
                    cmux_result,
                    post_bootstrap,
                    cmux_result_sha256,
                )
            )
        else:
            raise FleetError("unsupported command")
        return 0
    except (OSError, FleetError) as error:
        print(json.dumps({"error": str(error)}, sort_keys=True, separators=(",", ":")), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
