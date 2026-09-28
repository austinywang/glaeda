#!/usr/bin/env python3
"""Pure CMUX fleet execution-role, capability, and local-admission model."""
from __future__ import annotations

import json
import re
from typing import Any, Iterable

import cmux_fleet as fleet

NODE_SCHEMA = "glaeda-cmux-node-capabilities/v1"
ROLE_CANARY_SCHEMA = "glaeda-cmux-role-canary/v1"
CAPACITY_SCHEMA = "glaeda-cmux-role-capacity/v1"
WORKLOAD_SCHEMA = "glaeda-cmux-workload-requirement/v1"
PREFERENCE_SCHEMA = "glaeda-cmux-placement-preference/v1"
STATUS_SCHEMA = "glaeda-cmux-execution-role-status/v1"
DECISION_SCHEMA = "glaeda-cmux-local-admission/v1"
PHYSICAL_LEASE_SCHEMA = "glaeda-cmux-physical-lease-observation/v1"

TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9._:-]{0,95}\Z")
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
NODE_RE = re.compile(r"cmux-[a-z0-9][a-z0-9-]{2,59}\Z")

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

RESOURCE_PROFILES = ("small", "medium", "large", "exclusive")
CAPABILITY_CLASSES = (
    "app_host_test",
    "apple_silicon",
    "artifact_service",
    "background_replay",
    "background_verification",
    "benchmark",
    "build_helper",
    "cgroup_v2",
    "diagnostic",
    "linux_agent",
    "linux_ci",
    "linux_ci_toolchain",
    "native_glaeda_build",
    "native_release_build",
    "native_xcode_build",
    "systemd_execution",
    "task_isolation",
    "web_ci",
)
CPU_CLASSES = ("small", "medium", "large")
MEMORY_CLASSES = ("small", "medium", "large", "xlarge")
DISK_CLASSES = ("small", "medium", "large")
PRESSURE_CLASSES = ("normal", "elevated", "critical", "unknown")
SWAP_CLASSES = ("none", "low", "elevated", "critical", "unknown")
THERMAL_CLASSES = ("stable", "elevated", "critical", "unknown")
PHYSICAL_LEASE_STATES = ("active", "releasing", "unsettled")
SLOT_CLASSES = (
    "artifact_publisher_slot",
    "artifact_service_slot",
    "background_replay_slot",
    "benchmark_slot",
    "browser_ui_test_slot",
    "diagnostic_slot",
    "linux_heavy_slot",
    "linux_medium_slot",
    "mac_app_host_test_slot",
    "mac_native_build_lane",
    "mac_native_heavy_slot",
    "mac_release_universal_slot",
)
ROUTABLE_STATES = {"eligible", "active"}
KNOWN_STATES = {
    "new",
    "discovered",
    "bootstrapping",
    "enrolling",
    "canary",
    "eligible",
    "active",
    "draining",
    "maintenance",
    "quarantined",
    "retired",
}

# Role identity stays small. More precise work belongs in capability and operation vocabulary.
ROLE_REQUIREMENTS = {
    "cmux_macos_native_build": {
        "platform": "macos",
        "architectures": {"arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {
            "apple_silicon",
            "native_glaeda_build",
            "native_xcode_build",
        },
        "requiresToolchainProfile": True,
    },
    "cmux_macos_test": {
        "platform": "macos",
        "architectures": {"arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {"apple_silicon", "app_host_test"},
        "requiresToolchainProfile": True,
    },
    "cmux_linux_ci": {
        "platform": "linux",
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {
            "cgroup_v2",
            "linux_ci_toolchain",
            "systemd_execution",
            "task_isolation",
        },
        "requiresToolchainProfile": True,
    },
    "cmux_linux_agent": {
        "platform": "linux",
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {"cgroup_v2", "systemd_execution", "task_isolation"},
        "requiresToolchainProfile": True,
    },
    "artifact_cache": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "large",
        "requiredCapabilities": {"artifact_service"},
        "requiresToolchainProfile": False,
    },
    "background_replay": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "small",
        "requiredCapabilities": {"background_replay"},
        "requiresToolchainProfile": False,
    },
    "benchmark": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "small",
        "requiredCapabilities": {"benchmark"},
        "requiresToolchainProfile": False,
    },
    "diagnostic": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "small",
        "requiredCapabilities": {"diagnostic"},
        "requiresToolchainProfile": False,
    },
}

