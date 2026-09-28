from __future__ import annotations

import json
import math
import re
from pathlib import Path
from typing import Any, Iterable

from .model import FleetError, catalog, digest_json, load_json

WINDOW_SCHEMA_VERSION = 1
TOKEN_RE = re.compile(r"^[a-z0-9][a-z0-9._:-]{0,95}$")


def nearest_rank(values: Iterable[float], percentile: float) -> float | None:
    ordered = sorted(values)
    if not ordered:
        return None
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[rank - 1]


def concurrent_max(intervals: list[tuple[int, int]]) -> int:
    events: list[tuple[int, int]] = []
    for start, end in intervals:
        if end < start:
            raise FleetError("contention member has a reversed command interval")
        events.append((start, 1))
        events.append((end, -1))
    current = maximum = 0
    # End events sort before start events at the same timestamp, so back-to-back
    # commands do not count as simultaneous.
    for _, delta in sorted(events, key=lambda item: (item[0], item[1])):
        current += delta
        if current < 0:
            raise FleetError("contention intervals are internally inconsistent")
        maximum = max(maximum, current)
    return maximum


def _token(value: Any, field: str) -> str:
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise FleetError(f"{field} must be a bounded lowercase token")
    return value


def _positive_number(value: Any, field: str) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(float(value))
        or value <= 0
    ):
        raise FleetError(f"{field} must be finite and positive")
    return float(value)


