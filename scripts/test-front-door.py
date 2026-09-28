#!/usr/bin/env python3
"""Focused fixtures for the one-command Glaeda readiness journey."""

from __future__ import annotations

import sys
from pathlib import Path

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

from workspace_bootstrap.journey import (
    RECEIPT_TYPE,
    SCHEMA_VERSION,
    _valid_bootstrap_receipt,
    _valid_doctor_receipt,
    build_journey_report,
    render_human,
)
from workspace_bootstrap.receipt import (
    RECEIPT_TYPE as WORKSPACE_RECEIPT_TYPE,
    SCHEMA_VERSION as WORKSPACE_SCHEMA_VERSION,
    capability_fingerprint,
)


def bootstrap(state: str = "ready", *, commit: str = "1" * 40) -> dict[str, object]:
    blockers = [] if state != "blocked" else [{"code": "required_tool_git_unavailable"}]
    return {
        "schema_version": WORKSPACE_SCHEMA_VERSION,
        "receipt_type": WORKSPACE_RECEIPT_TYPE,
        "state": state,
        "source": {
            "commit": commit,
            "tree": "2" * 40,
            "clean_after": True,
        },
        "capability_fingerprint": "sha256:" + "3" * 64,
        "deviations": [{"code": "optional_tool_just_unavailable"}],
        "blocking_reasons": blockers,
    }


def fingerprint_fixture(schema_version: int, receipt_type: str) -> dict[str, object]:
    return {
        "schema_version": schema_version,
        "receipt_type": receipt_type,
        "state": "ready",
        "operation": "verify",
        "repository_root": {"kind": "git-worktree"},
        "required_tools": [],
        "optional_tools": [],
        "verification_backends": [],
        "formatter_capabilities": [],
        "declared_cache_paths": [],
        "git_identity": {"evaluated": False},
        "publication_readiness": {"evaluated": False},
        "next_verification_profiles": [],
        "deviations": [],
        "blocking_reasons": [],
        "source": {
            "commit": "1" * 40,
            "tree": "2" * 40,
            "clean_before": True,
            "clean_after": True,
        },
        "resources": {"recommended_concurrency": 1},
    }


def doctor(overall: str = "pass") -> dict[str, object]:
    return {
        "schema_version": 1,
        "overall": overall,
        "checks": [{"id": "platform", "status": overall, "summary": "bounded"}],
    }


def main() -> int:
    assert WORKSPACE_SCHEMA_VERSION == 2
    assert WORKSPACE_RECEIPT_TYPE == "glaeda-workspace-capability-receipt"
    assert SCHEMA_VERSION == 2
    assert RECEIPT_TYPE == "glaeda-front-door-readiness-receipt"

    legacy_fingerprint = capability_fingerprint(
        fingerprint_fixture(1, "smolrunner-workspace-capability-receipt")
    )
    successor_fingerprint = capability_fingerprint(
        fingerprint_fixture(WORKSPACE_SCHEMA_VERSION, WORKSPACE_RECEIPT_TYPE)
    )
    assert legacy_fingerprint == "sha256:36c2241c321de864851150fc6aeed7b8163bd9ce0583501bd9585c0eb4492e58"
    assert successor_fingerprint == "sha256:fbd1d3c06c7660323464bca65cd5f023f3da4f05b05a41cc284f2dc35514cc15"
    assert successor_fingerprint != legacy_fingerprint

    ready = build_journey_report(bootstrap(), doctor(), None, bootstrap())
    assert ready["schema_version"] == 2
    assert ready["receipt_type"] == "glaeda-front-door-readiness-receipt"
    assert ready["verdict"] == "ready"
    assert ready["next_action"] == "none"
    assert ready["doctor"] == {"evaluated": True, "overall": "pass"}

    warning = build_journey_report(bootstrap("ready_with_declared_deviations"), doctor("warn"), None, bootstrap("ready_with_declared_deviations"))
    assert warning["verdict"] == "ready"

    blocked = build_journey_report(bootstrap("blocked"), None, None, None)
    assert blocked["verdict"] == "blocked"
    assert blocked["blocking_codes"] == ["required_tool_git_unavailable"]

    failed = build_journey_report(bootstrap(), doctor("fail"), None, bootstrap())
    assert failed["verdict"] == "blocked"
    assert "doctor_reported_failure" in failed["blocking_codes"]

    unavailable = build_journey_report(bootstrap(), None, "doctor_timeout", bootstrap())
    assert unavailable["verdict"] == "blocked"
    assert unavailable["blocking_codes"] == ["doctor_timeout"]

    changed = build_journey_report(bootstrap(), doctor(), None, bootstrap(commit="4" * 40))
    assert changed["verdict"] == "blocked"
    assert "checkout_changed_during_journey" in changed["blocking_codes"]

    assert _valid_bootstrap_receipt(bootstrap())
    legacy_bootstrap = bootstrap()
    legacy_bootstrap["schema_version"] = 1
    legacy_bootstrap["receipt_type"] = "smolrunner-workspace-capability-receipt"
    assert not _valid_bootstrap_receipt(legacy_bootstrap)
    future_bootstrap = bootstrap()
    future_bootstrap["schema_version"] = 3
    assert not _valid_bootstrap_receipt(future_bootstrap)
    assert _valid_doctor_receipt(doctor())
    future_doctor = doctor()
    future_doctor["schema_version"] = 2
    assert not _valid_doctor_receipt(future_doctor)

    public = str(ready) + render_human(ready)
    for marker in ["/Users/", "/home/runner/", "secret.txt", "remote.origin.url", "GIT_ASKPASS", "HOME="]:
        assert marker not in public

    print("Glaeda front-door readiness tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