# Useful work names semantic requirements. Scarce slot claims stay local.
OPERATION_CONTRACTS = {
    "cmux_macos_compile_admission": {
        "role": "cmux_macos_native_build",
        "requiredCapabilities": {"native_xcode_build"},
        "slotClaims": ("mac_native_build_lane", "mac_native_heavy_slot"),
    },
    "cmux_native_dev_build_admission": {
        "role": "cmux_macos_native_build",
        "requiredCapabilities": {"native_glaeda_build"},
        "slotClaims": ("mac_native_build_lane", "mac_native_heavy_slot"),
    },
    "cmux_macos_release_admission": {
        "role": "cmux_macos_native_build",
        "requiredCapabilities": {"native_release_build"},
        "slotClaims": (
            "artifact_publisher_slot",
            "mac_native_build_lane",
            "mac_native_heavy_slot",
            "mac_release_universal_slot",
        ),
    },
    "cmux_macos_app_host_test": {
        "role": "cmux_macos_test",
        "requiredCapabilities": {"app_host_test"},
        "slotClaims": ("mac_app_host_test_slot", "mac_native_heavy_slot"),
    },
    "cmux_linux_ci_admission": {
        "role": "cmux_linux_ci",
        "requiredCapabilities": {"linux_ci"},
        "slotClaims": ("linux_medium_slot",),
        "largeSlotClaims": ("linux_heavy_slot", "linux_medium_slot"),
    },
    "cmux_web_ci_admission": {
        "role": "cmux_linux_ci",
        "requiredCapabilities": {"web_ci"},
        "slotClaims": ("browser_ui_test_slot", "linux_medium_slot"),
        "largeSlotClaims": (
            "browser_ui_test_slot",
            "linux_heavy_slot",
            "linux_medium_slot",
        ),
    },
    "cmux_linux_agent_admission": {
        "role": "cmux_linux_agent",
        "requiredCapabilities": {"linux_agent"},
        "slotClaims": ("linux_medium_slot",),
        "largeSlotClaims": ("linux_heavy_slot", "linux_medium_slot"),
    },
    "cmux_build_helper_admission": {
        "role": "cmux_linux_agent",
        "requiredCapabilities": {"build_helper"},
        "slotClaims": ("linux_medium_slot",),
        "largeSlotClaims": ("linux_heavy_slot", "linux_medium_slot"),
    },
    "background_verification": {
        "role": "cmux_linux_ci",
        "requiredCapabilities": {"background_verification"},
        "slotClaims": ("linux_medium_slot",),
    },
    "artifact_cache_service": {
        "role": "artifact_cache",
        "requiredCapabilities": {"artifact_service"},
        "slotClaims": ("artifact_service_slot",),
    },
    "artifact_cache_publish": {
        "role": "artifact_cache",
        "requiredCapabilities": {"artifact_service"},
        "slotClaims": ("artifact_publisher_slot",),
    },
    "background_replay": {
        "role": "background_replay",
        "requiredCapabilities": {"background_replay"},
        "slotClaims": ("background_replay_slot",),
    },
    "benchmark": {
        "role": "benchmark",
        "requiredCapabilities": {"benchmark"},
        "slotClaims": ("benchmark_slot",),
    },
    "diagnostic": {
        "role": "diagnostic",
        "requiredCapabilities": {"diagnostic"},
        "slotClaims": ("diagnostic_slot",),
    },
}


class RoleModelError(RuntimeError):
    """Closed refusal for invalid role/capability evidence."""


