#!/usr/bin/env python3
"""Quiet operator projection for one CMUX-owned Glaeda fleet node."""
from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path
from typing import Any

MODULE_PATH = Path(__file__).with_name("cmux_fleet.py")
SPEC = importlib.util.spec_from_file_location("cmux_fleet_operator_base", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cmux fleet module is unavailable")
fleet = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fleet)

SUMMARY_SCHEMA = "glaeda-cmux-fleet-operator-summary/v1"


def _role_reason(unavailable_roles: list[dict[str, object]]) -> str:
    reasons = sorted(
        {
            item["reason"]
            for item in unavailable_roles
            if isinstance(item.get("reason"), str)
        }
    )
    if len(reasons) == 1:
        return reasons[0]
    return "role_acceptance_required"


def summarize_status(
    enrollment_value: object,
    status_value: object,
) -> dict[str, Any]:
    enrollment = fleet.validate_enrollment(enrollment_value)
    if not isinstance(status_value, dict):
        raise fleet.FleetError("node status is invalid")
    status = status_value
    if (
        status.get("schema") != fleet.STATUS_SCHEMA
        or status.get("nodeId") != enrollment["nodeId"]
        or status.get("enrollmentGeneration") != enrollment["enrollmentGeneration"]
        or status.get("state") != enrollment["state"]
        or type(status.get("routingCandidateEligible")) is not bool
        or status.get("automaticDispatchAuthorized") is not False
        or not isinstance(status.get("roles"), list)
    ):
        raise fleet.FleetError("node status does not match enrollment")

    roles: list[dict[str, object]] = []
    for item in status["roles"]:
        if (
            not isinstance(item, dict)
            or set(item) != {"role", "eligible", "reason"}
            or item.get("role") not in enrollment["allowedExecutionRoles"]
            or type(item.get("eligible")) is not bool
            or not isinstance(item.get("reason"), str)
        ):
            raise fleet.FleetError("node status role projection is invalid")
        roles.append(item)
    if sorted(item["role"] for item in roles) != enrollment["allowedExecutionRoles"]:
        raise fleet.FleetError("node status roles do not match enrollment")

    eligible_roles = sorted(
        str(item["role"]) for item in roles if item["eligible"] is True
    )
    unavailable_roles = [
        {
            "role": str(item["role"]),
            "reason": str(item["reason"]),
        }
        for item in roles
        if item["eligible"] is False
    ]

    state = enrollment["state"]
    if state == "eligible" and eligible_roles:
        health_class = "healthy"
        attention_required = False
        action = "none"
        reason = "ready"
    elif state == "eligible":
        health_class = "attention"
        attention_required = True
        action = "run_role_acceptance"
        reason = _role_reason(unavailable_roles)
    elif state == "discovered":
        health_class = "attention"
        attention_required = True
        action = "complete_enrollment"
        reason = "node_discovered"
    elif state == "enrolling":
        health_class = "attention"
        attention_required = True
        action = "run_role_acceptance"
        reason = "node_enrolling"
    elif state == "draining":
        health_class = "attention"
        attention_required = True
        action = "observe_drain"
        reason = "node_draining"
    elif state == "quarantined":
        health_class = "attention"
        attention_required = True
        action = "inspect_quarantine"
        reason = enrollment["quarantineReason"]
    elif state == "retired":
        health_class = "inactive"
        attention_required = False
        action = "none"
        reason = "node_retired"
    else:
        raise fleet.FleetError("node state has no operator projection")

    summary = {
        "schema": SUMMARY_SCHEMA,
        "nodeId": enrollment["nodeId"],
        "state": state,
        "healthClass": health_class,
        "attentionRequired": attention_required,
        "action": action,
        "reason": reason,
        "routingCandidateEligible": status["routingCandidateEligible"],
        "automaticDispatchAuthorized": False,
        "eligibleRoles": eligible_roles,
        "unavailableRoles": unavailable_roles,
    }
    if len(fleet.canonical(summary)) > fleet.MAX_STATUS_BYTES:
        raise fleet.FleetError("operator summary exceeds size ceiling")
    return summary


def operator_summary(
    enrollment_value: object,
    acceptance_values: list[object],
) -> dict[str, Any]:
    enrollment = fleet.validate_enrollment(enrollment_value)
    status = fleet.node_status(enrollment, acceptance_values)
    return summarize_status(enrollment, status)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("enrollment", type=Path)
    result.add_argument("--acceptance", action="append", type=Path, default=[])
    return result


def main() -> int:
    try:
        args = parser().parse_args()
        enrollment = fleet.load(args.enrollment)
        acceptances = [fleet.load(path) for path in args.acceptance]
        fleet.emit(operator_summary(enrollment, acceptances))
        return 0
    except (OSError, fleet.FleetError) as error:
        import json
        import sys

        print(
            json.dumps({"error": str(error)}, sort_keys=True, separators=(",", ":")),
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
