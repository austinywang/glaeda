#!/usr/bin/env python3
"""Publish and consume bounded advisory resident-node snapshots through GitHub."""

from __future__ import annotations

import argparse
import datetime as dt
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import stat
import tempfile
from typing import Any


NODE_DOCUMENT = "glaeda-github-resident-node-snapshot"
FLEET_DOCUMENT = "glaeda-github-resident-fleet"
TRUST_DOCUMENT = "glaeda-github-resident-trust"
VIEW_DOCUMENT = "glaeda-github-resident-view"
PUBLICATION_DOCUMENT = "glaeda-github-resident-publication-receipt"
SCHEMA_VERSION = 1
SIGNING_NAMESPACE = "glaeda-github-resident-node-snapshot-v1"
STATUS_BRANCH = "glaeda-status/v1"
STATUS_FILE = "resident-nodes.json"
MAX_NODE_BYTES = 16 * 1024
MAX_FLEET_BYTES = 128 * 1024
MIN_USEFUL_AGE_SECONDS = 60
MAX_USEFUL_AGE_SECONDS = 600
DEFAULT_USEFUL_AGE_SECONDS = 300
DEFAULT_REFRESH_INTERVAL_SECONDS = 240
MAX_CLOCK_SKEW_SECONDS = 30
# Above MAX_FLEET_BYTES on purpose: remote_fleet reads the status file through git show, so a
# subprocess ceiling at or below the fleet ceiling would refuse a fleet that validate_fleet accepts
# and publishes, wedging every node's next publish with no way back but a force-push.
MAX_GIT_OUTPUT_BYTES = 256 * 1024
OID_RE = re.compile(r"[0-9a-f]{40}\Z")
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
NODE_RE = re.compile(r"node-[0-9a-f]{16}\Z")
KEY_RE = re.compile(r"key-[0-9a-f]{16}\Z")
REQUEST_RE = re.compile(r"req-[0-9a-f]{16,64}\Z")
REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}/[A-Za-z0-9_.-]{1,100}\Z")
PROFILE_RE = re.compile(r"[a-z0-9][a-z0-9_.-]{0,62}/v[1-9][0-9]{0,5}\Z")
VERIFICATION_PROFILE_RE = re.compile(r"[a-z0-9][a-z0-9_.-]{0,79}(?:/v[1-9][0-9]{0,5})?\Z")
SSH_PUBLIC_KEY_RE = re.compile(r"ssh-ed25519 [A-Za-z0-9+/=]{40,200}(?: [^\r\n]{0,80})?\Z")
HEAT_CLASSES = {"cold", "warm", "resident_hot", "revalidation_required"}
BUILD_STATE_CLASSES = {"cold", "warm", "resident_generation", "revalidation_required", "unknown"}
REQUEST_STATES = {"queued", "preparing", "running", "terminal", "superseded"}
DURATION_CLASSES = {"under_10s", "10s_to_1m", "1m_to_5m", "over_5m", "unknown"}
AVAILABILITY_CLASSES = {"available", "held", "draining", "unknown"}
PRESSURE_CLASSES = {"low", "high", "unknown"}
CAPACITY_CLASSES = {"available", "reserved", "insufficient", "unknown"}
REUSABLE_STATE_CLASSES = {
    "package_manager_state",
    "compiler_cache",
    "incremental_build_state",
    "immutable_compiled_product",
    "container_layer",
    "prepared_dependency_generation",
    "project_local_approved_hot_state",
}
REUSABLE_STATE_HEAT_CLASSES = {"cold", "warm", "hot"}
REUSABLE_STATE_SIZE_CLASSES = {"empty", "tiny", "small", "medium", "large", "huge"}
REUSABLE_STATE_RECENT_HIT_CLASSES = {
    "never",
    "within_hour",
    "within_day",
    "within_week",
    "older",
    "clock_skew",
}


class SnapshotError(RuntimeError):
    """A bounded resident-snapshot refusal."""