def _exact(value: object, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise RoleModelError(f"{label} has unknown or missing fields")
    return value


def _token(value: object, label: str, *, optional: bool = False) -> str | None:
    if value is None and optional:
        return None
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise RoleModelError(f"{label} is invalid")
    return value


def _sha(value: object, label: str) -> str:
    if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
        raise RoleModelError(f"{label} is invalid")
    return value


def _positive_int(value: object, label: str, *, maximum: int = 2**31 - 1) -> int:
    if type(value) is not int or value < 1 or value > maximum:
        raise RoleModelError(f"{label} is invalid")
    return value


def _nonnegative_int(value: object, label: str, *, maximum: int = 2**31 - 1) -> int:
    if type(value) is not int or value < 0 or value > maximum:
        raise RoleModelError(f"{label} is invalid")
    return value


def _sorted_tokens(
    value: object, label: str, *, allowed: set[str] | None = None
) -> list[str]:
    if (
        not isinstance(value, list)
        or len(value) > 64
        or any(not isinstance(v, str) for v in value)
    ):
        raise RoleModelError(f"{label} is invalid")
    if value != sorted(set(value)):
        raise RoleModelError(f"{label} must be sorted and unique")
    for item in value:
        _token(item, label)
        if allowed is not None and item not in allowed:
            raise RoleModelError(f"{label} contains an unsupported value")
    return value


def _class_at_least(current: str, required: str, classes: tuple[str, ...]) -> bool:
    return classes.index(current) >= classes.index(required)


def fleet_acceptance_binding(value: object) -> dict[str, object]:
    try:
        receipt = fleet.validate_acceptance_receipt(value)
    except fleet.FleetError as error:
        raise RoleModelError("fleet acceptance receipt is invalid") from error
    if receipt["result"] != "accepted":
        raise RoleModelError("fleet acceptance receipt is not accepted")
    return {
        "role": receipt["role"],
        "nodeId": receipt["nodeId"],
        "enrollmentGeneration": receipt["enrollmentGeneration"],
        "profile": dict(receipt["profile"]),
        "receiptSha256": fleet.digest(receipt),
    }


def fleet_role_acceptances(
    node_id: str,
    enrollment_generation: int,
    values: Iterable[object],
) -> dict[str, dict[str, object]]:
    if not isinstance(node_id, str) or NODE_RE.fullmatch(node_id) is None:
        raise RoleModelError("nodeId is invalid")
    _positive_int(enrollment_generation, "enrollment generation")
    projected: dict[str, dict[str, object]] = {}
    for value in values:
        binding = fleet_acceptance_binding(value)
        if binding["nodeId"] != node_id:
            raise RoleModelError("fleet acceptance belongs to a different node")
        if binding["enrollmentGeneration"] != enrollment_generation:
            raise RoleModelError("fleet acceptance enrollment generation is stale")
        role = binding["role"]
        if role in projected:
            raise RoleModelError("duplicate fleet acceptance role")
        projected[role] = {
            "nodeId": binding["nodeId"],
            "enrollmentGeneration": binding["enrollmentGeneration"],
            "profile": binding["profile"],
            "receiptSha256": binding["receiptSha256"],
        }
    return projected


def validate_node(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "state",
            "platform",
            "architecture",
            "osVersionClass",
            "enrollmentGeneration",
            "cpuClass",
            "memoryClass",
            "diskClass",
            "capabilityGeneration",
            "toolchainProfiles",
            "capabilities",
            "roleAcceptances",
            "pressure",
        },
        "node capability",
    )
    if doc["schema"] != NODE_SCHEMA:
        raise RoleModelError("node capability schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("nodeId is invalid")
    if doc["state"] not in KNOWN_STATES:
        raise RoleModelError("node state is unsupported")
    if doc["platform"] not in {"macos", "linux"}:
        raise RoleModelError("node platform is unsupported")
    if doc["architecture"] not in {"arm64", "x86_64"}:
        raise RoleModelError("node architecture is unsupported")
    _token(doc["osVersionClass"], "OS version class")
    _positive_int(doc["enrollmentGeneration"], "enrollment generation")
    if doc["cpuClass"] not in CPU_CLASSES:
        raise RoleModelError("CPU class is unsupported")
    if doc["memoryClass"] not in MEMORY_CLASSES:
        raise RoleModelError("memory class is unsupported")
    if doc["diskClass"] not in DISK_CLASSES:
        raise RoleModelError("disk class is unsupported")
    _positive_int(doc["capabilityGeneration"], "capability generation")
    _sorted_tokens(doc["toolchainProfiles"], "toolchain profiles")
    _sorted_tokens(
        doc["capabilities"],
        "node capabilities",
        allowed=set(CAPABILITY_CLASSES),
    )
    role_acceptances = doc["roleAcceptances"]
    if not isinstance(role_acceptances, dict):
        raise RoleModelError("role acceptances are invalid")
    for role, acceptance in role_acceptances.items():
        if role not in fleet.ENROLLABLE_ROLES:
            raise RoleModelError("role acceptance is not reviewed by fleet enrollment")
        entry = _exact(
            acceptance,
            {"nodeId", "enrollmentGeneration", "profile", "receiptSha256"},
            "role acceptance",
        )
        if entry["nodeId"] != doc["nodeId"]:
            raise RoleModelError("role acceptance belongs to a different node")
        if entry["enrollmentGeneration"] != doc["enrollmentGeneration"]:
            raise RoleModelError("role acceptance enrollment generation is stale")
        profile = _exact(entry["profile"], {"id", "generation"}, "role acceptance profile")
        _token(profile["id"], "role acceptance profile id")
        _positive_int(profile["generation"], "role acceptance profile generation")
        if profile != fleet.ROLE_PROFILES.get(role):
            raise RoleModelError("role acceptance profile is not current")
        _sha(entry["receiptSha256"], "role acceptance receipt digest")
    pressure = _exact(
        doc["pressure"], {"cpu", "memory", "swap", "thermal"}, "pressure"
    )
    if (
        pressure["cpu"] not in PRESSURE_CLASSES
        or pressure["memory"] not in PRESSURE_CLASSES
    ):
        raise RoleModelError("CPU/memory pressure class is unsupported")
    if pressure["swap"] not in SWAP_CLASSES:
        raise RoleModelError("swap class is unsupported")
    if pressure["thermal"] not in THERMAL_CLASSES:
        raise RoleModelError("thermal class is unsupported")
    return doc


def validate_role_canary(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "enrollmentGeneration",
            "capabilityGeneration",
            "role",
            "toolchainProfile",
            "acceptedCapabilities",
            "acceptedResourceProfiles",
            "profile",
            "acceptanceReceiptSha256",
            "result",
        },
        "role canary",
    )
    if doc["schema"] != ROLE_CANARY_SCHEMA:
        raise RoleModelError("role canary schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("role canary nodeId is invalid")
    _positive_int(doc["enrollmentGeneration"], "role canary enrollment generation")
    _positive_int(
        doc["capabilityGeneration"], "role canary capability generation"
    )
    if doc["role"] not in ROLES:
        raise RoleModelError("role canary role is unsupported")
    _token(
        doc["toolchainProfile"], "role canary toolchain profile", optional=True
    )
    _sorted_tokens(
        doc["acceptedCapabilities"],
        "accepted capabilities",
        allowed=set(CAPABILITY_CLASSES),
    )
    _sorted_tokens(
        doc["acceptedResourceProfiles"],
        "accepted resource profiles",
        allowed=set(RESOURCE_PROFILES),
    )
    profile = _exact(doc["profile"], {"id", "generation"}, "role canary profile")
    _token(profile["id"], "role canary profile id")
    _positive_int(profile["generation"], "role canary profile generation")
    _sha(doc["acceptanceReceiptSha256"], "role canary acceptance receipt digest")
    if doc["result"] not in {"accepted", "rejected"}:
        raise RoleModelError("role canary result is invalid")
    return doc


def validate_capacity(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "enrollmentGeneration",
            "capabilityGeneration",
            "role",
            "resourceProfile",
            "slotClass",
            "maxConcurrent",
            "contentionEvidenceGeneration",
            "measurement",
            "result",
        },
        "capacity evidence",
    )
    if doc["schema"] != CAPACITY_SCHEMA:
        raise RoleModelError("capacity schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("capacity nodeId is invalid")
    _positive_int(doc["enrollmentGeneration"], "capacity enrollment generation")
    _positive_int(doc["capabilityGeneration"], "capacity generation")
    if doc["role"] not in ROLES:
        raise RoleModelError("capacity role is unsupported")
    if doc["resourceProfile"] not in RESOURCE_PROFILES:
        raise RoleModelError("resource profile is unsupported")
    _token(doc["slotClass"], "slot class")
    if doc["slotClass"] not in SLOT_CLASSES:
        raise RoleModelError("slot class is unsupported")
    _positive_int(doc["maxConcurrent"], "max concurrent", maximum=16)
    _sha(doc["contentionEvidenceGeneration"], "contention evidence generation")
    measurement = _exact(
        doc["measurement"],
        {
            "offeredTasks",
            "maximumSimultaneous",
            "startedTasks",
            "settledTasks",
            "validatedCompletions",
            "p50Millis",
            "p90Millis",
            "cpuPressure",
            "memoryPressure",
            "swapStartBytes",
            "swapPeakBytes",
            "swapEndBytes",
            "thermalBehavior",
            "unfinishedWork",
        },
        "capacity measurement",
    )
    offered = _nonnegative_int(
        measurement["offeredTasks"],
        "offered tasks",
        maximum=16,
    )
    maximum_simultaneous = _nonnegative_int(
        measurement["maximumSimultaneous"],
        "maximum simultaneous tasks",
        maximum=16,
    )
    started = _nonnegative_int(
        measurement["startedTasks"],
        "started tasks",
        maximum=16,
    )
    settled = _nonnegative_int(
        measurement["settledTasks"],
        "settled tasks",
        maximum=16,
    )
    validated = _nonnegative_int(
        measurement["validatedCompletions"],
        "validated completions",
        maximum=16,
    )
    if not 0 <= maximum_simultaneous <= started <= offered:
        raise RoleModelError("capacity cohort counts are inconsistent")
    if settled > started or validated > settled:
        raise RoleModelError("capacity settled/completion counts are inconsistent")
    _positive_int(measurement["p50Millis"], "p50 millis")
    _positive_int(measurement["p90Millis"], "p90 millis")
    if measurement["p90Millis"] < measurement["p50Millis"]:
        raise RoleModelError("p90 cannot be lower than p50")
    if measurement["cpuPressure"] not in PRESSURE_CLASSES:
        raise RoleModelError("capacity CPU pressure is unsupported")
    if measurement["memoryPressure"] not in PRESSURE_CLASSES:
        raise RoleModelError("capacity memory pressure is unsupported")
    _nonnegative_int(measurement["swapStartBytes"], "swap start bytes")
    _nonnegative_int(measurement["swapPeakBytes"], "swap peak bytes")
    _nonnegative_int(measurement["swapEndBytes"], "swap end bytes")
    if measurement["swapPeakBytes"] < max(
        measurement["swapStartBytes"], measurement["swapEndBytes"]
    ):
        raise RoleModelError("swap peak bytes cannot be below start or end")
    if measurement["thermalBehavior"] not in THERMAL_CLASSES:
        raise RoleModelError("capacity thermal class is unsupported")
    unfinished = _nonnegative_int(
        measurement["unfinishedWork"], "unfinished work", maximum=16
    )
    if unfinished != offered - settled:
        raise RoleModelError("capacity unfinished work disagrees with cohort counts")
    if doc["result"] not in {"accepted", "rejected"}:
        raise RoleModelError("capacity result is invalid")
    if doc["result"] == "accepted":
        if validated == 0:
            raise RoleModelError("accepted capacity requires validated completions")
        if unfinished != 0 or settled != offered:
            raise RoleModelError("accepted capacity requires all offered work settled")
        if validated != settled:
            raise RoleModelError(
                "accepted capacity requires every settled task validated"
            )
        if maximum_simultaneous < doc["maxConcurrent"]:
            raise RoleModelError(
                "accepted capacity concurrency exceeds measured simultaneous work"
            )
        if validated < doc["maxConcurrent"]:
            raise RoleModelError(
                "accepted capacity concurrency exceeds validated completions"
            )
        if measurement["cpuPressure"] in {"critical", "unknown"}:
            raise RoleModelError("accepted capacity requires bounded CPU pressure")
        if measurement["memoryPressure"] in {"critical", "unknown"}:
            raise RoleModelError("accepted capacity requires bounded memory pressure")
        if measurement["thermalBehavior"] == "critical":
            raise RoleModelError("accepted capacity cannot use critical thermal evidence")
    return doc


def validate_workload(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "operation",
            "role",
            "platform",
            "architecture",
            "toolchainProfile",
            "minimumCpuClass",
            "minimumMemoryClass",
            "requiredCapabilities",
            "resourceProfile",
        },
        "workload requirement",
    )
    if doc["schema"] != WORKLOAD_SCHEMA:
        raise RoleModelError("workload schema is unsupported")
    operation = _token(doc["operation"], "operation")
    if operation not in OPERATION_CONTRACTS:
        raise RoleModelError("operation is unsupported")
    contract = OPERATION_CONTRACTS[operation]
    if doc["role"] != contract["role"]:
        raise RoleModelError("operation and role disagree")
    if doc["platform"] not in {"macos", "linux"}:
        raise RoleModelError("workload platform is unsupported")
    if doc["architecture"] not in {"arm64", "x86_64"}:
        raise RoleModelError("workload architecture is unsupported")
    _token(
        doc["toolchainProfile"], "workload toolchain profile", optional=True
    )
    if doc["minimumCpuClass"] not in CPU_CLASSES:
        raise RoleModelError("workload CPU class is unsupported")
    if doc["minimumMemoryClass"] not in MEMORY_CLASSES:
        raise RoleModelError("workload memory class is unsupported")
    required = _sorted_tokens(
        doc["requiredCapabilities"],
        "required capabilities",
        allowed=set(CAPABILITY_CLASSES),
    )
    if not set(contract["requiredCapabilities"]).issubset(required):
        raise RoleModelError("workload omits operation-required capability")
    if doc["resourceProfile"] not in RESOURCE_PROFILES:
        raise RoleModelError("workload resource profile is unsupported")
    return doc