def _positive_integer(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise FleetError(f"{field} must be a positive integer")
    return value


def _receipt_identity(receipt: dict[str, Any]) -> dict[str, Any]:
    if receipt.get("authority") != "performance_observation_only":
        raise FleetError("window member authority is unsupported")
    sample_id = receipt.get("sample_id")
    if not isinstance(sample_id, str) or not sample_id:
        raise FleetError("window member is missing sample_id")

    result = receipt.get("result")
    if not isinstance(result, dict) or type(result.get("validated")) is not bool:
        raise FleetError("window member validated result is invalid")
    result_class = result.get("class")
    if result_class not in {
        "validated",
        "command_failed",
        "timed_out",
        "semantic_mismatch",
        "source_changed",
    }:
        raise FleetError("window member result class is invalid")
    exit_code = result.get("exit_code")
    if type(exit_code) is not int:
        raise FleetError("window member exit code is invalid")
    failure_count = result.get("failure_count")
    if type(failure_count) is not int or failure_count not in {0, 1}:
        raise FleetError("window member failure count is invalid")
    if type(result.get("timed_out")) is not bool:
        raise FleetError("window member timeout evidence is invalid")
    if type(result.get("source_unchanged")) is not bool:
        raise FleetError("window member source-currentness evidence is invalid")
    semantic_validation = result.get("semantic_validation")
    if not isinstance(semantic_validation, str) or not semantic_validation:
        raise FleetError("window member semantic validation evidence is invalid")

    expected = {
        "validated": (True, 0, False),
        "command_failed": (False, 1, False),
        "timed_out": (False, 1, True),
        "semantic_mismatch": (False, 0, False),
        "source_changed": (False, 0, False),
    }[result_class]
    exit_consistent = (
        (result_class in {"validated", "semantic_mismatch", "source_changed"} and exit_code == 0)
        or (result_class in {"command_failed", "timed_out"} and exit_code != 0)
    )
    if (
        (result["validated"], failure_count, result["timed_out"]) != expected
        or not exit_consistent
        or (result_class == "source_changed" and result["source_unchanged"])
        or (
            result_class in {"validated", "semantic_mismatch"}
            and not result["source_unchanged"]
        )
    ):
        raise FleetError("window member result class is inconsistent")

    events = receipt.get("events")
    if not isinstance(events, dict):
        raise FleetError("window member event evidence is invalid")
    for name in ("fallback_count", "reset_count"):
        value = events.get(name)
        if type(value) is not int or value not in {0, 1}:
            raise FleetError(f"window member {name} is invalid")

    execution = receipt.get("execution") or {}
    runtime = execution.get("runtime") or {}
    storage = receipt.get("storage") or {}
    return {
        "machine_id": receipt.get("machine_id"),
        "machine_comparison_digest": receipt.get("machine_comparison_digest"),
        "backend_id": execution.get("backend_id"),
        "runtime_digest": runtime.get("digest"),
        "workload_id": receipt.get("workload", {}).get("id"),
        "variant": receipt.get("workload", {}).get("variant"),
        "source_commit": receipt.get("workload", {}).get("commit"),
        "source_tree": receipt.get("workload", {}).get("tree"),
        "operation_digest": receipt.get("workload", {}).get("operation_digest"),
        "toolchain_digest": receipt.get("toolchain", {}).get("digest"),
        "state_class": receipt.get("state", {}).get("class"),
        "state_preparation_id": receipt.get("state", {}).get("preparation_id"),
        "resource_policy_status": execution.get("resource_policy_status"),
        "resource_policy_evidence_id": execution.get("resource_policy_evidence_id"),
        "timeout_seconds": execution.get("timeout_seconds"),
        "source_storage_tier": storage.get("source_tier"),
        "source_storage_id": storage.get("source_id"),
        "state_storage_tier": storage.get("state_tier"),
        "state_storage_id": storage.get("state_id"),
        "source_filesystem": storage.get("source_filesystem"),
        "state_filesystem": storage.get("state_filesystem"),
    }


def _validate_member_receipt(
    receipt: dict[str, Any],
    manifest: dict[str, Any],
    *,
    expected_cpu_millis: int,
    expected_memory_bytes: int,
    expected_request_ns: int,
    arrival_tolerance_ns: int,
    window_start_ns: int,
    window_end_ns: int,
) -> None:
    if (
        receipt.get("schema_version") != 1
        or receipt.get("document_type") != "glaeda-owned-fleet-benchmark-receipt"
    ):
        raise FleetError("window member is not a supported fleet benchmark receipt")
    if receipt.get("machine_id") != manifest["machine_id"]:
        raise FleetError("window member machine differs from manifest")
    if receipt.get("workload", {}).get("id") != manifest["workload_id"]:
        raise FleetError("window member workload differs from manifest")
    if receipt.get("workload", {}).get("variant") != manifest.get("variant"):
        raise FleetError("window member variant differs from manifest")
    if receipt.get("state", {}).get("class") != manifest["state_class"]:
        raise FleetError("window member state class differs from manifest")
    sample_id = receipt.get("sample_id")
    if not isinstance(sample_id, str) or not sample_id:
        raise FleetError("window member is missing sample_id")

    execution = receipt.get("execution") or {}
    if execution.get("resource_policy_id") != manifest["resource_policy_id"]:
        raise FleetError("window member resource policy differs from manifest")
    if execution.get("resource_policy_status") != manifest["resource_policy_status"]:
        raise FleetError("window member resource policy status differs from manifest")
    if execution.get("resource_policy_evidence_id") != manifest.get(
        "resource_policy_evidence_id"
    ):
        raise FleetError("window member resource policy evidence differs from manifest")
    if execution.get("declared_cpu_millis") != expected_cpu_millis:
        raise FleetError("window member CPU limit does not match contention profile")
    if execution.get("declared_memory_limit_bytes") != expected_memory_bytes:
        raise FleetError("window member memory limit does not match contention profile")

    timing = receipt.get("timing") or {}
    keys = (
        "request_known_monotonic_ns",
        "command_start_monotonic_ns",
        "command_exit_monotonic_ns",
        "final_result_monotonic_ns",
    )
    if any(
        isinstance(timing.get(key), bool) or not isinstance(timing.get(key), int)
        for key in keys
    ):
        raise FleetError("window member is missing monotonic timing evidence")
    request_ns, command_start_ns, command_exit_ns, final_ns = (
        timing[key] for key in keys
    )
    if not (
        window_start_ns
        <= request_ns
        <= command_start_ns
        <= command_exit_ns
        <= final_ns
        <= window_end_ns
    ):
        raise FleetError("window member timing falls outside the fixed window")
    if abs(request_ns - expected_request_ns) > arrival_tolerance_ns:
        raise FleetError("window member request-known time violates the frozen arrival pattern")

    milestones = receipt.get("milestones") or {}
    latency = milestones.get("request_known_to_final_result_ms")
    if (
        isinstance(latency, bool)
        or not isinstance(latency, (int, float))
        or not math.isfinite(float(latency))
        or latency < 0
    ):
        raise FleetError("window member final-result latency is invalid")
    expected_latency = (final_ns - request_ns) / 1_000_000
    if abs(float(latency) - expected_latency) > 0.001:
        raise FleetError(
            "window member final-result latency disagrees with monotonic timing"
        )


def reduce_window(manifest: dict[str, Any], base_dir: Path) -> dict[str, Any]:
    if (
        manifest.get("schema_version") != 1
        or manifest.get("document_type") != "glaeda-owned-fleet-window-manifest"
    ):
        raise FleetError("unsupported window manifest")
    required = (
        "experiment_id",
        "machine_id",
        "workload_id",
        "state_class",
        "profile_id",
        "window_start_monotonic_ns",
        "window_elapsed_seconds",
        "arrival_pattern_id",
        "arrival_tolerance_ms",
        "resource_policy_id",
        "resource_policy_status",
        "aggregate_cpu_millis",
        "aggregate_memory_limit_bytes",
        "offered_work",
    )
    if any(field not in manifest for field in required):
        raise FleetError("window manifest is incomplete")

    _token(manifest["experiment_id"], "experiment_id")
    _token(manifest["machine_id"], "machine_id")
    _token(manifest["arrival_pattern_id"], "arrival_pattern_id")
    _token(manifest["resource_policy_id"], "resource_policy_id")
    resource_policy_status = manifest["resource_policy_status"]
    if resource_policy_status not in {"declared_only", "enforced"}:
        raise FleetError("resource_policy_status must be declared_only or enforced")
    policy_evidence_id = manifest.get("resource_policy_evidence_id")
    if resource_policy_status == "enforced":
        if policy_evidence_id is None:
            raise FleetError("enforced window requires resource_policy_evidence_id")
        _token(policy_evidence_id, "resource_policy_evidence_id")
    elif policy_evidence_id is not None:
        raise FleetError(
            "declared-only window cannot claim resource-policy enforcement evidence"
        )
    value_catalog = catalog()
    profile_id = manifest["profile_id"]
    if profile_id not in value_catalog["contention_profiles"]:
        raise FleetError("unknown contention profile")
    jobs = value_catalog["contention_profiles"][profile_id]["jobs"]

    start_ns = manifest["window_start_monotonic_ns"]
    if isinstance(start_ns, bool) or not isinstance(start_ns, int) or start_ns < 0:
        raise FleetError("window_start_monotonic_ns must be a non-negative integer")
    elapsed_seconds = _positive_number(
        manifest["window_elapsed_seconds"], "window_elapsed_seconds"
    )
    elapsed_ns = int(round(elapsed_seconds * 1_000_000_000))
    end_ns = start_ns + elapsed_ns
    arrival_tolerance_ms = _positive_number(
        manifest["arrival_tolerance_ms"], "arrival_tolerance_ms"
    )
    arrival_tolerance_ns = int(round(arrival_tolerance_ms * 1_000_000))

    aggregate_cpu = _positive_integer(
        manifest["aggregate_cpu_millis"], "aggregate_cpu_millis"
    )
    aggregate_memory = _positive_integer(
        manifest["aggregate_memory_limit_bytes"], "aggregate_memory_limit_bytes"
    )
    if aggregate_cpu % jobs or aggregate_memory % jobs:
        raise FleetError(
            "aggregate CPU and memory must divide exactly across the profile job count"
        )
    per_job_cpu = aggregate_cpu // jobs
    per_job_memory = aggregate_memory // jobs

    offered = manifest["offered_work"]
    if not isinstance(offered, list) or not offered:
        raise FleetError("window must offer at least one work item")
    work_ids: list[str] = []
    arrival_basis: list[dict[str, Any]] = []
    receipts: list[dict[str, Any]] = []
    sample_ids: set[str] = set()
    unfinished = 0

    for item in offered:
        if not isinstance(item, dict):
            raise FleetError("window offered_work entries must be objects")
        work_id = _token(item.get("work_id"), "work_id")
        if work_id in work_ids:
            raise FleetError("window work IDs must be unique bounded opaque tokens")
        work_ids.append(work_id)
        offset_ms = item.get("arrival_offset_ms")
        if (
            isinstance(offset_ms, bool)
            or not isinstance(offset_ms, (int, float))
            or not math.isfinite(float(offset_ms))
            or offset_ms < 0
            or offset_ms >= elapsed_seconds * 1000
        ):
            raise FleetError("arrival_offset_ms must fall inside the fixed window")
        arrival_basis.append(
            {"work_id": work_id, "arrival_offset_ms": float(offset_ms)}
        )

        receipt_ref = item.get("receipt")
        if receipt_ref is None:
            unfinished += 1
            continue
        path = Path(receipt_ref)
        if not path.is_absolute():
            path = base_dir / path
        receipt = load_json(path)
        expected_request_ns = start_ns + int(round(float(offset_ms) * 1_000_000))
        _validate_member_receipt(
            receipt,
            manifest,
            expected_cpu_millis=per_job_cpu,
            expected_memory_bytes=per_job_memory,
            expected_request_ns=expected_request_ns,
            arrival_tolerance_ns=arrival_tolerance_ns,
            window_start_ns=start_ns,
            window_end_ns=end_ns,
        )
        sample_id = receipt["sample_id"]
        if sample_id in sample_ids:
            raise FleetError("window cannot count the same sample receipt twice")
        sample_ids.add(sample_id)
        receipts.append(receipt)

    offered_digest = digest_json(
        sorted(arrival_basis, key=lambda item: item["work_id"])
    )
    arrival_pattern_digest = digest_json(
        {
            "arrival_pattern_id": manifest["arrival_pattern_id"],
            "arrival_tolerance_ms": arrival_tolerance_ms,
            "offered_work": sorted(arrival_basis, key=lambda item: item["work_id"]),
        }
    )

    if not receipts:
        return {
            "schema_version": WINDOW_SCHEMA_VERSION,
            "document_type": "glaeda-owned-fleet-window-partial-receipt",
            "authority": "declared_offer_only",
            "experiment_id": manifest["experiment_id"],
            "machine_id": manifest["machine_id"],
            "workload_id": manifest["workload_id"],
            "variant": manifest.get("variant"),
            "state_class": manifest["state_class"],
            "profile_id": profile_id,
            "profile": value_catalog["contention_profiles"][profile_id],
            "resource_policy_id": manifest["resource_policy_id"],
            "resource_policy_status": resource_policy_status,
            "resource_policy_evidence_id": policy_evidence_id,
            "evidence_class": "manifest_only",
            "aggregate_cpu_millis": aggregate_cpu,
            "aggregate_memory_limit_bytes": aggregate_memory,
            "per_job_cpu_millis": per_job_cpu,
            "per_job_memory_limit_bytes": per_job_memory,
            "arrival_pattern_id": manifest["arrival_pattern_id"],
            "arrival_pattern_digest": arrival_pattern_digest,
            "offered_work_digest": offered_digest,
            "window_start_monotonic_ns": start_ns,
            "window_elapsed_seconds": elapsed_seconds,
            "counts": {
                "offered": len(offered),
                "settled": 0,
                "validated_completions": 0,
                "unfinished": len(offered),
                "semantic_mismatch_count": 0,
                "unvalidated_completion_count": 0,
                "failure_count": 0,
                "unknown_result_count": 0,
                "fallback_count": 0,
                "reset_count": 0,
            },
            "validated_completions_per_second": 0.0,
            "final_result_latency_ms": {"p50": None, "p90": None},
            "concurrency": {
                "declared_jobs": jobs,
                "maximum_simultaneous_observed": 0,
                "underfilled": True,
            },
            "resources": {
                "max_member_peak_rss_kib": None,
                "swap_used_max_observed_bytes": None,
                "swap_growth_max_observed_bytes": None,
                "memory_psi_some_avg10_max_observed": None,
                "max_temperature_c_observed": None,
            },
        }

    identities = [_receipt_identity(receipt) for receipt in receipts]
    identity_digests = {digest_json(identity) for identity in identities}
    if len(identity_digests) != 1:
        raise FleetError(
            "window members differ on frozen machine/backend/source/toolchain/state/storage identity"
        )
    comparison_identity_digest = next(iter(identity_digests))
    comparison_identity = identities[0]
    allowed_none = {"variant"}
    if resource_policy_status == "declared_only":
        allowed_none.add("resource_policy_evidence_id")
    for key, value in comparison_identity.items():
        if value is None and key not in allowed_none:
            raise FleetError(f"window member comparison identity is missing {key}")

    validated = [
        receipt
        for receipt in receipts
        if receipt.get("result", {}).get("validated") is True
    ]
    latencies = [
        float(receipt["milestones"]["request_known_to_final_result_ms"])
        for receipt in validated
    ]
    intervals = [
        (
            int(receipt["timing"]["command_start_monotonic_ns"]),
            int(receipt["timing"]["command_exit_monotonic_ns"]),
        )
        for receipt in receipts
    ]
    maximum_simultaneous = concurrent_max(intervals)
    if maximum_simultaneous > jobs:
        raise FleetError(
            "observed command concurrency exceeds the declared contention profile"
        )

    def max_known(path: tuple[str, ...]) -> float | int | None:
        values = []
        for receipt in receipts:
            current: Any = receipt
            for key in path:
                current = current.get(key) if isinstance(current, dict) else None
            if (
                isinstance(current, (int, float))
                and not isinstance(current, bool)
                and math.isfinite(float(current))
            ):
                values.append(current)
        return max(values) if values else None

    swap_growth_values: list[float] = []
    for receipt in receipts:
        resources = receipt.get("resources") or {}
        start_swap = resources.get("swap_used_start_bytes")
        max_swap = resources.get("swap_used_max_observed_bytes")
        end_swap = resources.get("swap_used_end_bytes")
        if all(
            isinstance(value, (int, float))
            and not isinstance(value, bool)
            and math.isfinite(float(value))
            for value in (start_swap, max_swap, end_swap)
        ):
            swap_growth_values.append(
                max(
                    0.0,
                    float(max_swap) - float(start_swap),
                    float(end_swap) - float(start_swap),
                )
            )


    return {
        "schema_version": WINDOW_SCHEMA_VERSION,
        "document_type": "glaeda-owned-fleet-window-receipt",
        "authority": "performance_observation_only",
        "experiment_id": manifest["experiment_id"],
        "machine_id": manifest["machine_id"],
        "machine_comparison_digest": comparison_identity["machine_comparison_digest"],
        "backend_id": comparison_identity["backend_id"],
        "runtime_digest": comparison_identity["runtime_digest"],
        "workload_id": manifest["workload_id"],
        "variant": manifest.get("variant"),
        "state_class": manifest["state_class"],
        "profile_id": profile_id,
        "profile": value_catalog["contention_profiles"][profile_id],
        "resource_policy_id": manifest["resource_policy_id"],
        "resource_policy_status": resource_policy_status,
        "resource_policy_evidence_id": policy_evidence_id,
        "evidence_class": (
            "capacity_evidence"
            if resource_policy_status == "enforced"
            else "diagnostic_only"
        ),
        "aggregate_cpu_millis": aggregate_cpu,
        "aggregate_memory_limit_bytes": aggregate_memory,
        "per_job_cpu_millis": per_job_cpu,
        "per_job_memory_limit_bytes": per_job_memory,
        "arrival_pattern_id": manifest["arrival_pattern_id"],
        "arrival_pattern_digest": arrival_pattern_digest,
        "offered_work_digest": offered_digest,
        "comparison_identity_digest": comparison_identity_digest,
        "window_start_monotonic_ns": start_ns,
        "window_elapsed_seconds": elapsed_seconds,
        "counts": {
            "offered": len(offered),
            "settled": len(receipts),
            "validated_completions": len(validated),
            "unfinished": unfinished,
            "semantic_mismatch_count": sum(
                1
                for receipt in receipts
                if receipt.get("result", {}).get("class") == "semantic_mismatch"
            ),
            "unvalidated_completion_count": sum(
                1
                for receipt in receipts
                if receipt.get("result", {}).get("class") == "source_changed"
            ),
            "failure_count": sum(
                int(receipt.get("result", {}).get("failure_count", 0))
                for receipt in receipts
            ),
            "unknown_result_count": 0,
            "fallback_count": sum(
                int(receipt.get("events", {}).get("fallback_count", 0))
                for receipt in receipts
            ),
            "reset_count": sum(
                int(receipt.get("events", {}).get("reset_count", 0))
                for receipt in receipts
            ),
        },
        "validated_completions_per_second": (
            len(validated) / elapsed_seconds
        ),
        "final_result_latency_ms": {
            "p50": nearest_rank(latencies, 0.50),
            "p90": nearest_rank(latencies, 0.90),
        },
        "concurrency": {
            "declared_jobs": jobs,
            "maximum_simultaneous_observed": maximum_simultaneous,
            "underfilled": maximum_simultaneous < min(jobs, len(receipts)),
        },
        "resources": {
            "max_member_peak_rss_kib": max_known(
                ("resources", "peak_aggregate_rss_kib")
            ),
            "swap_used_max_observed_bytes": max_known(
                ("resources", "swap_used_max_observed_bytes")
            ),
            "swap_growth_max_observed_bytes": (
                max(swap_growth_values) if swap_growth_values else None
            ),
            "memory_psi_some_avg10_max_observed": max_known(
                ("resources", "memory_psi_some_avg10_max_observed")
            ),
            "max_temperature_c_observed": max_known(
                ("resources", "max_temperature_c_observed")
            ),
        },
    }


def compare_windows(windows: list[dict[str, Any]]) -> dict[str, Any]:
    if len(windows) != 3:
        raise FleetError(
            "compare-windows requires exactly one large, medium, and small window"
        )
    for window in windows:
        if (
            window.get("schema_version") != WINDOW_SCHEMA_VERSION
            or window.get("document_type") != "glaeda-owned-fleet-window-receipt"
        ):
            raise FleetError("compare-windows received an unsupported window receipt")

    by_profile: dict[str, dict[str, Any]] = {}
    for window in windows:
        profile_id = window.get("profile_id")
        if profile_id in by_profile:
            raise FleetError("compare-windows received a duplicate contention profile")
        by_profile[profile_id] = window
    if set(by_profile) != {"large", "medium", "small"}:
        raise FleetError(
            "compare-windows requires exactly large, medium, and small profiles"
        )

    comparable_keys = (
        "experiment_id",
        "machine_id",
        "machine_comparison_digest",
        "backend_id",
        "runtime_digest",
        "workload_id",
        "variant",
        "state_class",
        "offered_work_digest",
        "arrival_pattern_id",
        "arrival_pattern_digest",
        "comparison_identity_digest",
        "resource_policy_id",
        "resource_policy_status",
        "resource_policy_evidence_id",
        "aggregate_cpu_millis",
        "aggregate_memory_limit_bytes",
        "window_elapsed_seconds",
    )
    for key in comparable_keys:
        if len({json.dumps(window.get(key), sort_keys=True) for window in windows}) != 1:
            raise FleetError(f"contention windows differ on comparability key {key}")

    rows = []
    best_p90 = min(
        (
            window["final_result_latency_ms"]["p90"]
            for window in windows
            if window["final_result_latency_ms"]["p90"] is not None
        ),
        default=None,
    )
    max_validated = max(
        window["counts"]["validated_completions"] for window in windows
    )
    for name in ("large", "medium", "small"):
        window = by_profile[name]
        flags = []
        if window["counts"]["unfinished"]:
            flags.append("unfinished_work")
        if window["counts"].get("semantic_mismatch_count", 0):
            flags.append("semantic_mismatch")
        if window["counts"].get("unvalidated_completion_count", 0):
            flags.append("unvalidated_completion")
        if window["counts"]["failure_count"]:
            flags.append("failed_work")
        if window["counts"].get("unknown_result_count", 0):
            flags.append("unknown_result")
        if window["counts"]["fallback_count"] or window["counts"]["reset_count"]:
            flags.append("fallback_or_reset")
        if window["counts"]["validated_completions"] < max_validated:
            flags.append("fewer_validated_completions")
        if window["concurrency"]["underfilled"]:
            flags.append("underfilled_declared_concurrency")
        p90 = window["final_result_latency_ms"]["p90"]
        if best_p90 is not None and p90 is not None and p90 > 1.5 * best_p90:
            flags.append("p90_latency_regression_over_50pct")
        rows.append(
            {
                "profile_id": name,
                "declared_jobs": window["concurrency"]["declared_jobs"],
                "maximum_simultaneous_observed": window["concurrency"][
                    "maximum_simultaneous_observed"
                ],
                "validated_completions": window["counts"][
                    "validated_completions"
                ],
                "p50_ms": window["final_result_latency_ms"]["p50"],
                "p90_ms": p90,
                "unfinished": window["counts"]["unfinished"],
                "semantic_mismatch_count": window["counts"].get(
                    "semantic_mismatch_count", 0
                ),
                "unvalidated_completion_count": window["counts"].get(
                    "unvalidated_completion_count", 0
                ),
                "failure_count": window["counts"]["failure_count"],
                "unknown_result_count": window["counts"].get(
                    "unknown_result_count", 0
                ),
                "swap_max_bytes": window["resources"][
                    "swap_used_max_observed_bytes"
                ],
                "pressure_some_avg10_max": window["resources"][
                    "memory_psi_some_avg10_max_observed"
                ],
                "fallback_count": window["counts"]["fallback_count"],
                "reset_count": window["counts"]["reset_count"],
                "collapse_flags": flags,
            }
        )

    first = windows[0]
    return {
        "schema_version": 1,
        "document_type": "glaeda-owned-fleet-contention-comparison",
        "authority": (
            "capacity_observation_only"
            if first.get("resource_policy_status") == "enforced"
            else "diagnostic_observation_only"
        ),
        "experiment_id": first["experiment_id"],
        "machine_id": first["machine_id"],
        "machine_comparison_digest": first["machine_comparison_digest"],
        "backend_id": first["backend_id"],
        "workload_id": first["workload_id"],
        "variant": first.get("variant"),
        "state_class": first["state_class"],
        "offered_work_digest": first["offered_work_digest"],
        "arrival_pattern_digest": first["arrival_pattern_digest"],
        "resource_policy_id": first["resource_policy_id"],
        "resource_policy_status": first["resource_policy_status"],
        "resource_policy_evidence_id": first.get("resource_policy_evidence_id"),
        "aggregate_cpu_millis": first["aggregate_cpu_millis"],
        "aggregate_memory_limit_bytes": first["aggregate_memory_limit_bytes"],
        "window_elapsed_seconds": first["window_elapsed_seconds"],
        "rows": rows,
    }