class PublicationSuppressed(SnapshotError):
    """The snapshot is valid but no GitHub write is useful yet."""


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def digest(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def exact_object(value: object, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise SnapshotError(f"{label} must be an object")
    return value


def exact_list(value: object, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise SnapshotError(f"{label} must be an array")
    return value


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise SnapshotError(f"{label} has unsupported fields")


def integer(value: object, label: str, minimum: int, maximum: int) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or not minimum <= value <= maximum:
        raise SnapshotError(f"{label} is invalid")
    return value


def bounded_string(value: object, pattern: re.Pattern[str], label: str) -> str:
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise SnapshotError(f"{label} is invalid")
    return value


def parse_time(value: object, label: str) -> dt.datetime:
    if not isinstance(value, str) or len(value) > 32 or not value.endswith("Z"):
        raise SnapshotError(f"{label} is invalid")
    try:
        parsed = dt.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise SnapshotError(f"{label} is invalid") from error
    if parsed.utcoffset() != dt.timedelta(0):
        raise SnapshotError(f"{label} must be UTC")
    return parsed


def format_time(value: dt.datetime) -> str:
    if value.tzinfo is None or value.utcoffset() != dt.timedelta(0):
        raise SnapshotError("time must be UTC")
    return value.isoformat(timespec="milliseconds").replace("+00:00", "Z")


@contextlib.contextmanager
def scratch_directory(prefix: str):
    """A private temporary directory whose creation failure refuses with a constant, never a path."""
    try:
        directory = tempfile.TemporaryDirectory(prefix=prefix)
    except OSError as error:
        raise SnapshotError("a bounded scratch directory is unavailable") from error
    try:
        yield directory.name
    finally:
        directory.cleanup()


def write_scratch(path: Path, payload: bytes) -> None:
    """Write scratch bytes, refusing with a constant so ENOSPC/EACCES never stringifies the path."""
    try:
        path.write_bytes(payload)
    except OSError as error:
        raise SnapshotError("a bounded scratch file could not be written") from error


def load_json(path: Path, maximum_bytes: int = MAX_FLEET_BYTES) -> object:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise SnapshotError("bounded JSON input is unavailable") from error
    if len(raw) > maximum_bytes:
        raise SnapshotError("bounded JSON input is oversized")
    try:
        return json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SnapshotError("bounded JSON input is invalid") from error


def validate_trust(value: object) -> dict[str, Any]:
    trust = exact_object(value, "trust document")
    if len(canonical_json(trust)) > MAX_FLEET_BYTES:
        raise SnapshotError("trust document exceeds the byte budget")
    exact_keys(trust, {"document_type", "schema_version", "repositories", "nodes"}, "trust document")
    if trust["document_type"] != TRUST_DOCUMENT or trust["schema_version"] != SCHEMA_VERSION:
        raise SnapshotError("trust document version is unsupported")
    repositories = exact_list(trust["repositories"], "trusted repositories")
    if not repositories or any(not isinstance(item, str) or REPOSITORY_RE.fullmatch(item) is None for item in repositories):
        raise SnapshotError("trusted repositories are invalid")
    if len(set(repositories)) != len(repositories):
        raise SnapshotError("trusted repositories contain duplicates")
    nodes = exact_list(trust["nodes"], "trusted nodes")
    seen_nodes: set[str] = set()
    seen_keys: set[str] = set()
    for entry_raw in nodes:
        entry = exact_object(entry_raw, "trusted node")
        exact_keys(entry, {"id", "key_id", "ssh_public_key", "os_class", "architecture_class"}, "trusted node")
        node_id = bounded_string(entry["id"], NODE_RE, "trusted node id")
        key_id = bounded_string(entry["key_id"], KEY_RE, "trusted key id")
        if node_id in seen_nodes or key_id in seen_keys:
            raise SnapshotError("trusted node identities must be unique")
        seen_nodes.add(node_id)
        seen_keys.add(key_id)
        if not isinstance(entry["ssh_public_key"], str) or SSH_PUBLIC_KEY_RE.fullmatch(entry["ssh_public_key"]) is None:
            raise SnapshotError("trusted SSH public key is invalid")
        if entry["os_class"] not in {"linux", "macos"}:
            raise SnapshotError("trusted OS class is invalid")
        if entry["architecture_class"] not in {"x86_64", "arm64"}:
            raise SnapshotError("trusted architecture class is invalid")
    return trust


def trust_node(trust: dict[str, Any], node_id: str) -> dict[str, Any]:
    for entry in trust["nodes"]:
        if entry["id"] == node_id:
            return entry
    raise SnapshotError("node is absent from the reviewed trust set")


def admission_projection(value: object) -> dict[str, object]:
    admission = exact_object(value, "admission observation")
    exact_keys(
        admission,
        {"document_type", "schema_version", "outcome", "reason", "grants_authority", "authorizes_execution", "authorizes_redispatch"},
        "admission observation",
    )
    if admission["document_type"] != "glaeda-owned-admission-observation" or admission["schema_version"] != 1:
        raise SnapshotError("admission observation version is unsupported")
    if admission["grants_authority"] is not False or admission["authorizes_execution"] is not False or admission["authorizes_redispatch"] is not False:
        raise SnapshotError("admission observation carries authority")
    pair = (admission["outcome"], admission["reason"])
    mapping: dict[tuple[object, object], tuple[str, str, str, int]] = {
        ("ready", "compatible"): ("available", "low", "available", 0),
        ("wait", "reserved"): ("available", "unknown", "reserved", 1),
        ("wait", "node_draining"): ("draining", "unknown", "unknown", 0),
        ("wait", "pressure_high"): ("available", "high", "insufficient", 0),
        ("wait", "capacity_unavailable"): ("available", "unknown", "insufficient", 0),
        ("wait", "node_held"): ("held", "unknown", "unknown", 0),
        ("refused", "observation_unavailable"): ("unknown", "unknown", "unknown", 0),
    }
    if pair not in mapping:
        raise SnapshotError("admission observation disposition is unsupported")
    availability, pressure, capacity, active = mapping[pair]
    return {
        "availability_class": availability,
        "pressure_class": pressure,
        "capacity_class": capacity,
        "active_work_count": active,
    }


def capability_projection(
    value: object,
    *,
    trust: dict[str, Any],
    public_node_id: str,
    observed_at: dt.datetime,
    published_at: dt.datetime,
) -> tuple[dict[str, object], list[dict[str, object]], list[dict[str, object]], str, dt.datetime]:
    capability = exact_object(value, "owned workstation capability")
    if capability.get("schema") != "glaeda-owned-workstation-capability/v1":
        raise SnapshotError("owned workstation capability version is unsupported")
    if capability.get("advisoryOnly") is not True or capability.get("authorizesDispatch") is not False or capability.get("authorizesExecution") is not False:
        raise SnapshotError("owned workstation capability carries unexpected authority")
    cap_observed = parse_time(capability.get("observedAt"), "capability observedAt")
    cap_expires = parse_time(capability.get("expiresAt"), "capability expiresAt")
    if cap_observed > observed_at + dt.timedelta(seconds=MAX_CLOCK_SKEW_SECONDS) or cap_expires <= published_at:
        raise SnapshotError("owned workstation capability is stale")

    node = exact_object(capability.get("node"), "capability node")
    if node.get("osClass") not in {"linux", "macos"} or node.get("architectureClass") not in {"x86_64", "arm64"}:
        raise SnapshotError("capability node class is invalid")
    integer(node.get("generation"), "capability node generation", 1, 2**31 - 1)
    reviewed = trust_node(trust, public_node_id)
    if node["osClass"] != reviewed["os_class"] or node["architectureClass"] != reviewed["architecture_class"]:
        raise SnapshotError("capability node class disagrees with reviewed trust")
    public_node = {
        "id": public_node_id,
        "os_class": node["osClass"],
        "architecture_class": node["architectureClass"],
        "glaeda_node_generation": node["generation"],
    }

    profiles_raw = exact_list(capability.get("profiles"), "capability profiles")
    profiles: list[dict[str, object]] = []
    profile_ids: set[str] = set()
    for item_raw in profiles_raw:
        item = exact_object(item_raw, "capability profile")
        profile_id = bounded_string(item.get("id"), PROFILE_RE, "profile id")
        profile_class = item.get("class")
        generation = item.get("versionSha256")
        if not isinstance(profile_class, str) or len(profile_class) > 48 or not re.fullmatch(r"[a-z0-9_]+", profile_class):
            raise SnapshotError("profile class is invalid")
        bounded_string(generation, SHA256_RE, "profile generation")
        if profile_id in profile_ids:
            raise SnapshotError("profile identities must be unique")
        profile_ids.add(profile_id)
        profiles.append({"id": profile_id, "class": profile_class, "generation": generation})

    repositories = set(trust["repositories"])
    projects_raw = exact_list(capability.get("projects"), "capability projects")
    projects: list[dict[str, object]] = []
    for item_raw in projects_raw:
        item = exact_object(item_raw, "capability project")
        repository = item.get("repository")
        if repository not in repositories:
            raise SnapshotError("project is absent from the reviewed repository allowlist")
        source = exact_object(item.get("source"), "project source")
        commit = bounded_string(source.get("commitOid"), OID_RE, "project commit")
        tree = bounded_string(source.get("treeOid"), OID_RE, "project tree")
        heat = item.get("heatClass")
        heat_map = {"resident_hot": "resident_hot", "resident_cold": "cold"}
        if heat not in heat_map:
            raise SnapshotError("project heat class is unsupported")
        verification_profiles = exact_list(item.get("verificationProfiles"), "project verification profiles")
        if any(not isinstance(profile, str) or len(profile) > 80 for profile in verification_profiles):
            raise SnapshotError("project verification profiles are invalid")
        projects.append({
            "repository": repository,
            "source": {"commit_oid": commit, "tree_oid": tree},
            "heat_class": heat_map[heat],
            "verification_profiles": sorted(set(verification_profiles)),
            "dependency_build_state_class": "resident_generation" if heat == "resident_hot" else "cold",
            "reusable_states": [],
            "active_task_count": 0,
            "recent_compatible_receipt_ref": None,
        })

    producer = exact_object(capability.get("producer"), "capability producer")
    glaeda_generation = bounded_string(producer.get("glaedaRuntimeSha256"), SHA256_RE, "Glaeda generation")
    return public_node, profiles, projects, glaeda_generation, cap_observed


def validate_reusable_states(value: object) -> list[dict[str, object]]:
    summaries: list[dict[str, object]] = []
    seen: set[tuple[str, str]] = set()
    for raw in exact_list(value, "project reusable states"):
        item = exact_object(raw, "project reusable state")
        exact_keys(
            item,
            {
                "schema_version",
                "cache_class",
                "generation",
                "heat",
                "size",
                "recent_hit",
                "revalidation_required",
            },
            "project reusable state",
        )
        if item["schema_version"] != 1:
            raise SnapshotError("reusable-state summary version is unsupported")
        cache_class = item["cache_class"]
        generation = bounded_string(item["generation"], SHA256_RE, "reusable-state generation")
        if cache_class not in REUSABLE_STATE_CLASSES:
            raise SnapshotError("reusable-state class is invalid")
        if item["heat"] not in REUSABLE_STATE_HEAT_CLASSES:
            raise SnapshotError("reusable-state heat class is invalid")
        if item["size"] not in REUSABLE_STATE_SIZE_CLASSES:
            raise SnapshotError("reusable-state size class is invalid")
        if item["recent_hit"] not in REUSABLE_STATE_RECENT_HIT_CLASSES:
            raise SnapshotError("reusable-state recent-hit class is invalid")
        if not isinstance(item["revalidation_required"], bool):
            raise SnapshotError("reusable-state revalidation flag is invalid")
        if item["revalidation_required"] and item["heat"] != "cold":
            raise SnapshotError("reusable-state revalidation requires cold heat")
        identity = (cache_class, generation)
        if identity in seen:
            raise SnapshotError("project reusable-state identities must be unique")
        seen.add(identity)
        summaries.append(
            {
                "schema_version": 1,
                "cache_class": cache_class,
                "generation": generation,
                "heat": item["heat"],
                "size": item["size"],
                "recent_hit": item["recent_hit"],
                "revalidation_required": item["revalidation_required"],
            }
        )
    return sorted(summaries, key=lambda item: (str(item["cache_class"]), str(item["generation"])))


def validate_project_state(value: object, trust: dict[str, Any], profile_ids: set[str]) -> list[dict[str, object]]:
    doc = exact_object(value, "project state input")
    exact_keys(doc, {"document_type", "schema_version", "projects"}, "project state input")
    if doc["document_type"] != "glaeda-github-project-heat-input" or doc["schema_version"] != 1:
        raise SnapshotError("project state input version is unsupported")
    repositories = set(trust["repositories"])
    projects: list[dict[str, object]] = []
    seen: set[str] = set()
    for raw in exact_list(doc["projects"], "project state"):
        item = exact_object(raw, "project state")
        exact_keys(
            item,
            {"repository", "source", "heat_class", "verification_profiles", "dependency_build_state_class", "reusable_states", "active_task_count", "recent_compatible_receipt_ref"},
            "project state",
        )
        repository = item["repository"]
        if repository not in repositories or repository in seen:
            raise SnapshotError("project state repository is invalid")
        seen.add(repository)
        source = exact_object(item["source"], "project state source")
        exact_keys(source, {"commit_oid", "tree_oid"}, "project state source")
        commit = bounded_string(source["commit_oid"], OID_RE, "project state commit")
        tree = bounded_string(source["tree_oid"], OID_RE, "project state tree")
        if item["heat_class"] not in HEAT_CLASSES or item["dependency_build_state_class"] not in BUILD_STATE_CLASSES:
            raise SnapshotError("project state class is invalid")
        active = integer(item["active_task_count"], "project active task count", 0, 32)
        verification_profiles = exact_list(item["verification_profiles"], "project verification profiles")
        if any(not isinstance(profile, str) or VERIFICATION_PROFILE_RE.fullmatch(profile) is None for profile in verification_profiles):
            raise SnapshotError("project verification profile is invalid")
        receipt = item["recent_compatible_receipt_ref"]
        if receipt is not None:
            bounded_string(receipt, SHA256_RE, "recent receipt reference")
        projects.append({
            "repository": repository,
            "source": {"commit_oid": commit, "tree_oid": tree},
            "heat_class": item["heat_class"],
            "verification_profiles": sorted(set(verification_profiles)),
            "dependency_build_state_class": item["dependency_build_state_class"],
            "reusable_states": validate_reusable_states(item["reusable_states"]),
            "active_task_count": active,
            "recent_compatible_receipt_ref": receipt,
        })
    return sorted(projects, key=lambda item: str(item["repository"]))


def validate_request_state(value: object, trust: dict[str, Any], profile_ids: set[str]) -> list[dict[str, object]]:
    doc = exact_object(value, "request state input")
    exact_keys(doc, {"document_type", "schema_version", "requests"}, "request state input")
    if doc["document_type"] != "glaeda-github-request-state-input" or doc["schema_version"] != 1:
        raise SnapshotError("request state input version is unsupported")
    repositories = set(trust["repositories"])
    requests: list[dict[str, object]] = []
    seen: set[str] = set()
    for raw in exact_list(doc["requests"], "request state"):
        item = exact_object(raw, "request state")
        exact_keys(
            item,
            {"request_id", "source", "profile", "state", "terminal_receipt_ref", "elapsed_class", "estimate_class"},
            "request state",
        )
        request_id = bounded_string(item["request_id"], REQUEST_RE, "request id")
        if request_id in seen:
            raise SnapshotError("request ids must be unique")
        seen.add(request_id)
        source = exact_object(item["source"], "request source")
        exact_keys(source, {"repository", "commit_oid", "tree_oid"}, "request source")
        if source["repository"] not in repositories:
            raise SnapshotError("request repository is absent from the reviewed allowlist")
        commit = bounded_string(source["commit_oid"], OID_RE, "request commit")
        tree = bounded_string(source["tree_oid"], OID_RE, "request tree")
        if item["profile"] not in profile_ids or item["state"] not in REQUEST_STATES:
            raise SnapshotError("request profile or state is invalid")
        if item["elapsed_class"] not in DURATION_CLASSES or item["estimate_class"] not in DURATION_CLASSES:
            raise SnapshotError("request duration class is invalid")
        receipt = item["terminal_receipt_ref"]
        if item["state"] == "terminal":
            bounded_string(receipt, SHA256_RE, "terminal receipt reference")
        elif receipt is not None:
            raise SnapshotError("non-terminal request cannot claim a terminal receipt")
        requests.append({
            "request_id": request_id,
            "source": {"repository": source["repository"], "commit_oid": commit, "tree_oid": tree},
            "profile": item["profile"],
            "state": item["state"],
            "terminal_receipt_ref": receipt,
            "elapsed_class": item["elapsed_class"],
            "estimate_class": item["estimate_class"],
        })
    return sorted(requests, key=lambda item: str(item["request_id"]))


def build_unsigned_snapshot(
    capability: object,
    admission: object,
    trust_value: object,
    *,
    public_node_id: str,
    producer_generation: int,
    snapshot_sequence: int,
    observed_at: dt.datetime,
    published_at: dt.datetime,
    maximum_useful_age_seconds: int = DEFAULT_USEFUL_AGE_SECONDS,
    project_state: object | None = None,
    request_state: object | None = None,
) -> dict[str, object]:
    trust = validate_trust(trust_value)
    public_node_id = bounded_string(public_node_id, NODE_RE, "public node id")
    reviewed = trust_node(trust, public_node_id)
    producer_generation = integer(producer_generation, "producer generation", 1, 2**31 - 1)
    snapshot_sequence = integer(snapshot_sequence, "snapshot sequence", 1, 2**63 - 1)
    maximum_useful_age_seconds = integer(
        maximum_useful_age_seconds, "maximum useful age", MIN_USEFUL_AGE_SECONDS, MAX_USEFUL_AGE_SECONDS
    )
    if published_at < observed_at:
        raise SnapshotError("published_at precedes observed_at")
    node, profiles, projects, glaeda_generation, capability_observed_at = capability_projection(
        capability,
        trust=trust,
        public_node_id=public_node_id,
        observed_at=observed_at,
        published_at=published_at,
    )
    effective_observed_at = min(observed_at, capability_observed_at)
    if published_at < effective_observed_at:
        raise SnapshotError("published_at precedes observed evidence")
    node.update(admission_projection(admission))
    profile_ids = {str(profile["id"]) for profile in profiles}
    if project_state is not None:
        projects = validate_project_state(project_state, trust, profile_ids)
    else:
        active = integer(node["active_work_count"], "node active work count", 0, 32)
        if len(projects) == 1:
            projects[0]["active_task_count"] = active
    requests = [] if request_state is None else validate_request_state(request_state, trust, profile_ids)
    active_work_count = integer(node["active_work_count"], "node active work count", 0, 32)
    active_request_count = sum(item["state"] in {"preparing", "running"} for item in requests)
    active_project_count = sum(int(item["active_task_count"]) for item in projects)
    if active_request_count > active_work_count or active_project_count > active_work_count:
        raise SnapshotError("published active work disagrees with local admission evidence")
    unsigned = {
        "document_type": NODE_DOCUMENT,
        "schema_version": SCHEMA_VERSION,
        "payload": {
            "authority": {
                "advisory_only": True,
                "authorizes_dispatch": False,
                "authorizes_execution": False,
                "authorizes_host_selection": False,
                "authorizes_cleanup": False,
            },
            "freshness": {
                "snapshot_sequence": snapshot_sequence,
                "observed_at": format_time(effective_observed_at),
                "published_at": format_time(published_at),
                "producer_generation": producer_generation,
                "maximum_useful_age_seconds": maximum_useful_age_seconds,
            },
            "producer": {
                "glaeda_generation": glaeda_generation,
                "key_id": reviewed["key_id"],
            },
            "node": node,
            "profiles": sorted(profiles, key=lambda item: str(item["id"])),
            "projects": sorted(projects, key=lambda item: str(item["repository"])),
            "requests": requests,
        },
    }
    validate_unsigned_snapshot(unsigned, trust, check_freshness=False)
    return unsigned


def validate_unsigned_snapshot(
    value: object,
    trust_value: object,
    *,
    now: dt.datetime | None = None,
    check_freshness: bool = True,
) -> dict[str, Any]:
    trust = validate_trust(trust_value)
    value = exact_object(value, "node snapshot")
    if len(canonical_json(value)) > MAX_NODE_BYTES:
        raise SnapshotError("node snapshot exceeds the byte budget")
    exact_keys(value, {"document_type", "schema_version", "payload"}, "node snapshot")
    if value["document_type"] != NODE_DOCUMENT or value["schema_version"] != SCHEMA_VERSION:
        raise SnapshotError("node snapshot version is unsupported")
    payload = exact_object(value["payload"], "node payload")
    exact_keys(payload, {"authority", "freshness", "producer", "node", "profiles", "projects", "requests"}, "node payload")
    authority = exact_object(payload["authority"], "snapshot authority")
    exact_keys(authority, {"advisory_only", "authorizes_dispatch", "authorizes_execution", "authorizes_host_selection", "authorizes_cleanup"}, "snapshot authority")
    if authority != {
        "advisory_only": True,
        "authorizes_dispatch": False,
        "authorizes_execution": False,
        "authorizes_host_selection": False,
        "authorizes_cleanup": False,
    }:
        raise SnapshotError("node snapshot carries authority")
    freshness = exact_object(payload["freshness"], "snapshot freshness")
    exact_keys(freshness, {"snapshot_sequence", "observed_at", "published_at", "producer_generation", "maximum_useful_age_seconds"}, "snapshot freshness")
    integer(freshness["snapshot_sequence"], "snapshot sequence", 1, 2**63 - 1)
    integer(freshness["producer_generation"], "producer generation", 1, 2**31 - 1)
    max_age = integer(freshness["maximum_useful_age_seconds"], "maximum useful age", MIN_USEFUL_AGE_SECONDS, MAX_USEFUL_AGE_SECONDS)
    observed = parse_time(freshness["observed_at"], "snapshot observed_at")
    published = parse_time(freshness["published_at"], "snapshot published_at")
    if published < observed:
        raise SnapshotError("snapshot publication time is invalid")
    if check_freshness:
        now = now or dt.datetime.now(dt.UTC)
        if now.tzinfo is None or now.utcoffset() != dt.timedelta(0):
            raise SnapshotError("freshness clock must be UTC")
        if observed > now + dt.timedelta(seconds=MAX_CLOCK_SKEW_SECONDS) or published > now + dt.timedelta(seconds=MAX_CLOCK_SKEW_SECONDS):
            raise SnapshotError("snapshot timestamp is in the future")
        if now - observed > dt.timedelta(seconds=max_age):
            raise SnapshotError("snapshot is stale")

    producer = exact_object(payload["producer"], "snapshot producer")
    exact_keys(producer, {"glaeda_generation", "key_id"}, "snapshot producer")
    bounded_string(producer["glaeda_generation"], SHA256_RE, "Glaeda generation")
    key_id = bounded_string(producer["key_id"], KEY_RE, "producer key id")

    node = exact_object(payload["node"], "snapshot node")
    exact_keys(node, {"id", "os_class", "architecture_class", "glaeda_node_generation", "availability_class", "pressure_class", "capacity_class", "active_work_count"}, "snapshot node")
    node_id = bounded_string(node["id"], NODE_RE, "snapshot node id")
    reviewed = trust_node(trust, node_id)
    if key_id != reviewed["key_id"]:
        raise SnapshotError("snapshot key id disagrees with reviewed trust")
    if node["os_class"] != reviewed["os_class"] or node["architecture_class"] != reviewed["architecture_class"]:
        raise SnapshotError("snapshot node class disagrees with reviewed trust")
    integer(node["glaeda_node_generation"], "Glaeda node generation", 1, 2**31 - 1)
    if node["availability_class"] not in AVAILABILITY_CLASSES or node["pressure_class"] not in PRESSURE_CLASSES or node["capacity_class"] not in CAPACITY_CLASSES:
        raise SnapshotError("snapshot node state class is invalid")
    integer(node["active_work_count"], "active work count", 0, 32)

    profiles = exact_list(payload["profiles"], "snapshot profiles")
    profile_ids: set[str] = set()
    for raw in profiles:
        item = exact_object(raw, "snapshot profile")
        exact_keys(item, {"id", "class", "generation"}, "snapshot profile")
        profile_id = bounded_string(item["id"], PROFILE_RE, "snapshot profile id")
        if profile_id in profile_ids:
            raise SnapshotError("snapshot profile ids must be unique")
        profile_ids.add(profile_id)
        if not isinstance(item["class"], str) or len(item["class"]) > 48 or re.fullmatch(r"[a-z0-9_]+", item["class"]) is None:
            raise SnapshotError("snapshot profile class is invalid")
        bounded_string(item["generation"], SHA256_RE, "snapshot profile generation")

    repositories = set(trust["repositories"])
    projects = exact_list(payload["projects"], "snapshot projects")
    seen_projects: set[str] = set()
    for raw in projects:
        item = exact_object(raw, "snapshot project")
        exact_keys(item, {"repository", "source", "heat_class", "verification_profiles", "dependency_build_state_class", "reusable_states", "active_task_count", "recent_compatible_receipt_ref"}, "snapshot project")
        repository = item["repository"]
        if repository not in repositories or repository in seen_projects:
            raise SnapshotError("snapshot project repository is invalid")
        seen_projects.add(repository)
        source = exact_object(item["source"], "snapshot project source")
        exact_keys(source, {"commit_oid", "tree_oid"}, "snapshot project source")
        bounded_string(source["commit_oid"], OID_RE, "snapshot project commit")
        bounded_string(source["tree_oid"], OID_RE, "snapshot project tree")
        if item["heat_class"] not in HEAT_CLASSES or item["dependency_build_state_class"] not in BUILD_STATE_CLASSES:
            raise SnapshotError("snapshot project state class is invalid")
        validate_reusable_states(item["reusable_states"])
        verification_profiles = exact_list(item["verification_profiles"], "snapshot project profiles")
        if any(not isinstance(profile, str) or VERIFICATION_PROFILE_RE.fullmatch(profile) is None for profile in verification_profiles):
            raise SnapshotError("snapshot project verification profile is invalid")
        integer(item["active_task_count"], "snapshot project active task count", 0, 32)
        if item["recent_compatible_receipt_ref"] is not None:
            bounded_string(item["recent_compatible_receipt_ref"], SHA256_RE, "snapshot recent receipt")

    requests = exact_list(payload["requests"], "snapshot requests")
    seen_requests: set[str] = set()
    for raw in requests:
        item = exact_object(raw, "snapshot request")
        exact_keys(item, {"request_id", "source", "profile", "state", "terminal_receipt_ref", "elapsed_class", "estimate_class"}, "snapshot request")
        request_id = bounded_string(item["request_id"], REQUEST_RE, "snapshot request id")
        if request_id in seen_requests:
            raise SnapshotError("snapshot request ids must be unique")
        seen_requests.add(request_id)
        source = exact_object(item["source"], "snapshot request source")
        exact_keys(source, {"repository", "commit_oid", "tree_oid"}, "snapshot request source")
        if source["repository"] not in repositories:
            raise SnapshotError("snapshot request repository is invalid")
        bounded_string(source["commit_oid"], OID_RE, "snapshot request commit")
        bounded_string(source["tree_oid"], OID_RE, "snapshot request tree")
        if item["profile"] not in profile_ids or item["state"] not in REQUEST_STATES:
            raise SnapshotError("snapshot request state is invalid")
        if item["elapsed_class"] not in DURATION_CLASSES or item["estimate_class"] not in DURATION_CLASSES:
            raise SnapshotError("snapshot request duration class is invalid")
        if item["state"] == "terminal":
            bounded_string(item["terminal_receipt_ref"], SHA256_RE, "snapshot terminal receipt")
        elif item["terminal_receipt_ref"] is not None:
            raise SnapshotError("non-terminal snapshot request claims a terminal receipt")

    return value


def signing_environment() -> dict[str, str]:
    return {"LC_ALL": "C"}


def require_executable(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise SnapshotError(f"{label} must be an absolute path")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise SnapshotError(f"{label} is unavailable") from error
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise SnapshotError(f"{label} is unavailable")
    return resolved


def run_bounded(
    argv: list[str],
    *,
    cwd: Path | None = None,
    input_bytes: bytes | None = None,
    env: dict[str, str] | None = None,
    timeout: int = 10,
) -> subprocess.CompletedProcess[bytes]:
    try:
        options: dict[str, object] = {
            "cwd": cwd,
            "stdout": subprocess.PIPE,
            "stderr": subprocess.PIPE,
            "check": False,
            "timeout": timeout,
            "env": env or {"LC_ALL": "C"},
        }
        if input_bytes is None:
            options["stdin"] = subprocess.DEVNULL
        else:
            options["input"] = input_bytes
        completed = subprocess.run(argv, **options)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SnapshotError("bounded subprocess failed") from error
    if len(completed.stdout) > MAX_GIT_OUTPUT_BYTES or len(completed.stderr) > MAX_GIT_OUTPUT_BYTES:
        raise SnapshotError("bounded subprocess output exceeded its ceiling")
    return completed


def sign_snapshot(
    unsigned_value: object,
    trust_value: object,
    *,
    private_key: Path,
    ssh_keygen: Path = Path("/usr/bin/ssh-keygen"),
) -> dict[str, object]:
    trust = validate_trust(trust_value)
    unsigned = validate_unsigned_snapshot(unsigned_value, trust, check_freshness=False)
    node_id = unsigned["payload"]["node"]["id"]
    reviewed = trust_node(trust, node_id)
    key_id = reviewed["key_id"]
    ssh_keygen = require_executable(ssh_keygen, "ssh-keygen")
    try:
        private_key = private_key.resolve(strict=True)
        key_mode = stat.S_IMODE(private_key.stat().st_mode)
    except OSError as error:
        raise SnapshotError("snapshot signing key is unavailable") from error
    if not private_key.is_file():
        raise SnapshotError("snapshot signing key is unavailable")
    if key_mode & 0o077:
        raise SnapshotError("snapshot signing key permissions are too broad")
    public = run_bounded(
        [str(ssh_keygen), "-y", "-f", str(private_key)],
        env=signing_environment(),
        timeout=5,
    )
    if public.returncode != 0:
        raise SnapshotError("snapshot signing key could not be read")
    observed_public_fields = public.stdout.decode("ascii", errors="strict").strip().split()
    expected_public_fields = reviewed["ssh_public_key"].split()
    if len(observed_public_fields) < 2 or len(expected_public_fields) < 2:
        raise SnapshotError("snapshot signing public key is invalid")
    observed_public = " ".join(observed_public_fields[:2])
    expected_public = " ".join(expected_public_fields[:2])
    if observed_public != expected_public:
        raise SnapshotError("snapshot signing key disagrees with reviewed trust")

    message = canonical_json(unsigned)
    with scratch_directory("glaeda-status-sign-") as directory:
        message_path = Path(directory) / "snapshot"
        write_scratch(message_path, message)
        completed = run_bounded(
            [str(ssh_keygen), "-Y", "sign", "-f", str(private_key), "-n", SIGNING_NAMESPACE, str(message_path)],
            env=signing_environment(),
            timeout=5,
        )
        if completed.returncode != 0:
            raise SnapshotError("snapshot signature could not be created")
        signature_path = Path(str(message_path) + ".sig")
        try:
            signature = signature_path.read_text(encoding="ascii")
        except (OSError, UnicodeDecodeError) as error:
            raise SnapshotError("snapshot signature output is unavailable") from error
    if len(signature.encode("ascii")) > 4096 or "BEGIN SSH SIGNATURE" not in signature:
        raise SnapshotError("snapshot signature output is invalid")
    signed = {
        **unsigned,
        "signature": {
            "algorithm": "sshsig-ed25519",
            "key_id": key_id,
            "namespace": SIGNING_NAMESPACE,
            "value": signature,
        },
    }
    if len(canonical_json(signed)) > MAX_NODE_BYTES:
        raise SnapshotError("signed node snapshot exceeds the byte ceiling")
    return signed


def verify_snapshot_signature(
    value: object,
    trust_value: object,
    *,
    ssh_keygen: Path = Path("/usr/bin/ssh-keygen"),
) -> dict[str, Any]:
    trust = validate_trust(trust_value)
    signed = exact_object(value, "signed node snapshot")
    exact_keys(signed, {"document_type", "schema_version", "payload", "signature"}, "signed node snapshot")
    signature = exact_object(signed["signature"], "snapshot signature")
    exact_keys(signature, {"algorithm", "key_id", "namespace", "value"}, "snapshot signature")
    if signature["algorithm"] != "sshsig-ed25519" or signature["namespace"] != SIGNING_NAMESPACE:
        raise SnapshotError("snapshot signature algorithm is unsupported")
    key_id = bounded_string(signature["key_id"], KEY_RE, "snapshot signature key id")
    if not isinstance(signature["value"], str) or len(signature["value"].encode("utf-8")) > 4096:
        raise SnapshotError("snapshot signature is invalid")
    unsigned = {key: signed[key] for key in ("document_type", "schema_version", "payload")}
    validate_unsigned_snapshot(unsigned, trust, check_freshness=False)
    node_id = unsigned["payload"]["node"]["id"]
    reviewed = trust_node(trust, node_id)
    if key_id != reviewed["key_id"] or unsigned["payload"]["producer"]["key_id"] != key_id:
        raise SnapshotError("snapshot signature key disagrees with reviewed trust")
    ssh_keygen = require_executable(ssh_keygen, "ssh-keygen")
    with scratch_directory("glaeda-status-verify-") as directory:
        signature_path = Path(directory) / "snapshot.sig"
        allowed_path = Path(directory) / "allowed_signers"
        write_scratch(signature_path, signature["value"].encode("ascii"))
        write_scratch(allowed_path, f"{key_id} {reviewed['ssh_public_key']}\n".encode("ascii"))
        completed = run_bounded(
            [str(ssh_keygen), "-Y", "verify", "-f", str(allowed_path), "-I", key_id, "-n", SIGNING_NAMESPACE, "-s", str(signature_path)],
            input_bytes=canonical_json(unsigned),
            env=signing_environment(),
            timeout=5,
        )
        if completed.returncode != 0:
            raise SnapshotError("snapshot signature verification failed")
    return signed


def validate_signed_snapshot(
    value: object,
    trust_value: object,
    *,
    now: dt.datetime | None = None,
    check_freshness: bool = True,
    ssh_keygen: Path = Path("/usr/bin/ssh-keygen"),
) -> dict[str, Any]:
    signed = verify_snapshot_signature(value, trust_value, ssh_keygen=ssh_keygen)
    unsigned = {key: signed[key] for key in ("document_type", "schema_version", "payload")}
    validate_unsigned_snapshot(unsigned, trust_value, now=now, check_freshness=check_freshness)
    return signed


def raw_snapshot_node_id(value: object) -> str | None:
    if not isinstance(value, dict):
        return None
    payload = value.get("payload")
    if not isinstance(payload, dict):
        return None
    node = payload.get("node")
    if not isinstance(node, dict):
        return None
    node_id = node.get("id")
    if not isinstance(node_id, str) or NODE_RE.fullmatch(node_id) is None:
        return None
    return node_id


def raw_snapshot_key_id(value: object) -> str | None:
    if not isinstance(value, dict):
        return None
    payload = value.get("payload")
    signature = value.get("signature")
    if not isinstance(payload, dict) or not isinstance(signature, dict):
        return None
    producer = payload.get("producer")
    if not isinstance(producer, dict):
        return None
    signature_key = signature.get("key_id")
    producer_key = producer.get("key_id")
    if (
        not isinstance(signature_key, str)
        or not isinstance(producer_key, str)
        or signature_key != producer_key
        or KEY_RE.fullmatch(signature_key) is None
    ):
        return None
    return signature_key


def validate_fleet(value: object) -> dict[str, Any]:
    fleet = exact_object(value, "fleet snapshot")
    if len(canonical_json(fleet)) > MAX_FLEET_BYTES:
        raise SnapshotError("fleet snapshot exceeds the byte budget")
    exact_keys(fleet, {"document_type", "schema_version", "nodes"}, "fleet snapshot")
    if fleet["document_type"] != FLEET_DOCUMENT or fleet["schema_version"] != SCHEMA_VERSION:
        raise SnapshotError("fleet snapshot version is unsupported")
    exact_list(fleet["nodes"], "fleet node snapshots")
    return fleet


def empty_fleet() -> dict[str, object]:
    return {"document_type": FLEET_DOCUMENT, "schema_version": SCHEMA_VERSION, "nodes": []}


def snapshot_version(snapshot: dict[str, Any]) -> tuple[int, int]:
    freshness = snapshot["payload"]["freshness"]
    return int(freshness["producer_generation"]), int(freshness["snapshot_sequence"])


def snapshot_semantics(snapshot: dict[str, Any]) -> bytes:
    payload = dict(snapshot["payload"])
    payload.pop("freshness", None)
    return canonical_json(payload)


def upsert_fleet(
    fleet_value: object,
    candidate_value: object,
    trust_value: object,
    *,
    now: dt.datetime,
    refresh_interval_seconds: int = DEFAULT_REFRESH_INTERVAL_SECONDS,
    ssh_keygen: Path = Path("/usr/bin/ssh-keygen"),
) -> tuple[dict[str, object], str]:
    refresh_interval_seconds = integer(
        refresh_interval_seconds, "refresh interval", 30, MAX_USEFUL_AGE_SECONDS
    )
    trust = validate_trust(trust_value)
    fleet = validate_fleet(fleet_value)
    candidate = validate_signed_snapshot(
        candidate_value, trust, now=now, ssh_keygen=ssh_keygen
    )
    candidate_max_age = int(
        candidate["payload"]["freshness"]["maximum_useful_age_seconds"]
    )
    if refresh_interval_seconds > candidate_max_age - MAX_CLOCK_SKEW_SECONDS:
        raise SnapshotError("refresh interval leaves no bounded stale margin")
    node_id = candidate["payload"]["node"]["id"]

    reviewed_by_id = {entry["id"]: entry for entry in trust["nodes"]}
    original_nodes = list(fleet["nodes"])
    original_ids = [raw_snapshot_node_id(item) for item in original_nodes]
    if any(raw_id is None for raw_id in original_ids):
        raise SnapshotError("fleet contains a malformed node entry")

    retained = [
        (item, raw_id)
        for item, raw_id in zip(original_nodes, original_ids)
        if raw_id in reviewed_by_id
    ]
    fleet_pruned = len(retained) != len(original_nodes)
    retained_ids = [raw_id for _, raw_id in retained]
    if len(set(retained_ids)) != len(retained_ids):
        raise SnapshotError("fleet contains duplicate reviewed node entries")

    nodes: list[dict[str, Any]] = []
    trust_state_by_id: dict[str, str] = {}
    for item, raw_id in retained:
        reviewed = reviewed_by_id[raw_id]
        try:
            current = validate_signed_snapshot(
                item, trust, check_freshness=False, ssh_keygen=ssh_keygen
            )
        except SnapshotError:
            historical_key_id = raw_snapshot_key_id(item)
            if (
                historical_key_id is None
                or historical_key_id == reviewed["key_id"]
            ):
                raise
            # The entry cannot be verified under the reviewed key, so it is evidence of nothing and
            # is about to be recommitted under this publisher's authorship. Bound and shape-check
            # it so a junk blob cannot ride the rotation branch into the status branch and push the
            # fleet past the ceiling remote_fleet can read. An honestly published entry passed both
            # of these when it was signed, so only injected bytes can fail here.
            superseded = exact_object(item, "superseded node snapshot")
            exact_keys(
                superseded,
                {"document_type", "schema_version", "payload", "signature"},
                "superseded node snapshot",
            )
            if len(canonical_json(superseded)) > MAX_NODE_BYTES:
                raise SnapshotError("superseded node snapshot exceeds the byte ceiling")
            trust_state_by_id[raw_id] = "superseded_key"
            nodes.append(superseded)
        else:
            trust_state_by_id[raw_id] = "current_key"
            nodes.append(current)

    raw_ids = [raw_snapshot_node_id(item) for item in nodes]
    matches = [index for index, raw_id in enumerate(raw_ids) if raw_id == node_id]
    if len(matches) > 1:
        raise SnapshotError("fleet contains duplicate entries for the node")
    if not matches:
        nodes.append(candidate)
        result = {
            "document_type": FLEET_DOCUMENT,
            "schema_version": SCHEMA_VERSION,
            "nodes": sorted(nodes, key=lambda item: item["payload"]["node"]["id"]),
        }
        validate_fleet(result)
        return result, "transition"

    index = matches[0]
    candidate_version = snapshot_version(candidate)
    if trust_state_by_id[node_id] == "superseded_key":
        if candidate_version[1] != 1:
            raise SnapshotError(
                "first snapshot under a new reviewed key must restart snapshot sequence at one"
            )
        nodes[index] = candidate
        result = {
            "document_type": FLEET_DOCUMENT,
            "schema_version": SCHEMA_VERSION,
            "nodes": sorted(nodes, key=lambda item: item["payload"]["node"]["id"]),
        }
        validate_fleet(result)
        return result, "transition"

    current = nodes[index]
    current_version = snapshot_version(current)
    if candidate_version == current_version:
        if canonical_json(candidate) == canonical_json(current):
            raise PublicationSuppressed("snapshot is already published")
        raise SnapshotError("same snapshot version carries different content")
    if candidate_version < current_version:
        raise SnapshotError("older producer or snapshot sequence cannot overwrite newer state")
    if (
        candidate_version[0] == current_version[0]
        and candidate_version[1] != current_version[1] + 1
    ):
        raise SnapshotError(
            "snapshot sequence must advance by exactly one within a producer generation"
        )
    if candidate_version[0] > current_version[0] and candidate_version[1] != 1:
        raise SnapshotError("a new producer generation must restart snapshot sequence at one")

    current_published = parse_time(
        current["payload"]["freshness"]["published_at"], "current published_at"
    )
    if (
        not fleet_pruned
        and snapshot_semantics(candidate) == snapshot_semantics(current)
        and now - current_published < dt.timedelta(seconds=refresh_interval_seconds)
    ):
        raise PublicationSuppressed("unchanged snapshot is inside the refresh interval")
    nodes[index] = candidate
    result = {
        "document_type": FLEET_DOCUMENT,
        "schema_version": SCHEMA_VERSION,
        "nodes": sorted(nodes, key=lambda item: item["payload"]["node"]["id"]),
    }
    validate_fleet(result)
    reason = (
        "refresh"
        if not fleet_pruned
        and snapshot_semantics(candidate) == snapshot_semantics(current)
        else "transition"
    )
    return result, reason

def unknown_node(entry: dict[str, Any], reason: str) -> dict[str, object]:
    return {
        "id": entry["id"],
        "os_class": entry["os_class"],
        "architecture_class": entry["architecture_class"],
        "freshness_class": "unknown",
        "reason": reason,
        "availability_class": "unknown",
        "pressure_class": "unknown",
        "capacity_class": "unknown",
        "active_work_count": None,
        "glaeda_generation": None,
        "profiles": [],
        "projects": [],
        "requests": [],
    }


def bounded_reason(error: Exception) -> str:
    message = str(error)
    if "stale" in message:
        return "stale"
    if "signature" in message or "key" in message:
        return "untrusted"
    if "future" in message:
        return "future_timestamp"
    return "invalid"


def consume_fleet(
    fleet_value: object,
    trust_value: object,
    *,
    now: dt.datetime,
    ssh_keygen: Path = Path("/usr/bin/ssh-keygen"),
) -> dict[str, object]:
    trust = validate_trust(trust_value)
    try:
        fleet = validate_fleet(fleet_value)
        raw_nodes = fleet["nodes"]
    except SnapshotError:
        raw_nodes = []
    result_nodes: list[dict[str, object]] = []
    for trusted in trust["nodes"]:
        candidates = [item for item in raw_nodes if raw_snapshot_node_id(item) == trusted["id"]]
        if len(candidates) != 1:
            result_nodes.append(unknown_node(trusted, "missing" if len(candidates) == 0 else "duplicate"))
            continue
        try:
            snapshot = validate_signed_snapshot(candidates[0], trust, now=now, ssh_keygen=ssh_keygen)
        except SnapshotError as error:
            result_nodes.append(unknown_node(trusted, bounded_reason(error)))
            continue
        payload = snapshot["payload"]
        node = payload["node"]
        result_nodes.append({
            "id": node["id"],
            "os_class": node["os_class"],
            "architecture_class": node["architecture_class"],
            "freshness_class": "fresh",
            "reason": "verified",
            "availability_class": node["availability_class"],
            "pressure_class": node["pressure_class"],
            "capacity_class": node["capacity_class"],
            "active_work_count": node["active_work_count"],
            "glaeda_generation": payload["producer"]["glaeda_generation"],
            "profiles": payload["profiles"],
            "projects": payload["projects"],
            "requests": payload["requests"],
        })
    return {
        "document_type": VIEW_DOCUMENT,
        "schema_version": SCHEMA_VERSION,
        "observed_at": format_time(now),
        "authority": {"advisory_only": True, "authorizes_dispatch": False, "authorizes_execution": False},
        "nodes": result_nodes,
    }


def git_environment() -> dict[str, str]:
    env = {"LC_ALL": "C", "GIT_TERMINAL_PROMPT": "0", "PATH": "/usr/bin:/bin"}
    for key in ("HOME", "SSH_AUTH_SOCK"):
        value = os.environ.get(key)
        if value:
            env[key] = value
    return env


def git_call(
    git: Path,
    repository_root: Path,
    arguments: list[str],
    *,
    input_bytes: bytes | None = None,
    extra_env: dict[str, str] | None = None,
    timeout: int = 20,
) -> subprocess.CompletedProcess[bytes]:
    env = git_environment()
    if extra_env:
        env.update(extra_env)
    return run_bounded(
        [str(git), "-c", "core.hooksPath=/dev/null", *arguments],
        cwd=repository_root,
        input_bytes=input_bytes,
        env=env,
        timeout=timeout,
    )


def remote_fleet(
    git: Path,
    repository_root: Path,
    remote: str,
    branch: str,
) -> tuple[str | None, dict[str, object]]:
    fetch = git_call(git, repository_root, ["fetch", "--quiet", "--no-tags", remote, f"refs/heads/{branch}"])
    if fetch.returncode != 0:
        text = fetch.stderr.decode("utf-8", errors="replace")
        if "couldn't find remote ref" in text or "could not find remote ref" in text:
            return None, empty_fleet()
        raise SnapshotError("GitHub status fetch failed")
    head = git_call(git, repository_root, ["rev-parse", "FETCH_HEAD"])
    if head.returncode != 0:
        raise SnapshotError("GitHub status head is unavailable")
    sha = head.stdout.decode("ascii", errors="strict").strip()
    if OID_RE.fullmatch(sha) is None:
        raise SnapshotError("GitHub status head is invalid")
    content = git_call(git, repository_root, ["show", f"{sha}:{STATUS_FILE}"])
    if content.returncode != 0 or len(content.stdout) > MAX_FLEET_BYTES:
        raise SnapshotError("GitHub status file is unavailable")
    try:
        fleet = json.loads(content.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SnapshotError("GitHub status file is invalid") from error
    return sha, validate_fleet(fleet)


def create_status_commit(
    git: Path,
    repository_root: Path,
    fleet: dict[str, object],
    parent: str | None,
    node_id: str,
) -> str:
    fleet_bytes = canonical_json(fleet)
    blob = git_call(git, repository_root, ["hash-object", "-w", "--stdin"], input_bytes=fleet_bytes)
    if blob.returncode != 0:
        raise SnapshotError("Git object creation failed")
    blob_sha = blob.stdout.decode("ascii", errors="strict").strip()
    if OID_RE.fullmatch(blob_sha) is None:
        raise SnapshotError("Git blob identity is invalid")
    tree_line = f"100644 blob {blob_sha}\t{STATUS_FILE}\n".encode("ascii")
    tree = git_call(git, repository_root, ["mktree"], input_bytes=tree_line)
    if tree.returncode != 0:
        raise SnapshotError("Git tree creation failed")
    tree_sha = tree.stdout.decode("ascii", errors="strict").strip()
    if OID_RE.fullmatch(tree_sha) is None:
        raise SnapshotError("Git tree identity is invalid")
    args = ["commit-tree", tree_sha]
    if parent is not None:
        args.extend(["-p", parent])
    args.extend(["-m", f"Update advisory resident snapshot {node_id}"])
    commit = git_call(
        git,
        repository_root,
        args,
        extra_env={
            "GIT_AUTHOR_NAME": "Glaeda resident snapshot",
            "GIT_AUTHOR_EMAIL": "glaeda-resident-snapshot@invalid",
            "GIT_COMMITTER_NAME": "Glaeda resident snapshot",
            "GIT_COMMITTER_EMAIL": "glaeda-resident-snapshot@invalid",
        },
    )
    if commit.returncode != 0:
        raise SnapshotError("Git status commit creation failed")
    commit_sha = commit.stdout.decode("ascii", errors="strict").strip()
    if OID_RE.fullmatch(commit_sha) is None:
        raise SnapshotError("Git status commit identity is invalid")
    return commit_sha


def publish_snapshot(
    candidate_value: object,
    trust_value: object,
    *,
    repository_root: Path,
    remote: str = "origin",
    branch: str = STATUS_BRANCH,
    refresh_interval_seconds: int = DEFAULT_REFRESH_INTERVAL_SECONDS,
    now: dt.datetime | None = None,
    git: Path = Path("/usr/bin/git"),
    ssh_keygen: Path = Path("/usr/bin/ssh-keygen"),
    retries: int = 2,
) -> dict[str, object]:
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", remote):
        raise SnapshotError("Git remote name is invalid")
    if branch != STATUS_BRANCH:
        raise SnapshotError("GitHub status branch is fixed by the v1 contract")
    retries = integer(retries, "publish retries", 0, 3)
    now = now or dt.datetime.now(dt.UTC)
    git = require_executable(git, "git")
    try:
        repository_root = repository_root.resolve(strict=True)
    except OSError as error:
        raise SnapshotError("repository root is unavailable") from error
    candidate = validate_signed_snapshot(candidate_value, trust_value, now=now, ssh_keygen=ssh_keygen)
    node_id = candidate["payload"]["node"]["id"]
    candidate_digest = digest(canonical_json(candidate))
    for attempt in range(retries + 1):
        parent, fleet = remote_fleet(git, repository_root, remote, branch)
        try:
            updated, reason = upsert_fleet(
                fleet,
                candidate,
                trust_value,
                now=now,
                refresh_interval_seconds=refresh_interval_seconds,
                ssh_keygen=ssh_keygen,
            )
        except PublicationSuppressed as suppressed:
            return {
                "document_type": PUBLICATION_DOCUMENT,
                "schema_version": SCHEMA_VERSION,
                "state": "unchanged",
                "reason": "already_published" if "already" in str(suppressed) else "refresh_interval",
                "branch": branch,
                "snapshot_sha256": candidate_digest,
                "network_round_trips": 1,
            }
        commit_sha = create_status_commit(git, repository_root, updated, parent, node_id)
        push = git_call(git, repository_root, ["push", "--quiet", remote, f"{commit_sha}:refs/heads/{branch}"])
        if push.returncode == 0:
            return {
                "document_type": PUBLICATION_DOCUMENT,
                "schema_version": SCHEMA_VERSION,
                "state": "published",
                "reason": reason,
                "branch": branch,
                "snapshot_sha256": candidate_digest,
                "network_round_trips": 2 + attempt * 2,
            }
    raise SnapshotError("GitHub status publication lost a bounded compare-and-swap race")


def current_time(value: str | None) -> dt.datetime:
    return dt.datetime.now(dt.UTC) if value is None else parse_time(value, "requested time")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subparsers = root.add_subparsers(dest="command", required=True)

    compose = subparsers.add_parser("compose", help="compose and sign one advisory node snapshot")
    compose.add_argument("--capability", required=True, type=Path)
    compose.add_argument("--admission", required=True, type=Path)
    compose.add_argument("--trust", required=True, type=Path)
    compose.add_argument("--private-key", required=True, type=Path)
    compose.add_argument("--public-node-id", required=True)
    compose.add_argument("--producer-generation", required=True, type=int)
    compose.add_argument("--snapshot-sequence", required=True, type=int)
    compose.add_argument("--project-state", type=Path)
    compose.add_argument("--request-state", type=Path)
    compose.add_argument("--observed-at")
    compose.add_argument("--published-at")
    compose.add_argument("--maximum-useful-age-seconds", type=int, default=DEFAULT_USEFUL_AGE_SECONDS)
    compose.add_argument("--ssh-keygen", type=Path, default=Path("/usr/bin/ssh-keygen"))

    consume = subparsers.add_parser("consume", help="validate one fleet file into an agent-readable view")
    consume.add_argument("--fleet", required=True, type=Path)
    consume.add_argument("--trust", required=True, type=Path)
    consume.add_argument("--now")
    consume.add_argument("--ssh-keygen", type=Path, default=Path("/usr/bin/ssh-keygen"))

    publish = subparsers.add_parser("publish", help="compare-and-swap one signed snapshot into the GitHub status branch")
    publish.add_argument("--snapshot", required=True, type=Path)
    publish.add_argument("--trust", required=True, type=Path)
    publish.add_argument("--repository-root", required=True, type=Path)
    publish.add_argument("--remote", default="origin")
    publish.add_argument("--refresh-interval-seconds", type=int, default=DEFAULT_REFRESH_INTERVAL_SECONDS)
    publish.add_argument("--now")
    publish.add_argument("--git", type=Path, default=Path("/usr/bin/git"))
    publish.add_argument("--ssh-keygen", type=Path, default=Path("/usr/bin/ssh-keygen"))
    return root


def main(argv: list[str] | None = None) -> int:
    try:
        arguments = parser().parse_args(argv)
        if arguments.command == "compose":
            trust = load_json(arguments.trust, 32 * 1024)
            observed = current_time(arguments.observed_at)
            published = current_time(arguments.published_at) if arguments.published_at else observed
            unsigned = build_unsigned_snapshot(
                load_json(arguments.capability, MAX_NODE_BYTES),
                load_json(arguments.admission, MAX_NODE_BYTES),
                trust,
                public_node_id=arguments.public_node_id,
                producer_generation=arguments.producer_generation,
                snapshot_sequence=arguments.snapshot_sequence,
                observed_at=observed,
                published_at=published,
                maximum_useful_age_seconds=arguments.maximum_useful_age_seconds,
                project_state=load_json(arguments.project_state, MAX_NODE_BYTES) if arguments.project_state else None,
                request_state=load_json(arguments.request_state, MAX_NODE_BYTES) if arguments.request_state else None,
            )
            sys.stdout.buffer.write(canonical_json(sign_snapshot(unsigned, trust, private_key=arguments.private_key, ssh_keygen=arguments.ssh_keygen)))
            return 0
        if arguments.command == "consume":
            now = current_time(arguments.now)
            view = consume_fleet(
                load_json(arguments.fleet),
                load_json(arguments.trust, 32 * 1024),
                now=now,
                ssh_keygen=arguments.ssh_keygen,
            )
            sys.stdout.buffer.write(canonical_json(view))
            return 0
        if arguments.command == "publish":
            now = current_time(arguments.now)
            receipt = publish_snapshot(
                load_json(arguments.snapshot, MAX_NODE_BYTES),
                load_json(arguments.trust, 32 * 1024),
                repository_root=arguments.repository_root,
                remote=arguments.remote,
                refresh_interval_seconds=arguments.refresh_interval_seconds,
                now=now,
                git=arguments.git,
                ssh_keygen=arguments.ssh_keygen,
            )
            sys.stdout.buffer.write(canonical_json(receipt))
            return 0
        raise SnapshotError("unsupported command")
    except SnapshotError as error:
        sys.stderr.write(json.dumps({"error": str(error)}, sort_keys=True, separators=(",", ":")) + "\n")
        return 1
    except (OSError, subprocess.SubprocessError):
        # Only SnapshotError carries a reviewed constant message. An OSError or a SubprocessError
        # stringifies the path or the argv that raised it, so this prints a constant instead.
        sys.stderr.write(json.dumps({"error": "the command could not be completed"},
                                    sort_keys=True, separators=(",", ":")) + "\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