def validate_preference(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "role",
            "operation",
            "evidenceGeneration",
            "preference",
            "authority",
        },
        "placement preference",
    )
    if doc["schema"] != PREFERENCE_SCHEMA:
        raise RoleModelError("preference schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("preference nodeId is invalid")
    if doc["role"] not in ROLES:
        raise RoleModelError("preference role is unsupported")
    operation = _token(doc["operation"], "preference operation")
    if operation not in OPERATION_CONTRACTS:
        raise RoleModelError("preference operation is unsupported")
    if OPERATION_CONTRACTS[operation]["role"] != doc["role"]:
        raise RoleModelError("preference operation and role disagree")
    _sha(doc["evidenceGeneration"], "preference evidence generation")
    if doc["preference"] not in {"preferred", "neutral", "background"}:
        raise RoleModelError("preference value is unsupported")
    if doc["authority"] != "observation_only":
        raise RoleModelError(
            "preference authority must remain observation_only"
        )
    return doc


def role_eligibility(
    node_value: object, canary_values: Iterable[object]
) -> dict[str, dict[str, Any]]:
    node = validate_node(node_value)
    canaries: dict[str, dict[str, Any]] = {}
    for value in canary_values:
        canary = validate_role_canary(value)
        if canary["nodeId"] != node["nodeId"]:
            raise RoleModelError("role canary belongs to a different node")
        if canary["role"] in canaries:
            raise RoleModelError("duplicate role canary")
        canaries[canary["role"]] = canary

    results: dict[str, dict[str, Any]] = {}
    capabilities = set(node["capabilities"])
    toolchains = set(node["toolchainProfiles"])
    for role in ROLES:
        requirement = ROLE_REQUIREMENTS[role]
        if node["state"] not in ROUTABLE_STATES:
            results[role] = {
                "eligible": False,
                "reason": f"node_{node['state']}",
            }
            continue
        platform = requirement["platform"]
        if platform is not None and platform != node["platform"]:
            results[role] = {
                "eligible": False,
                "reason": "platform_mismatch",
            }
            continue
        if node["architecture"] not in requirement["architectures"]:
            results[role] = {
                "eligible": False,
                "reason": "architecture_mismatch",
            }
            continue
        if not _class_at_least(
            node["cpuClass"],
            requirement["minimumCpuClass"],
            CPU_CLASSES,
        ):
            results[role] = {
                "eligible": False,
                "reason": "insufficient_cpu_class",
            }
            continue
        if not _class_at_least(
            node["memoryClass"],
            requirement["minimumMemoryClass"],
            MEMORY_CLASSES,
        ):
            results[role] = {
                "eligible": False,
                "reason": "insufficient_memory_class",
            }
            continue
        if not _class_at_least(
            node["diskClass"],
            requirement["minimumDiskClass"],
            DISK_CLASSES,
        ):
            results[role] = {
                "eligible": False,
                "reason": "insufficient_disk_class",
            }
            continue
        if not requirement["requiredCapabilities"].issubset(capabilities):
            results[role] = {
                "eligible": False,
                "reason": "node_capability_missing",
            }
            continue
        current_acceptance = node["roleAcceptances"].get(role)
        if current_acceptance is None:
            results[role] = {
                "eligible": False,
                "reason": "fleet_acceptance_pending",
            }
            continue
        canary = canaries.get(role)
        if canary is None:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_pending",
            }
            continue
        if canary["profile"] != current_acceptance["profile"]:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_profile_stale",
            }
            continue
        if (
            canary["acceptanceReceiptSha256"]
            != current_acceptance["receiptSha256"]
        ):
            results[role] = {
                "eligible": False,
                "reason": "role_canary_acceptance_stale",
            }
            continue
        if canary["result"] != "accepted":
            results[role] = {
                "eligible": False,
                "reason": "role_canary_failed",
            }
            continue
        if canary["enrollmentGeneration"] != node["enrollmentGeneration"]:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_enrollment_stale",
            }
            continue
        if canary["capabilityGeneration"] != node["capabilityGeneration"]:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_pending",
            }
            continue
        if not set(requirement["requiredCapabilities"]).issubset(
            canary["acceptedCapabilities"]
        ):
            results[role] = {
                "eligible": False,
                "reason": "role_canary_capability_incomplete",
            }
            continue
        if requirement["requiresToolchainProfile"]:
            profile = canary["toolchainProfile"]
            if profile is None or profile not in toolchains:
                results[role] = {
                    "eligible": False,
                    "reason": "toolchain_canary_pending",
                }
                continue
        results[role] = {
            "eligible": True,
            "reason": "accepted",
            "toolchainProfile": canary["toolchainProfile"],
            "acceptedCapabilities": canary["acceptedCapabilities"],
            "acceptedResourceProfiles": canary[
                "acceptedResourceProfiles"
            ],
        }
    return results


