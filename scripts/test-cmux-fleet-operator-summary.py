#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

MODULE_PATH = Path(__file__).with_name("cmux_fleet_operator_summary.py")
SPEC = importlib.util.spec_from_file_location("cmux_fleet_operator_summary", MODULE_PATH)
assert SPEC and SPEC.loader
s = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(s)

A = "sha256:" + "a" * 64
B = "sha256:" + "b" * 64
C = "sha256:" + "c" * 64


def enrollment(state="eligible"):
    value = {
        "schema": s.fleet.ENROLLMENT_SCHEMA,
        "nodeId": "cmux-fixture-001",
        "architecture": "arm64",
        "os": {"family": "macos", "versionClass": "macos-26"},
        "hardwareCapabilityClass": "cmux-mac-build-large",
        "supportedToolchainGenerations": [A, B],
        "roleProfiles": {
            "cmux_macos_native_build": dict(
                s.fleet.ROLE_PROFILES["cmux_macos_native_build"]
            )
        },
        "allowedExecutionRoles": ["cmux_macos_native_build"],
        "operatorFleetScope": "cmux-founders",
        "enrollmentGeneration": 3,
        "glaedaGeneration": C,
        "state": state,
        "quarantineReason": None,
    }
    if state == "quarantined":
        value["quarantineReason"] = "disk_pressure"
    return value


def status(state="eligible", *, eligible=True, reason="accepted"):
    return {
        "schema": s.fleet.STATUS_SCHEMA,
        "nodeId": "cmux-fixture-001",
        "enrollmentGeneration": 3,
        "state": state,
        "routingCandidateEligible": eligible,
        "automaticDispatchAuthorized": False,
        "capability": {},
        "roles": [
            {
                "role": "cmux_macos_native_build",
                "eligible": eligible,
                "reason": reason,
            }
        ],
        "privacy": {},
    }


class OperatorSummaryTests(unittest.TestCase):
    def test_healthy_node_is_quiet(self):
        summary = s.summarize_status(enrollment(), status())
        self.assertEqual(summary["healthClass"], "healthy")
        self.assertFalse(summary["attentionRequired"])
        self.assertEqual(summary["action"], "none")
        self.assertEqual(summary["reason"], "ready")
        self.assertEqual(summary["eligibleRoles"], ["cmux_macos_native_build"])
        self.assertFalse(summary["automaticDispatchAuthorized"])

    def test_missing_or_stale_acceptance_requests_acceptance(self):
        for reason in (
            "acceptance_missing_or_rejected",
            "acceptance_enrollment_stale",
            "acceptance_glaeda_stale",
            "acceptance_glaeda_contract_stale",
            "acceptance_toolchain_stale",
            "acceptance_profile_stale",
        ):
            with self.subTest(reason=reason):
                summary = s.summarize_status(
                    enrollment(),
                    status(eligible=False, reason=reason),
                )
                self.assertTrue(summary["attentionRequired"])
                self.assertEqual(summary["action"], "run_role_acceptance")
                self.assertEqual(summary["reason"], reason)
                self.assertEqual(
                    summary["unavailableRoles"],
                    [{"role": "cmux_macos_native_build", "reason": reason}],
                )

    def test_enrollment_states_have_concrete_next_action(self):
        cases = {
            "discovered": ("complete_enrollment", "node_discovered"),
            "enrolling": ("run_role_acceptance", "node_enrolling"),
            "draining": ("observe_drain", "node_draining"),
        }
        for state_name, expected in cases.items():
            with self.subTest(state=state_name):
                summary = s.summarize_status(
                    enrollment(state_name),
                    status(
                        state_name,
                        eligible=False,
                        reason=f"node_{state_name}",
                    ),
                )
                self.assertTrue(summary["attentionRequired"])
                self.assertEqual((summary["action"], summary["reason"]), expected)

    def test_quarantine_surfaces_bounded_reviewed_reason(self):
        summary = s.summarize_status(
            enrollment("quarantined"),
            status(
                "quarantined",
                eligible=False,
                reason="node_quarantined",
            ),
        )
        self.assertEqual(summary["healthClass"], "attention")
        self.assertTrue(summary["attentionRequired"])
        self.assertEqual(summary["action"], "inspect_quarantine")
        self.assertEqual(summary["reason"], "disk_pressure")

    def test_retired_node_is_quiet_and_inactive(self):
        summary = s.summarize_status(
            enrollment("retired"),
            status("retired", eligible=False, reason="node_retired"),
        )
        self.assertEqual(summary["healthClass"], "inactive")
        self.assertFalse(summary["attentionRequired"])
        self.assertEqual(summary["action"], "none")
        self.assertEqual(summary["reason"], "node_retired")

    def test_real_status_without_acceptance_projects_one_action(self):
        summary = s.operator_summary(enrollment(), [])
        self.assertEqual(summary["action"], "run_role_acceptance")
        self.assertEqual(summary["reason"], "acceptance_missing_or_rejected")

    def test_forged_status_cannot_expand_authority(self):
        forged = status()
        forged["automaticDispatchAuthorized"] = True
        with self.assertRaisesRegex(
            s.fleet.FleetError,
            "does not match enrollment",
        ):
            s.summarize_status(enrollment(), forged)

    def test_private_fields_do_not_enter_projection(self):
        summary = s.summarize_status(enrollment(), status())
        encoded = s.fleet.canonical(summary).decode("utf-8")
        for forbidden in ("hostname", "serialNumber", "privateIp", "username"):
            self.assertNotIn(forbidden, encoded)


if __name__ == "__main__":
    unittest.main()
