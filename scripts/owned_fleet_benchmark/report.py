from __future__ import annotations

import math
import os
import shlex
from pathlib import Path
from typing import Any

from .model import (
    FleetError,
    catalog,
    load_json,
    machine_comparison_digest,
    render,
)
from .window import nearest_rank


def _validate_report_evidence(
    machine: dict[str, Any],
    receipts: list[dict[str, Any]],
    windows: list[dict[str, Any]],
    econ: list[dict[str, Any]],
) -> str | None:
    evidence = [*receipts, *windows, *econ]
    if not evidence:
        return None
    if machine.get("status") != "complete":
        raise FleetError(
            "a report with physical evidence requires a complete machine receipt"
        )
    comparison_digest = machine_comparison_digest(machine)
    for item in evidence:
        if item.get("machine_id") != machine["machine_id"]:
            raise FleetError("report evidence belongs to a different machine_id")
        if (
            item.get("document_type")
            != "glaeda-owned-fleet-window-partial-receipt"
            and item.get("machine_comparison_digest") != comparison_digest
        ):
            raise FleetError(
                "report evidence belongs to a different machine comparison identity"
            )
    return comparison_digest


def _measured_number(value: Any) -> int | float | None:
    """One actually observed number, or None when the fact was never measured.

    `resource.swap_used_bytes()` returns None when its probe fails or the platform
    is unsupported, and that None reaches the window receipt. Unmeasured is not
    measured-zero, so it must not satisfy a gate that exists to prove a zero.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if isinstance(value, float) and not math.isfinite(value):
        return None
    return value


def _window_label(window: dict[str, Any]) -> str:
    return (
        f"`{window.get('workload_id')}` / `{window.get('backend_id')}` / "
        f"`{window.get('profile_id')}`"
    )


def _stable_profile_sets(
    windows: list[dict[str, Any]],
) -> tuple[set[tuple[str, str | None]], list[str]]:
    """Profile sets proven stable, plus an explicit reason for each unproven one.

    A stability gate may only be satisfied by evidence that was observed. A window
    that never measured swap, or that reports no failure count at all, is reported
    as unproven rather than counted as clean.
    """
    grouped: dict[tuple[Any, ...], list[dict[str, Any]]] = {}
    for window in windows:
        key = (
            window.get("experiment_id"),
            window.get("machine_comparison_digest"),
            window.get("backend_id"),
            window.get("runtime_digest"),
            window.get("workload_id"),
            window.get("variant"),
            window.get("state_class"),
            window.get("comparison_identity_digest"),
            window.get("resource_policy_id"),
            window.get("resource_policy_status"),
            window.get("resource_policy_evidence_id"),
            window.get("offered_work_digest"),
            window.get("arrival_pattern_digest"),
            window.get("aggregate_cpu_millis"),
            window.get("aggregate_memory_limit_bytes"),
            window.get("window_elapsed_seconds"),
        )
        grouped.setdefault(key, []).append(window)

    stable: set[tuple[str, str | None]] = set()
    unproven: list[str] = []
    for group in grouped.values():
        if len(group) != 3:
            continue
        if {item.get("profile_id") for item in group} != {
            "large",
            "medium",
            "small",
        }:
            continue
        if group[0].get("resource_policy_status") != "enforced":
            continue
        unmeasured: list[str] = []
        for item in group:
            label = _window_label(item)
            p90 = _measured_number(item.get("final_result_latency_ms", {}).get("p90"))
            if p90 is None or p90 < 0:
                unmeasured.append(
                    f"{label} has no finite nonnegative p90, so latency stability is unproven"
                )
            counts = item.get("counts") or {}
            if _measured_number(counts.get("failure_count")) is None:
                unmeasured.append(
                    f"{label} reports no failure_count, so a clean run is unproven"
                )
            resources = item.get("resources") or {}
            swap_growth = resources.get("swap_growth_max_observed_bytes")
            if _measured_number(swap_growth) is None:
                unmeasured.append(
                    f"{label} never observed swap, so swap growth is unproven"
                )
        if unmeasured:
            unproven.extend(unmeasured)
            continue

        best_p90 = min(item["final_result_latency_ms"]["p90"] for item in group)
        if all(
            item["counts"]["validated_completions"] == item["counts"]["offered"]
            and item["counts"]["unfinished"] == 0
            and _measured_number(item["counts"]["failure_count"]) == 0
            and item["counts"]["fallback_count"] == 0
            and item["counts"]["reset_count"] == 0
            and not item.get("concurrency", {}).get("underfilled", True)
            and _measured_number(
                item["resources"]["swap_growth_max_observed_bytes"]
            )
            == 0
            and float(item["final_result_latency_ms"]["p90"]) <= best_p90 * 1.5
            for item in group
        ):
            first = group[0]
            stable.add((first.get("workload_id"), first.get("backend_id")))
    return stable, unproven

def markdown_report(
    machine: dict[str, Any],
    receipts: list[dict[str, Any]],
    windows: list[dict[str, Any]],
    econ: list[dict[str, Any]],
) -> str:
    _validate_report_evidence(machine, receipts, windows, econ)
    lines = [
        "# Owned-fleet machine report",
        "",
        (
            f"Machine: `{machine.get('machine_id', 'unknown')}` — "
            f"{machine.get('machine_config') or 'configuration incomplete'}"
        ),
        "",
        (
            "This report uses validated useful work only. Synthetic CPU scores "
            "carry no acquisition authority."
        ),
        "",
        "## What does this machine do well?",
        "",
    ]

    validated = [
        receipt
        for receipt in receipts
        if receipt.get("result", {}).get("validated") is True
    ]
    if not validated:
        lines.append(
            "Physical useful-work samples are pending. No workload role is earned yet."
        )
    else:
        by_group: dict[tuple[Any, ...], list[dict[str, Any]]] = {}
        for receipt in validated:
            execution = receipt.get("execution") or {}
            storage = receipt.get("storage") or {}
            key = (
                receipt["workload"]["id"],
                receipt["workload"].get("variant"),
                receipt["workload"].get("commit"),
                receipt["workload"].get("tree"),
                receipt["workload"].get("operation_digest"),
                receipt.get("toolchain", {}).get("digest"),
                execution.get("backend_id", "unknown"),
                execution.get("runtime", {}).get("digest"),
                execution.get("resource_policy_id"),
                execution.get("resource_policy_status"),
                execution.get("resource_policy_evidence_id"),
                execution.get("declared_cpu_millis"),
                execution.get("declared_memory_limit_bytes"),
                execution.get("timeout_seconds"),
                receipt["state"]["class"],
                storage.get("source_tier"),
                storage.get("source_id"),
                storage.get("state_tier"),
                storage.get("state_id"),
                storage.get("source_filesystem"),
                storage.get("state_filesystem"),
            )
            by_group.setdefault(key, []).append(receipt)
        for key, samples in sorted(by_group.items(), key=lambda item: repr(item[0])):
            wid = key[0]
            backend_id = key[6]
            state_class = key[14]
            p50 = nearest_rank(
                [
                    float(
                        sample["milestones"][
                            "request_known_to_final_result_ms"
                        ]
                    )
                    for sample in samples
                ],
                0.5,
            )
            lines.append(
                f"- `{wid}` / `{backend_id}` / `{state_class}`: "
                f"{len(samples)} exact-comparable validated samples, "
                f"final-result p50 {p50:.1f} ms."
            )

    lines += ["", "## How many useful concurrent jobs can it sustain?", ""]
    if not windows:
        lines.append(
            "Contention windows are pending. Run fixed-offer `large`, "
            "`medium`, and `small` windows before assigning fleet capacity."
        )
    else:
        for window in sorted(
            windows,
            key=lambda value: (
                value.get("workload_id", ""),
                value.get("backend_id", ""),
                value.get("profile_id", ""),
            ),
        ):
            counts = window["counts"]
            latency = window["final_result_latency_ms"]
            concurrency = window.get("concurrency") or {}
            failure_count = _measured_number(counts.get("failure_count"))
            lines.append(
                f"- `{window['workload_id']}` / "
                f"`{window.get('backend_id') or 'unobserved'}` / "
                f"`{window['profile_id']}`: "
                f"{counts['validated_completions']}/{counts['offered']} validated, "
                f"semantic_mismatches={counts.get('semantic_mismatch_count', 0)}, "
                f"unvalidated={counts.get('unvalidated_completion_count', 0)}, "
                f"failures={failure_count if failure_count is not None else 'unobserved'}, "
                f"unknown={counts.get('unknown_result_count', 0)}, "
                f"unfinished={counts['unfinished']}, p50={latency['p50']} ms, "
                f"p90={latency['p90']} ms, declared jobs="
                f"{concurrency.get('declared_jobs')}, max simultaneous observed="
                f"{concurrency.get('maximum_simultaneous_observed')}, "
                f"evidence={window.get('evidence_class', 'unknown')}."
            )

    lines += [
        "",
        "## What existing bottleneck would buying another one remove?",
        "",
    ]
    bottlenecks: list[str] = []
    quality_caveats: list[str] = []
    for window in windows:
        if (
            window.get("document_type")
            == "glaeda-owned-fleet-window-partial-receipt"
        ):
            continue
        if window.get("resource_policy_status") != "enforced":
            continue
        label = (
            f"`{window['workload_id']}` / `{window.get('backend_id')}` / "
            f"`{window['profile_id']}`"
        )
        if window["counts"]["unfinished"] > 0:
            bottlenecks.append(
                f"{label} leaves {window['counts']['unfinished']} offered jobs unfinished"
            )
        if window["counts"].get("semantic_mismatch_count", 0):
            quality_caveats.append(
                f"{label} records semantic-result mismatches"
            )
        if window["counts"].get("unvalidated_completion_count", 0):
            quality_caveats.append(
                f"{label} records source/currentness-invalid completions"
            )
        if window["counts"].get("unknown_result_count", 0):
            quality_caveats.append(
                f"{label} records unknown terminal results"
            )
        if window["counts"].get("failure_count", 0):
            quality_caveats.append(
                f"{label} records {window['counts']['failure_count']} failed jobs"
            )
        if (
            window["counts"]["fallback_count"]
            or window["counts"]["reset_count"]
        ):
            quality_caveats.append(
                f"{label} records fallback/reset activity"
            )
        swap_growth = window.get("resources", {}).get(
            "swap_growth_max_observed_bytes"
        )
        if isinstance(swap_growth, (int, float)) and swap_growth > 0:
            bottlenecks.append(
                f"{label} grows swap by up to {int(swap_growth)} bytes under fixed offered work"
            )

    queue_groups: dict[tuple[str, str], list[float]] = {}
    for receipt in validated:
        queue = receipt.get("queue_delay_ms")
        if isinstance(queue, (int, float)) and queue >= 0:
            key = (
                receipt["workload"]["id"],
                receipt.get("execution", {}).get("backend_id", "unknown"),
            )
            queue_groups.setdefault(key, []).append(float(queue))
    for (wid, backend_id), values in sorted(queue_groups.items()):
        if len(values) >= 2:
            p50_queue = nearest_rank(values, 0.5)
            if p50_queue and p50_queue > 0:
                bottlenecks.append(
                    f"`{wid}` / `{backend_id}` has repeated measured queue "
                    f"delay (p50 {p50_queue:.1f} ms)"
                )

    if bottlenecks:
        lines.extend(f"- {item}." for item in dict.fromkeys(bottlenecks))
    else:
        lines.append(
            "No measured capacity bottleneck yet justifies another node. "
            "Queue, pressure, or unfinished-work evidence must identify "
            "the removed reservoir."
        )

    lines += ["", "## Correctness and reliability caveats", ""]
    if quality_caveats:
        lines.append(
            "These observations block capacity/acquisition conclusions; they carry "
            "zero evidence that another node would remove the problem."
        )
        lines.extend(f"- {item}." for item in dict.fromkeys(quality_caveats))
    else:
        lines.append("No correctness/reliability caveat is present in the supplied windows.")

    lines += [
        "",
        "## At what utilization does ownership beat observed hosted alternatives?",
        "",
    ]
    if not econ:
        lines.append(
            "Equivalent hosted measurements are pending, so no break-even "
            "utilization is claimed."
        )
    else:
        for item in econ:
            by_life: dict[int, float | None] = {}
            for row in item["sensitivities"]:
                by_life.setdefault(
                    row["useful_life_months"],
                    row["break_even_utilization"],
                )
            rendered_thresholds = []
            for months, threshold in sorted(by_life.items()):
                if threshold is None or threshold > 1:
                    rendered_thresholds.append(f"{months}mo=no crossover <=100%")
                else:
                    rendered_thresholds.append(
                        f"{months}mo={threshold * 100:.1f}%"
                    )
            state = item.get("state_context") or {}
            hot = item.get("hot_state_benefit") or {}
            observed_utilization = item.get("observed_utilization")
            observed_rows = [
                row
                for row in item["sensitivities"]
                if row.get("utilization_basis") == "observed"
            ]
            observed_suffix = ""
            if isinstance(observed_utilization, (int, float)) and observed_rows:
                costs = ", ".join(
                    f"{row['useful_life_months']}mo="
                    f"{row['owned_cost_per_validated_completion']:.6g}"
                    for row in observed_rows
                )
                observed_suffix = (
                    f"; observed utilization={observed_utilization * 100:.1f}% "
                    f"owned cost/completion: {costs}"
                )
            hot_suffix = ""
            if hot.get("available") is True:
                hot_suffix = (
                    f"; heat delta vs {hot.get('reference_state_class')}="
                    f"{hot.get('seconds_saved'):.2f} s "
                    "(positive means the selected owned state finished earlier)"
                )
            lines.append(
                f"- `{item['workload_id']}` / `{item.get('backend_id')}` vs "
                f"`{item['hosted']['backend']}` measured "
                f"{item['hosted']['measurement_date']}: "
                + ", ".join(rendered_thresholds)
                + (
                    f"; owned state={state.get('owned_state_class')}, "
                    f"hosted state={state.get('hosted_state_class')}; "
                    f"queue-to-result savings="
                    f"{item['queue_delay_savings_seconds']:.2f} s"
                    + hot_suffix
                    + observed_suffix
                    + "."
                )
            )

    lines += ["", "## What workloads should still stay hosted?", ""]
    hosted_rows = []
    for item in econ:
        if item["queue_delay_savings_seconds"] < 0:
            hosted_rows.append(
                f"`{item['workload_id']}` has an observed hosted path that "
                "reaches the validated result earlier"
            )
        if not any(
            row["ownership_cheaper"] for row in item["sensitivities"]
        ):
            hosted_rows.append(
                f"`{item['workload_id']}` does not clear the modeled ownership "
                "cost band in any listed sensitivity"
            )
    if hosted_rows:
        lines.extend(f"- {row}." for row in dict.fromkeys(hosted_rows))
    else:
        lines.append(
            "Keep burst demand, unsupported capability work, and any workload "
            "whose measured hosted queue-plus-runtime beats the owned path in the "
            "hosted pool. Exact hosted comparisons are required before narrowing "
            "this set."
        )

    lines += ["", "## Hot-state deltas", ""]
    state_order = [
        "cold",
        "dependency_warm",
        "compiler_warm",
        "project_resident",
        "exact_reusable_compiled_product_present",
    ]
    hot_groups: dict[
        tuple[Any, ...], dict[str, list[float]]
    ] = {}
    for receipt in validated:
        execution = receipt.get("execution") or {}
        storage = receipt.get("storage") or {}
        key = (
            receipt["workload"]["id"],
            receipt["workload"].get("variant"),
            receipt["workload"].get("commit"),
            receipt["workload"].get("tree"),
            receipt["workload"].get("operation_digest"),
            receipt.get("toolchain", {}).get("digest"),
            execution.get("backend_id", "unknown"),
            execution.get("runtime", {}).get("digest"),
            execution.get("resource_policy_id"),
            execution.get("resource_policy_status"),
            execution.get("resource_policy_evidence_id"),
            execution.get("declared_cpu_millis"),
            execution.get("declared_memory_limit_bytes"),
            execution.get("timeout_seconds"),
            storage.get("source_tier"),
            storage.get("source_id"),
            storage.get("state_tier"),
            storage.get("state_id"),
            storage.get("source_filesystem"),
            storage.get("state_filesystem"),
        )
        hot_groups.setdefault(key, {}).setdefault(
            receipt["state"]["class"], []
        ).append(
            float(
                receipt["milestones"]["request_known_to_final_result_ms"]
            )
        )
    emitted_hot = False
    for key, states in sorted(hot_groups.items(), key=lambda item: repr(item[0])):
        wid = key[0]
        backend_id = key[6]
        medians = {
            state: nearest_rank(values, 0.5)
            for state, values in states.items()
        }
        available = [
            (state, medians[state])
            for state in state_order
            if state in medians
        ]
        if len(available) >= 2:
            emitted_hot = True
            rendered = ", ".join(
                f"{state}={value:.1f} ms"
                for state, value in available
                if value is not None
            )
            lines.append(
                f"- `{wid}` / `{backend_id}`: {rendered}."
            )
    if not emitted_hot:
        lines.append(
            "At least two validated state classes on the same workload/backend "
            "are required before a hot-state delta is reported."
        )

    lines += [
        "",
        "## Fleet-planning role evidence",
        "",
        (
            "These labels are acquisition/redeployment advice only. CMUX execution-role "
            "eligibility remains owned by the CMUX fleet acceptance contract."
        ),
        "",
    ]
    stable_sets, unproven_stability = _stable_profile_sets(windows)
    sample_counts: dict[tuple[str, str], int] = {}
    for receipt in validated:
        key = (
            receipt["workload"]["id"],
            receipt.get("execution", {}).get("backend_id", "unknown"),
        )
        sample_counts[key] = sample_counts.get(key, 0) + 1

    role_evidence: list[str] = []
    os_name = str((machine.get("platform") or {}).get("os", "")).lower()

    cmux_pair = {
        "cmux-native-apple-incremental-v1",
        "cmux-native-apple-broad-v1",
    }
    if (
        os_name in {"macos", "darwin"}
        and all(sample_counts.get((wid, "native-macos"), 0) >= 2 for wid in cmux_pair)
        and any((wid, "native-macos") in stable_sets for wid in cmux_pair)
    ):
        role_evidence.append("latency-sensitive Apple build node")

    linux_workloads = {
        wid
        for (wid, backend_id), count in sample_counts.items()
        if backend_id == "native-linux"
        and count >= 2
        and wid.startswith(("glaeda-", "quarry-"))
    }
    if (
        os_name == "linux"
        and len(linux_workloads) >= 2
        and any((wid, "native-linux") in stable_sets for wid in linux_workloads)
    ):
        role_evidence.append("native-Linux base-load node")

    broad_keys = {
        ("glaeda-rust-broad-v1", "native-linux"),
        ("glaeda-rust-broad-v1", "lima-vz"),
        ("quarry-verification-v1", "native-linux"),
        ("quarry-verification-v1", "lima-vz"),
    }
    if any(
        sample_counts.get(key, 0) >= 2 and key in stable_sets
        for key in broad_keys
    ):
        role_evidence.append("background/replay node")

    storage_groups: dict[
        tuple[Any, ...], dict[tuple[str, str, str | None], list[float]]
    ] = {}
    for receipt in validated:
        execution = receipt.get("execution") or {}
        storage = receipt.get("storage") or {}
        common_key = (
            receipt["workload"]["id"],
            receipt["workload"].get("variant"),
            receipt["workload"].get("commit"),
            receipt["workload"].get("tree"),
            receipt["workload"].get("operation_digest"),
            receipt.get("toolchain", {}).get("digest"),
            execution.get("backend_id", "unknown"),
            execution.get("runtime", {}).get("digest"),
            execution.get("resource_policy_id"),
            execution.get("resource_policy_status"),
            execution.get("resource_policy_evidence_id"),
            execution.get("declared_cpu_millis"),
            execution.get("declared_memory_limit_bytes"),
            execution.get("timeout_seconds"),
            receipt["state"]["class"],
            storage.get("source_tier"),
            storage.get("source_id"),
            storage.get("source_filesystem"),
        )
        tier = storage.get("state_tier")
        state_id = storage.get("state_id")
        state_fs = storage.get("state_filesystem")
        if tier in {"internal", "external_nvme"} and isinstance(state_id, str):
            treatment = (tier, state_id, state_fs)
            storage_groups.setdefault(common_key, {}).setdefault(
                treatment, []
            ).append(
                float(
                    receipt["milestones"][
                        "request_known_to_final_result_ms"
                    ]
                )
            )

    storage_role = False
    for treatments in storage_groups.values():
        internal = [
            (identity, values)
            for identity, values in treatments.items()
            if identity[0] == "internal" and len(values) >= 2
        ]
        external = [
            (identity, values)
            for identity, values in treatments.items()
            if identity[0] == "external_nvme" and len(values) >= 2
        ]
        for internal_identity, internal_values in internal:
            for external_identity, external_values in external:
                internal_p50 = nearest_rank(internal_values, 0.5)
                external_p50 = nearest_rank(external_values, 0.5)
                if (
                    internal_p50 is not None
                    and external_p50 is not None
                    and external_p50 <= internal_p50 * 1.10
                ):
                    role_evidence.append(
                        "cache/artifact node candidate; external-state "
                        f"`{external_identity[1]}` stayed within 10% of "
                        f"internal `{internal_identity[1]}` p50 on an exact "
                        "comparable workload"
                    )
                    storage_role = True
                    break
            if storage_role:
                break
        if storage_role:
            break

    if any(
        item["queue_delay_savings_seconds"] < 0
        or not any(row["ownership_cheaper"] for row in item["sensitivities"])
        for item in econ
    ):
        role_evidence.append(
            "burst-only hosted pool retained for workloads where observed hosted "
            "completion or cost still wins"
        )

    if role_evidence:
        lines.extend(f"- {role}" for role in dict.fromkeys(role_evidence))
    else:
        lines.append(
            "No fleet-planning role is classified before repeated validated samples and "
            "stable contention evidence exist."
        )
    if unproven_stability:
        lines += [
            "",
            (
                "Stability is unproven for the following windows; an unmeasured "
                "fact is not a clean measurement and does not satisfy the gate:"
            ),
            "",
        ]
        lines.extend(
            f"- {reason}" for reason in dict.fromkeys(unproven_stability)
        )
    lines.append("")
    return "\n".join(lines)


def collect_json_files(
    directory: Path | None, document_type: str | tuple[str, ...]
) -> list[dict[str, Any]]:
    if directory is None or not directory.exists():
        return []
    allowed = (
        {document_type}
        if isinstance(document_type, str)
        else set(document_type)
    )
    values = []
    for path in sorted(directory.glob("*.json")):
        # A corrupt receipt is missing evidence, not absent evidence. Refuse the
        # report instead of quietly producing one that omits it.
        value = load_json(path)
        if not isinstance(value, dict):
            raise FleetError(
                f"benchmark evidence {path} is not a JSON object"
            )
        if value.get("document_type") in allowed:
            values.append(value)
    return values


def plan_text(
    workload: dict[str, Any],
    operation: dict[str, Any],
    variant: str | None,
    repo_root: Path,
    state_class: str,
    state_dir: Path,
) -> str:
    semantic = state_dir / "semantic-receipt.json"
    environment = workload.get("environment") or {}
    lines = [
        f"workload={workload['id']}",
        f"variant={variant or 'default'}",
        f"repository={workload['repository']}",
        f"commit={workload['commit']}",
        f"tree={workload['tree']}",
        f"state_class={state_class}",
        "",
        "# Exact source",
        (
            f"git -C {shlex.quote(os.fspath(repo_root))} fetch origin "
            f"{workload['commit']}"
        ),
        (
            f"git -C {shlex.quote(os.fspath(repo_root))} checkout --detach "
            f"{workload['commit']}"
        ),
        (
            f'test "$(git -C {shlex.quote(os.fspath(repo_root))} '
            f'rev-parse HEAD^{{tree}})" = {workload["tree"]}'
        ),
        "",
        (
            "# Preflight/toolchain (outside command-start timing but inside "
            "request-known -> command-start)"
        ),
    ]
    lines.extend(workload["toolchain"].get("preflight", []))
    lines.extend(workload["toolchain"].get("probes", []))
    lines += ["", "# Benchmark-owned state environment"]
    for key, value in environment.items():
        lines.append(
            f"export {key}="
            f"{shlex.quote(value.format(state_dir=os.fspath(state_dir)))}"
        )
    lines += [
        "",
        "# Timed semantic operation",
        render(operation["command"], state_dir, semantic),
        "",
        "# Required run identity",
        (
            "Choose an explicit backend_id (for example native-macos, "
            "native-linux, or lima-vz) and record source/state storage tiers."
        ),
        "",
        "# Expected receipt",
        (
            "A successful run emits glaeda-owned-fleet-benchmark-receipt "
            "schema_version=1 with result.validated=true."
        ),
    ]
    return "\n".join(lines) + "\n"