def measured_slot_capacity(
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
) -> dict[str, int]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    node = validate_node(node_value)
    eligibility = role_eligibility(node, canary_values)
    slots: dict[str, int] = {}
    for value in capacity_values:
        receipt = validate_capacity(value)
        if receipt["nodeId"] != node["nodeId"]:
            raise RoleModelError(
                "capacity evidence belongs to a different node"
            )
        if receipt["enrollmentGeneration"] != node["enrollmentGeneration"]:
            continue
        if receipt["capabilityGeneration"] != node["capabilityGeneration"]:
            continue
        if receipt["result"] != "accepted":
            continue
        role = receipt["role"]
        if not eligibility[role]["eligible"]:
            continue
        if (
            receipt["resourceProfile"]
            not in eligibility[role]["acceptedResourceProfiles"]
        ):
            continue
        slots[receipt["slotClass"]] = max(
            slots.get(receipt["slotClass"], 0), receipt["maxConcurrent"]
        )
    return dict(sorted(slots.items()))


def _slot_claims(workload: dict[str, Any]) -> tuple[str, ...]:
    contract = OPERATION_CONTRACTS[workload["operation"]]
    if (
        workload["resourceProfile"] in {"large", "exclusive"}
        and "largeSlotClaims" in contract
    ):
        return contract["largeSlotClaims"]
    return contract["slotClaims"]


