from __future__ import annotations

import math
import re
from datetime import date
from typing import Any

from .model import (
    FleetError,
    catalog,
    machine_comparison_digest,
    validate_machine,
)

ECONOMICS_SCHEMA_VERSION = 1
SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")


def _finite_number(value: Any, field: str, *, minimum: float | None = None) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise FleetError(f"{field} must be numeric")
    number = float(value)
    if not math.isfinite(number):
        raise FleetError(f"{field} must be finite")
    if minimum is not None and number < minimum:
        raise FleetError(f"{field} must be at least {minimum}")
    return number


def _iso_date(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value:
        raise FleetError(f"{field} must be a date")
    try:
        date.fromisoformat(value)
    except ValueError as exc:
        raise FleetError(f"{field} must be YYYY-MM-DD") from exc
    return value


def hosted_cost(
    hosted: dict[str, Any], purchase_currency: str
) -> tuple[float, float, float]:
    if (
        hosted.get("schema_version") != 1
        or hosted.get("document_type") != "glaeda-hosted-equivalent-job-receipt"
    ):
        raise FleetError("unsupported hosted-equivalent receipt")
    if hosted.get("validated") is not True:
        raise FleetError("hosted comparator must be semantically validated")
    for key in ("backend", "billing_currency", "rate_source"):
        if not isinstance(hosted.get(key), str) or not hosted[key]:
            raise FleetError(f"hosted comparator requires {key}")
    evidence = hosted.get("measurement_evidence_sha256")
    if not isinstance(evidence, str) or SHA256_RE.fullmatch(evidence) is None:
        raise FleetError(
            "hosted comparator requires measurement_evidence_sha256"
        )
    _iso_date(hosted.get("measurement_date"), "measurement_date")

    wall = _finite_number(
        hosted.get("actual_wall_seconds"), "actual_wall_seconds", minimum=0.000001
    )
    _finite_number(
        hosted.get("queue_delay_seconds"), "queue_delay_seconds", minimum=0
    )
    rate = _finite_number(
        hosted.get("rate_per_minute"), "rate_per_minute", minimum=0
    )
    increment = _finite_number(
        hosted.get("billing_increment_seconds", 1),
        "billing_increment_seconds",
        minimum=0.000001,
    )
    minimum = _finite_number(
        hosted.get("minimum_billed_seconds", 0),
        "minimum_billed_seconds",
        minimum=0,
    )
    billed = max(minimum, math.ceil(wall / increment) * increment)
    native_cost = rate * billed / 60

    billing_currency = hosted["billing_currency"]
    raw_fx = hosted.get("fx_to_purchase_currency")
    if billing_currency == purchase_currency:
        if raw_fx is not None and _finite_number(raw_fx, "fx_to_purchase_currency") != 1:
            raise FleetError(
                "same-currency hosted comparison must omit FX or use exactly 1.0"
            )
        fx = 1.0
    else:
        fx = _finite_number(
            raw_fx, "fx_to_purchase_currency", minimum=0.000000001
        )
        _iso_date(hosted.get("fx_observed_at"), "fx_observed_at")
    return native_cost * fx, billed, fx


def economics(
    owned: dict[str, Any],
    hosted: dict[str, Any],
    machine: dict[str, Any],
    electricity_price: float,
    life_months: list[int],
    utilizations: list[float],
    owned_reference: dict[str, Any] | None = None,
    observed_utilization: float | None = None,
) -> dict[str, Any]:
    validate_machine(machine, require_complete=True)
    if (
        owned.get("schema_version") != 1
        or owned.get("document_type") != "glaeda-owned-fleet-benchmark-receipt"
        or owned.get("result", {}).get("validated") is not True
    ):
        raise FleetError(
            "owned economics requires one validated owned benchmark receipt"
        )
    if owned.get("machine_id") != machine["machine_id"]:
        raise FleetError("owned receipt machine_id differs from machine receipt")
    current_machine_digest = machine_comparison_digest(machine)
    if owned.get("machine_comparison_digest") != current_machine_digest:
        raise FleetError(
            "owned receipt machine comparison identity differs from machine receipt"
        )

    purchase = machine["purchase"]
    purchase_price = float(purchase["all_in_price"])
    purchase_currency = purchase["currency"]
    hosted_cost_purchase, hosted_billed_seconds, fx = hosted_cost(
        hosted, purchase_currency
    )

    equivalence = {
        "workload_id": (
            hosted.get("workload_id"),
            owned.get("workload", {}).get("id"),
        ),
        "variant": (
            hosted.get("variant"),
            owned.get("workload", {}).get("variant"),
        ),
        "source_commit": (
            hosted.get("source_commit"),
            owned.get("workload", {}).get("commit"),
        ),
        "source_tree": (
            hosted.get("source_tree"),
            owned.get("workload", {}).get("tree"),
        ),
        "operation_digest": (
            hosted.get("operation_digest"),
            owned.get("workload", {}).get("operation_digest"),
        ),
        "toolchain_digest": (
            hosted.get("toolchain_digest"),
            owned.get("toolchain", {}).get("digest"),
        ),
    }
    mismatched = [
        name for name, pair in equivalence.items() if pair[0] != pair[1]
    ]
    if mismatched:
        raise FleetError(
            "hosted comparator is not semantically equivalent on: "
            + ", ".join(mismatched)
        )

    known_states = set(catalog()["state_classes"])
    owned_state = owned.get("state", {}).get("class")
    hosted_state = hosted.get("state_class")
    if owned_state not in known_states or hosted_state not in known_states:
        raise FleetError("owned and hosted receipts must declare canonical state classes")

    owned_wall_seconds = (
        _finite_number(
            owned.get("milestones", {}).get("request_known_to_final_result_ms"),
            "owned request_known_to_final_result_ms",
            minimum=0.000001,
        )
        / 1000
    )

    hot_state_benefit: dict[str, Any] = {"available": False}
    if owned_reference is not None:
        if (
            owned_reference.get("schema_version") != 1
            or owned_reference.get("document_type")
            != "glaeda-owned-fleet-benchmark-receipt"
            or owned_reference.get("result", {}).get("validated") is not True
        ):
            raise FleetError("owned heat reference must be a validated fleet receipt")
        if owned_reference.get("machine_id") != machine["machine_id"]:
            raise FleetError("owned heat reference belongs to a different machine")
        if owned_reference.get("machine_comparison_digest") != current_machine_digest:
            raise FleetError("owned heat reference machine identity differs")

        def receipt_basis(receipt: dict[str, Any]) -> dict[str, Any]:
            execution = receipt.get("execution") or {}
            storage = receipt.get("storage") or {}
            return {
                "workload_id": receipt.get("workload", {}).get("id"),
                "variant": receipt.get("workload", {}).get("variant"),
                "source_commit": receipt.get("workload", {}).get("commit"),
                "source_tree": receipt.get("workload", {}).get("tree"),
                "operation_digest": receipt.get("workload", {}).get("operation_digest"),
                "toolchain_digest": receipt.get("toolchain", {}).get("digest"),
                "backend_id": execution.get("backend_id"),
                "runtime_digest": execution.get("runtime", {}).get("digest"),
                "resource_policy_id": execution.get("resource_policy_id"),
                "resource_policy_status": execution.get("resource_policy_status"),
                "resource_policy_evidence_id": execution.get(
                    "resource_policy_evidence_id"
                ),
                "declared_cpu_millis": execution.get("declared_cpu_millis"),
                "declared_memory_limit_bytes": execution.get(
                    "declared_memory_limit_bytes"
                ),
                "timeout_seconds": execution.get("timeout_seconds"),
                "source_storage_tier": storage.get("source_tier"),
                "source_storage_id": storage.get("source_id"),
                "state_storage_tier": storage.get("state_tier"),
                "state_storage_id": storage.get("state_id"),
                "source_filesystem": storage.get("source_filesystem"),
                "state_filesystem": storage.get("state_filesystem"),
            }

        if receipt_basis(owned_reference) != receipt_basis(owned):
            raise FleetError(
                "owned heat reference differs outside the heat/state treatment"
            )
        reference_state = owned_reference.get("state", {}).get("class")
        if reference_state not in known_states:
            raise FleetError("owned heat reference must declare a canonical state class")
        if reference_state == owned_state:
            raise FleetError("owned heat reference must use a different state class")
        reference_wall_seconds = (
            _finite_number(
                owned_reference.get("milestones", {}).get(
                    "request_known_to_final_result_ms"
                ),
                "owned reference request_known_to_final_result_ms",
                minimum=0.000001,
            )
            / 1000
        )
        seconds_saved = reference_wall_seconds - owned_wall_seconds
        hot_state_benefit = {
            "available": True,
            "reference_state_class": reference_state,
            "owned_state_class": owned_state,
            "reference_wall_seconds": reference_wall_seconds,
            "owned_wall_seconds": owned_wall_seconds,
            "seconds_saved": seconds_saved,
            "fraction_saved": seconds_saved / reference_wall_seconds,
        }
    owned_queue = (
        _finite_number(owned.get("queue_delay_ms", 0), "owned queue_delay_ms", minimum=0)
        / 1000
    )
    hosted_queue = _finite_number(
        hosted.get("queue_delay_seconds"), "hosted queue_delay_seconds", minimum=0
    )
    hosted_wall = _finite_number(
        hosted.get("actual_wall_seconds"),
        "hosted actual_wall_seconds",
        minimum=0.000001,
    )

    throughput_per_hour = 3600 / owned_wall_seconds
    power = machine.get("power") or {}
    if power.get("measurement_status") != "measured":
        raise FleetError("economics requires measured machine idle/load power")
    idle_watts = _finite_number(power.get("idle_watts"), "idle_watts", minimum=0)
    load_watts = _finite_number(power.get("load_watts"), "load_watts", minimum=0)
    if load_watts < idle_watts:
        raise FleetError("load power must be at least idle power")
    electricity_price = _finite_number(
        electricity_price, "electricity price", minimum=0
    )
    if not life_months or not utilizations:
        raise FleetError("economics requires useful-life and utilization sensitivities")
    if observed_utilization is not None:
        observed_utilization = _finite_number(
            observed_utilization, "observed utilization", minimum=0.000000001
        )
        if observed_utilization > 1:
            raise FleetError("observed utilization must be within (0,1]")

    evaluation_utilizations = list(utilizations)
    if observed_utilization is not None and observed_utilization not in evaluation_utilizations:
        evaluation_utilizations.append(observed_utilization)

    rows = []
    for months in life_months:
        if isinstance(months, bool) or not isinstance(months, int) or months <= 0:
            raise FleetError("useful life months must be positive integers")
        fixed_monthly = (
            purchase_price / months
            + idle_watts * 730 / 1000 * electricity_price
        )
        active_power_cost_per_completion = (
            (load_watts - idle_watts)
            / 1000
            * electricity_price
            / throughput_per_hour
        )
        denominator = (
            (hosted_cost_purchase - active_power_cost_per_completion)
            * 730
            * throughput_per_hour
        )
        threshold = fixed_monthly / denominator if denominator > 0 else None

        for utilization in evaluation_utilizations:
            utilization = _finite_number(
                utilization, "utilization sensitivity", minimum=0.000000001
            )
            if utilization > 1:
                raise FleetError("utilization sensitivity values must be within (0,1]")
            active_hours = 730 * utilization
            monthly_completions = throughput_per_hour * active_hours
            monthly_wh = (
                idle_watts * 730
                + (load_watts - idle_watts) * active_hours
            )
            wh_per_completion = monthly_wh / monthly_completions
            power_cost_per_completion = (
                wh_per_completion / 1000 * electricity_price
            )
            amortization_per_completion = (
                purchase_price / (months * monthly_completions)
            )
            total_cost = (
                amortization_per_completion + power_cost_per_completion
            )
            rows.append(
                {
                    "useful_life_months": months,
                    "utilization": utilization,
                    "utilization_basis": (
                        "observed"
                        if observed_utilization is not None
                        and utilization == observed_utilization
                        else "sensitivity"
                    ),
                    "monthly_validated_completions": monthly_completions,
                    "validated_completions_per_purchase_currency_month": (
                        monthly_completions / purchase_price
                    ),
                    "lifetime_validated_completions_per_purchase_currency": (
                        monthly_completions * months / purchase_price
                    ),
                    "accounted_watt_hours_per_validated_completion": (
                        wh_per_completion
                    ),
                    "validated_completions_per_watt_hour": (
                        1 / wh_per_completion if wh_per_completion else None
                    ),
                    "amortization_cost_per_validated_completion": (
                        amortization_per_completion
                    ),
                    "power_cost_per_validated_completion": (
                        power_cost_per_completion
                    ),
                    "owned_cost_per_validated_completion": total_cost,
                    "hosted_cost_per_validated_completion": (
                        hosted_cost_purchase
                    ),
                    "ownership_cheaper": total_cost < hosted_cost_purchase,
                    "break_even_utilization": threshold,
                }
            )

    execution = owned.get("execution") or {}
    return {
        "schema_version": ECONOMICS_SCHEMA_VERSION,
        "document_type": "glaeda-owned-vs-hosted-economics",
        "authority": "economic_observation_only",
        "machine_id": machine["machine_id"],
        "machine_comparison_digest": current_machine_digest,
        "backend_id": execution.get("backend_id"),
        "workload_id": owned["workload"]["id"],
        "variant": owned["workload"].get("variant"),
        "purchase_currency": purchase_currency,
        "purchase_price": purchase_price,
        "observed_utilization": observed_utilization,
        "state_context": {
            "owned_state_class": owned_state,
            "hosted_state_class": hosted_state,
            "same_state_class": owned_state == hosted_state,
        },
        "hot_state_benefit": hot_state_benefit,
        "hosted": {
            "backend": hosted["backend"],
            "measurement_date": hosted["measurement_date"],
            "measurement_evidence_sha256": hosted["measurement_evidence_sha256"],
            "actual_wall_seconds": hosted_wall,
            "queue_delay_seconds": hosted_queue,
            "billed_seconds": hosted_billed_seconds,
            "rate_per_minute": hosted["rate_per_minute"],
            "rate_source": hosted["rate_source"],
            "billing_currency": hosted["billing_currency"],
            "fx_to_purchase_currency": fx,
            "cost_per_validated_completion_purchase_currency": (
                hosted_cost_purchase
            ),
        },
        "owned": {
            "actual_wall_seconds": owned_wall_seconds,
            "queue_delay_seconds": owned_queue,
            "validated_completions_per_active_hour": throughput_per_hour,
            "idle_watts": idle_watts,
            "load_watts": load_watts,
            "electricity_price_per_kwh_purchase_currency": electricity_price,
        },
        "queue_delay_savings_seconds": (
            hosted_queue + hosted_wall
        )
        - (owned_queue + owned_wall_seconds),
        "sensitivities": rows,
    }
