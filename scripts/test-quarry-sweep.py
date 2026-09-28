#!/usr/bin/env python3
"""Unit tests for the Quarry sweep experiment adapter.

Pure logic only: no systemd, no bubblewrap, no Git, no network, no root.
Run with: python3 scripts/test-quarry-sweep.py
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from quarry_sweep_impl import (  # noqa: E402
    Refusal,
    SWEEP_CONFIGS,
    WORKLOAD_ID,
    canonical_bytes,
    command_fingerprint,
    decide_admission,
    driver_sha256,
    parse_pressure_value,
    sweep_environment,
    tcp_listen_ports,
    valid_evidence,
    valid_receipt,
    workload_identity,
)


def healthy_observation(**overrides):
    observation = {
        "pressure_avg10": {"cpu": 2.0, "memory": 0.0, "io": 0.5},
        "mem_available_kib": 16 * 1024 * 1024,
        "cpus": 16,
        "vpn_up": True,
        "rdp_listening": True,
    }
    observation.update(overrides)
    return observation


def sample_evidence():
    return {
        "schema_version": 1,
        "records": [
            {
                "config": name,
                "terminal": "succeeded",
                "elapsed_seconds": 0.1,
                "output_bytes": 100,
                "output_sha256": "sha256:" + "ab" * 32,
            }
            for name in SWEEP_CONFIGS
        ],
    }


class PressureTests(unittest.TestCase):
    def test_parses_some_avg10(self):
        self.assertEqual(
            parse_pressure_value("some avg10=2.10 avg60=1.50 avg300=0.54 total=11740103"),
            2.10,
        )

    def test_rejects_unparseable_text(self):
        with self.assertRaises(Refusal):
            parse_pressure_value("")


class AdmissionTests(unittest.TestCase):
    def test_healthy_node_admits(self):
        self.assertEqual(decide_admission(healthy_observation()), ("admit", "compatible"))

    def test_rdp_absence_does_not_block(self):
        disposition, _ = decide_admission(healthy_observation(rdp_listening=False))
        self.assertEqual(disposition, "admit")

    def test_vpn_down_refuses(self):
        self.assertEqual(
            decide_admission(healthy_observation(vpn_up=False)),
            ("refuse", "vpn_lane_impaired"),
        )

    def test_high_cpu_pressure_waits(self):
        observation = healthy_observation(
            pressure_avg10={"cpu": 45.0, "memory": 0.0, "io": 0.5}
        )
        self.assertEqual(decide_admission(observation), ("wait", "pressure_high"))

    def test_high_memory_pressure_waits(self):
        observation = healthy_observation(
            pressure_avg10={"cpu": 1.0, "memory": 12.0, "io": 0.5}
        )
        self.assertEqual(decide_admission(observation), ("wait", "pressure_high"))

    def test_high_io_pressure_waits(self):
        observation = healthy_observation(
            pressure_avg10={"cpu": 1.0, "memory": 0.0, "io": 60.0}
        )
        self.assertEqual(decide_admission(observation), ("wait", "pressure_high"))

    def test_small_memory_waits(self):
        observation = healthy_observation(mem_available_kib=4 * 1024 * 1024)
        self.assertEqual(decide_admission(observation), ("wait", "memory_reserve_unmet"))

    def test_few_cpus_wait(self):
        observation = healthy_observation(cpus=4)
        self.assertEqual(decide_admission(observation), ("wait", "cpu_reserve_unmet"))

    def test_incomplete_observation_refuses(self):
        with self.assertRaises(Refusal):
            decide_admission({"pressure_avg10": {}})

    def test_bool_memory_is_not_an_int(self):
        with self.assertRaises(Refusal):
            decide_admission(healthy_observation(mem_available_kib=True))

    def test_missing_keys_refuse(self):
        observation = healthy_observation()
        del observation["vpn_up"]
        with self.assertRaises(Refusal):
            decide_admission(observation)


class TcpParseTests(unittest.TestCase):
    def test_finds_rdp_listener(self):
        fixture = (
            "  sl  local_address rem_address   st\n"
            "   0: 00000000000000000000000000000000:0D3D "
            "00000000000000000000000000000000:0000 0A\n"
            "   1: 0100007F:0035 00000000:0000 0A\n"
        )
        self.assertIn(3389, tcp_listen_ports(fixture))

    def test_ignores_non_listen(self):
        fixture = (
            "  sl  local_address rem_address   st\n"
            "   0: 5706A8C0:0D3D 0100007F:0035 01\n"
        )
        self.assertNotIn(3389, tcp_listen_ports(fixture))


class WorkloadTests(unittest.TestCase):
    def test_identity_is_fixed(self):
        identity = workload_identity("a" * 40, "b" * 40)
        self.assertEqual(identity["workload_id"], WORKLOAD_ID)
        self.assertEqual(list(identity["configs"]), list(SWEEP_CONFIGS))
        self.assertEqual(len(SWEEP_CONFIGS), 7)

    def test_fingerprint_stable_and_canonical(self):
        first = command_fingerprint("a" * 40, "b" * 40)
        second = command_fingerprint("a" * 40, "b" * 40)
        self.assertEqual(first, second)
        self.assertRegex(first, r"^sha256:[a-f0-9]{64}$")
        self.assertNotEqual(first, command_fingerprint("c" * 40, "b" * 40))

    def test_driver_hash_stable(self):
        self.assertRegex(driver_sha256(), r"^sha256:[a-f0-9]{64}$")

    def test_environment_has_no_ambient_leak(self):
        environment = sweep_environment(Path("/workspace/source"), Path("/tmp/out"))
        self.assertEqual(
            set(environment),
            {"PATH", "HOME", "LC_ALL", "QUARRY_SRC", "QUARRY_SWEEP_CONFIGS", "QUARRY_OUTPUT_DIR"},
        )
        self.assertEqual(
            json.loads(environment["QUARRY_SWEEP_CONFIGS"]), list(SWEEP_CONFIGS)
        )


class EvidenceTests(unittest.TestCase):
    def test_accepts_exact_record_set(self):
        self.assertTrue(valid_evidence(sample_evidence()))

    def test_rejects_wrong_config_order(self):
        evidence = sample_evidence()
        evidence["records"] = list(reversed(evidence["records"]))
        self.assertFalse(valid_evidence(evidence))

    def test_rejects_missing_config(self):
        evidence = sample_evidence()
        evidence["records"] = evidence["records"][:-1]
        self.assertFalse(valid_evidence(evidence))

    def test_rejects_unknown_terminal(self):
        evidence = sample_evidence()
        evidence["records"][0]["terminal"] = "succeeded-ish"
        self.assertFalse(valid_evidence(evidence))

    def test_rejects_malformed_digest(self):
        evidence = sample_evidence()
        evidence["records"][0]["output_sha256"] = "ab" * 32
        self.assertFalse(valid_evidence(evidence))


class ReceiptTests(unittest.TestCase):
    def make_receipt(self, fingerprint):
        from quarry_sweep_impl import receipt

        return receipt(
            "a" * 40,
            "b" * 40,
            fingerprint,
            healthy_observation(),
            healthy_observation(),
            "admit",
            sample_evidence(),
            "sha256:" + "cd" * 32,
            "succeeded",
            0,
            1.5,
            True,
            True,
            7000,
            "sha256:" + "ef" * 32,
            1,
            2,
        )

    def test_accepts_complete_receipt(self):
        fingerprint = command_fingerprint("a" * 40, "b" * 40)
        document = self.make_receipt(fingerprint)
        self.assertTrue(valid_receipt(document, fingerprint))
        raw = canonical_bytes(document) + b"\n"
        self.assertEqual(json.loads(raw), document)

    def test_rejects_wrong_fingerprint(self):
        fingerprint = command_fingerprint("a" * 40, "b" * 40)
        document = self.make_receipt(fingerprint)
        self.assertFalse(valid_receipt(document, command_fingerprint("c" * 40, "b" * 40)))

    def test_rejects_authorizing_flags(self):
        fingerprint = command_fingerprint("a" * 40, "b" * 40)
        document = self.make_receipt(fingerprint)
        document["authorizes_work"] = True
        self.assertFalse(valid_receipt(document, fingerprint))


if __name__ == "__main__":
    unittest.main(verbosity=2)