def _capacity_sets_for_workload(
    node: dict[str, Any],
    workload: dict[str, Any],
    capacity_values: Iterable[object],
) -> list[dict[str, int]]:
    claims = set(_slot_claims(workload))
    by_generation: dict[str, dict[str, int]] = {}
    for value in capacity_values:
        receipt = validate_capacity(value)
        if (
            receipt["nodeId"] != node["nodeId"]
            or receipt["enrollmentGeneration"] != node["enrollmentGeneration"]
            or receipt["capabilityGeneration"] != node["capabilityGeneration"]
            or receipt["role"] != workload["role"]
            or receipt["resourceProfile"] != workload["resourceProfile"]
            or receipt["result"] != "accepted"
        ):
            continue
        generation = receipt["contentionEvidenceGeneration"]
        slots = by_generation.setdefault(generation, {})
        slot = receipt["slotClass"]
        if slot in slots and slots[slot] != receipt["maxConcurrent"]:
            raise RoleModelError(
                "one contention evidence generation disagrees on slot capacity"
            )
        slots[slot] = receipt["maxConcurrent"]
    return [
        slots
        for _, slots in sorted(by_generation.items())
        if claims.issubset(slots)
    ]


def _profile_has_capacity(
    node: dict[str, Any],
    workload: dict[str, Any],
    eligibility: dict[str, dict[str, Any]],
    capacity_values: Iterable[object],
) -> bool:
    role = workload["role"]
    if (
        workload["resourceProfile"]
        not in eligibility[role].get("acceptedResourceProfiles", [])
    ):
        return False
    return bool(_capacity_sets_for_workload(node, workload, capacity_values))


def select_eligible(
    workload_value: object,
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
) -> tuple[bool, str]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    workload = validate_workload(workload_value)
    node = validate_node(node_value)
    eligibility = role_eligibility(node, canary_values)
    role = workload["role"]
    if not eligibility[role]["eligible"]:
        return False, eligibility[role]["reason"]
    if workload["platform"] != node["platform"]:
        return False, "platform_mismatch"
    if workload["architecture"] != node["architecture"]:
        return False, "architecture_mismatch"
    if not _class_at_least(
        node["cpuClass"],
        workload["minimumCpuClass"],
        CPU_CLASSES,
    ):
        return False, "insufficient_cpu_class"
    if not _class_at_least(
        node["memoryClass"],
        workload["minimumMemoryClass"],
        MEMORY_CLASSES,
    ):
        return False, "insufficient_memory_class"
    if not set(workload["requiredCapabilities"]).issubset(
        node["capabilities"]
    ):
        return False, "node_capability_missing"
    if not set(workload["requiredCapabilities"]).issubset(
        eligibility[role].get("acceptedCapabilities", [])
    ):
        return False, "role_canary_capability_incomplete"
    requested_toolchain = workload["toolchainProfile"]
    if requested_toolchain is not None:
        if requested_toolchain not in node["toolchainProfiles"]:
            return False, "toolchain_profile_missing"
        if eligibility[role].get("toolchainProfile") != requested_toolchain:
            return False, "toolchain_profile_ineligible"
    if not _profile_has_capacity(
        node, workload, eligibility, capacity_values
    ):
        return False, "resource_profile_unmeasured"
    return True, "eligible"


def _workload_slot_capacities(
    node: dict[str, Any],
    workload: dict[str, Any],
    capacity_values: Iterable[object],
) -> dict[str, int]:
    complete = _capacity_sets_for_workload(node, workload, capacity_values)
    if not complete:
        return {}
    claims = _slot_claims(workload)
    return {
        slot: min(capacities[slot] for capacities in complete)
        for slot in claims
    }


def validate_physical_lease(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "leaseId",
            "leaseGeneration",
            "ownerNamespace",
            "state",
            "slotClaims",
            "exclusive",
        },
        "physical lease",
    )
    if doc["schema"] != PHYSICAL_LEASE_SCHEMA:
        raise RoleModelError("physical lease schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("physical lease nodeId is invalid")
    _token(doc["leaseId"], "physical lease ID")
    _positive_int(doc["leaseGeneration"], "physical lease generation")
    _token(doc["ownerNamespace"], "physical lease owner namespace")
    if doc["state"] not in PHYSICAL_LEASE_STATES:
        raise RoleModelError("physical lease state is unsupported")
    _sorted_tokens(
        doc["slotClaims"],
        "physical lease slot claims",
        allowed=set(SLOT_CLASSES),
    )
    if type(doc["exclusive"]) is not bool:
        raise RoleModelError("physical lease exclusive flag is invalid")
    return doc


def physical_slot_usage(
    node_value: object, lease_values: Iterable[object]
) -> tuple[dict[str, int], int, bool]:
    node = validate_node(node_value)
    usage: dict[str, int] = {}
    lease_count = 0
    exclusive = False
    lease_ids: set[str] = set()
    for value in lease_values:
        lease = validate_physical_lease(value)
        if lease["nodeId"] != node["nodeId"]:
            raise RoleModelError("physical lease belongs to a different node")
        if lease["leaseId"] in lease_ids:
            raise RoleModelError("duplicate physical lease ID")
        lease_ids.add(lease["leaseId"])
        lease_count += 1
        exclusive = exclusive or lease["exclusive"]
        for slot in lease["slotClaims"]:
            usage[slot] = usage.get(slot, 0) + 1
    return dict(sorted(usage.items())), lease_count, exclusive


def local_admission(
    workload_value: object,
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
    physical_lease_values: Iterable[object],
) -> dict[str, Any]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    physical_lease_values = list(physical_lease_values)
    workload = validate_workload(workload_value)
    node = validate_node(node_value)
    ok, reason = select_eligible(
        workload, node, canary_values, capacity_values
    )
    if not ok:
        return {
            "schema": DECISION_SCHEMA,
            "authority": "admission_only",
            "leaseBoundary": None,
            "nodeId": node["nodeId"],
            "operation": workload["operation"],
            "accepted": False,
            "reason": reason,
            "resourceProfile": workload["resourceProfile"],
            "physicalLeaseRequired": False,
            "slotClaims": [],
        }

    pressure = node["pressure"]
    if (
        pressure["cpu"] in {"critical", "unknown"}
        or pressure["memory"] in {"critical", "unknown"}
        or pressure["swap"] in {"critical", "unknown"}
        or pressure["thermal"] in {"critical", "unknown"}
    ):
        return {
            "schema": DECISION_SCHEMA,
            "authority": "admission_only",
            "leaseBoundary": None,
            "nodeId": node["nodeId"],
            "operation": workload["operation"],
            "accepted": False,
            "reason": "node_pressured",
            "resourceProfile": workload["resourceProfile"],
            "physicalLeaseRequired": False,
            "slotClaims": [],
        }

    capacities = _workload_slot_capacities(node, workload, capacity_values)
    claims = list(_slot_claims(workload))
    held_slots, held_lease_count, held_exclusive = physical_slot_usage(
        node, physical_lease_values
    )
    wants_exclusive = workload["resourceProfile"] == "exclusive"
    if (wants_exclusive and held_lease_count > 0) or held_exclusive:
        return {
            "schema": DECISION_SCHEMA,
            "authority": "admission_only",
            "leaseBoundary": "physical_execution_lease",
            "nodeId": node["nodeId"],
            "operation": workload["operation"],
            "accepted": False,
            "reason": "physical_exclusive_unavailable",
            "resourceProfile": workload["resourceProfile"],
            "physicalLeaseRequired": True,
            "slotClaims": claims,
        }
    for slot in claims:
        if capacities.get(slot, 0) <= held_slots.get(slot, 0):
            return {
                "schema": DECISION_SCHEMA,
                "authority": "admission_only",
                "leaseBoundary": "physical_execution_lease",
                "nodeId": node["nodeId"],
                "operation": workload["operation"],
                "accepted": False,
                "reason": "physical_slot_unavailable",
                "resourceProfile": workload["resourceProfile"],
                "physicalLeaseRequired": True,
                "slotClaims": claims,
            }
    return {
        "schema": DECISION_SCHEMA,
        "authority": "admission_only",
        "leaseBoundary": "physical_execution_lease" if claims or wants_exclusive else None,
        "nodeId": node["nodeId"],
        "operation": workload["operation"],
        "accepted": True,
        "reason": "admitted",
        "resourceProfile": workload["resourceProfile"],
        "physicalLeaseRequired": bool(claims) or wants_exclusive,
        "slotClaims": claims,
    }


def operator_status(
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
    preference_values: Iterable[object] = (),
) -> dict[str, Any]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    preference_values = list(preference_values)
    node = validate_node(node_value)
    eligibility = role_eligibility(node, canary_values)
    preferred: list[str] = []
    background: list[str] = []
    for value in preference_values:
        preference = validate_preference(value)
        if preference["nodeId"] != node["nodeId"]:
            raise RoleModelError(
                "preference belongs to a different node"
            )
        if preference["preference"] == "preferred":
            preferred.append(preference["operation"])
        elif preference["preference"] == "background":
            background.append(preference["operation"])
    preferred = sorted(set(preferred))
    background = sorted(set(background))
    eligible = [
        role for role in ROLES if eligibility[role]["eligible"]
    ]
    unavailable = [
        {"role": role, "reason": eligibility[role]["reason"]}
        for role in ROLES
        if not eligibility[role]["eligible"]
        and eligibility[role]["reason"]
        not in {"platform_mismatch", "architecture_mismatch"}
    ]
    return {
        "schema": STATUS_SCHEMA,
        "node": node["nodeId"],
        "eligible": eligible,
        "temporarilyUnavailable": unavailable,
        "capacity": measured_slot_capacity(
            node, canary_values, capacity_values
        ),
        "preferred": preferred,
        "background": background,
        "state": node["state"],
    }


def render_status(status_value: object) -> str:
    status = _exact(
        status_value,
        {
            "schema",
            "node",
            "eligible",
            "temporarilyUnavailable",
            "capacity",
            "preferred",
            "background",
            "state",
        },
        "operator status",
    )
    if status["schema"] != STATUS_SCHEMA:
        raise RoleModelError("operator status schema is unsupported")
    lines = [f"node {status['node']}", "eligible:"]
    lines.extend(f"  {role}" for role in status["eligible"])
    lines.append("temporarily unavailable:")
    for item in status["temporarilyUnavailable"]:
        lines.append(f"  {item['role']}: {item['reason']}")
    lines.append("capacity:")
    lines.extend(
        f"  {slot}: {count}"
        for slot, count in status["capacity"].items()
    )
    lines.append("preferred:")
    lines.extend(f"  {role}" for role in status["preferred"])
    lines.append("background:")
    lines.extend(f"  {role}" for role in status["background"])
    lines.append(f"state: {status['state']}")
    return "\n".join(lines) + "\n"


def render_json(value: object) -> str:
    return (
        json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        )
        + "\n"
    )
