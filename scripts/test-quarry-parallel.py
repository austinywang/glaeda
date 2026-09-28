#!/usr/bin/env python3
"""Unit tests for the Quarry parallel-full experiment adapter.

Pure logic plus real-loop capture semantics with trivial commands only:
no systemd units, no bubblewrap, no Git, no network, no root.
Run with: python3 scripts/test-quarry-parallel.py
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from owned_linux_task import Refusal, execute, execute_capturing, execute_capturing_split  # noqa: E402
from quarry_parallel_impl import (  # noqa: E402
    MAX_RECEIPT_BYTES,
    RECEIPT_SCHEMA_VERSION,
    WORKERS,
    command_fingerprint,
    mirror_venv_binaries,
    receipt as make_receipt,
    settle_producer_output,
    summarize_receipt,
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


def toolchain():
    return {
        "interpreter": "/usr/bin/python3",
        "interpreter_version": "3.14.4",
        "pytest_version": "9.1.1",
        "closure": "/tmp/closure",
        "closure_entries": 10,
        "closure_sha256": "sha256:" + "ab" * 32,
    }


def sample_raw(commit="a" * 40):
    receipt = {
        "schema_version": RECEIPT_SCHEMA_VERSION,
        "verified_head": commit,
        "result": {"termination_reason": "completed", "aggregate_wall_millis": 84000},
        "cleanup": {"status": "passed", "failure_codes": []},
        "outcomes": [
            {"name": "routine", "state": "passed", "output_sha256": "sha256:" + "11" * 32},
            {"name": "alpha", "state": "passed", "output_sha256": "sha256:" + "22" * 32},
        ],
    }
    return (json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n").encode()


class CaptureTests(unittest.TestCase):
    def test_execute_still_returns_its_exact_tuple(self):
        terminal, code, elapsed, settled, out_bytes, digest = execute(
            ["/bin/echo", "hello"],
            unit="quarry-parallel-test-absent.service",
            deadline_seconds=30,
            label="capture-test",
        )
        self.assertEqual((terminal, code, out_bytes), ("succeeded", 0, 6))
        self.assertRegex(digest, r"^sha256:[a-f0-9]{64}$")
        self.assertTrue(settled)
        self.assertGreaterEqual(elapsed, 0)

    def test_capturing_retains_exact_prefix(self):
        terminal, code, _, settled, out_bytes, digest, captured, overflow = (
            execute_capturing(
                ["/bin/echo", "hello"],
                unit="quarry-parallel-test-absent.service",
                deadline_seconds=30,
                label="capture-test",
                max_bytes=64,
            )
        )
        self.assertEqual(terminal, "succeeded")
        self.assertEqual(code, 0)
        self.assertEqual(captured, b"hello\n")
        self.assertFalse(overflow)
        self.assertEqual(out_bytes, 6)

    def test_capturing_flags_overflow_and_keeps_digest_honest(self):
        terminal, _, _, _, out_bytes, digest, captured, overflow = execute_capturing(
            ["/bin/echo", "hello"],
            unit="quarry-parallel-test-absent.service",
            deadline_seconds=30,
            label="capture-test",
            max_bytes=3,
        )
        self.assertEqual(terminal, "succeeded")
        self.assertEqual(captured, b"hel")
        self.assertTrue(overflow)
        self.assertEqual(out_bytes, 6)
        import hashlib

        self.assertEqual(digest, "sha256:" + hashlib.sha256(b"hello\n").hexdigest())

    def test_capturing_rejects_non_positive_ceiling(self):
        with self.assertRaises(Refusal):
            execute_capturing(
                ["/bin/echo", "hello"],
                unit="quarry-parallel-test-absent.service",
                deadline_seconds=30,
                label="capture-test",
                max_bytes=0,
            )

    def test_split_keeps_stderr_out_of_the_receipt_channel(self):
        terminal, code, _, _, out_bytes, _, captured, overflow, err_bytes, err_digest = (
            execute_capturing_split(
                ["/bin/bash", "-c", "echo out; echo err >&2"],
                unit="quarry-parallel-test-absent.service",
                deadline_seconds=30,
                label="capture-test",
                max_bytes=64,
            )
        )
        self.assertEqual(terminal, "succeeded")
        self.assertEqual(code, 0)
        self.assertEqual(captured, b"out\n")
        self.assertFalse(overflow)
        self.assertEqual(out_bytes, 4)
        self.assertEqual(err_bytes, 4)
        import hashlib

        self.assertEqual(err_digest, "sha256:" + hashlib.sha256(b"err\n").hexdigest())

    def test_split_preserves_default_merged_behavior_untouched(self):
        terminal, _, _, _, out_bytes, _, captured, _ = execute_capturing(
            ["/bin/bash", "-c", "echo out; echo err >&2"],
            unit="quarry-parallel-test-absent.service",
            deadline_seconds=30,
            label="capture-test",
            max_bytes=64,
        )
        self.assertEqual(terminal, "succeeded")
        self.assertEqual(out_bytes, 8)
        self.assertIn(b"err\n", captured)


class ReceiptChannelTests(unittest.TestCase):
    def test_accepts_exact_receipt_line(self):
        summary = summarize_receipt(sample_raw(), "a" * 40, "b" * 40, "test")
        self.assertEqual(summary["termination_reason"], "completed")
        self.assertEqual(summary["cleanup_status"], "passed")
        self.assertEqual(summary["receipt_bytes"], len(sample_raw()))
        self.assertEqual(
            summary["outcomes"],
            [
                {"name": "routine", "state": "passed", "output_sha256": "sha256:" + "11" * 32},
                {"name": "alpha", "state": "passed", "output_sha256": "sha256:" + "22" * 32},
            ],
        )

    def test_rejects_empty_channel(self):
        with self.assertRaises(Refusal):
            summarize_receipt(b"", "a" * 40, "b" * 40, "test")

    def test_rejects_prefixed_framing(self):
        with self.assertRaises(Refusal):
            summarize_receipt(b"parallel_receipt=" + sample_raw().strip(), "a" * 40, "b" * 40, "test")

    def test_rejects_multi_line_output(self):
        with self.assertRaises(Refusal):
            summarize_receipt(sample_raw() + sample_raw(), "a" * 40, "b" * 40, "test")

    def test_rejects_wrong_head(self):
        with self.assertRaises(Refusal):
            summarize_receipt(sample_raw("b" * 40), "a" * 40, "b" * 40, "test")

    def test_accepts_failure_receipt_bound_by_plan_key(self):
        value = json.loads(sample_raw().decode())
        value["verified_head"] = None
        value["plan"] = {
            "key": {
                "source": {"commit": "a" * 40, "tree": "b" * 40},
                "scheduler": {"workers": 4},
            }
        }
        raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
        summary = summarize_receipt(raw, "a" * 40, "b" * 40, "test")
        self.assertEqual(summary["termination_reason"], "completed")

    def test_rejects_failure_receipt_with_wrong_plan_tree(self):
        value = json.loads(sample_raw().decode())
        value["verified_head"] = None
        value["plan"] = {
            "key": {
                "source": {"commit": "a" * 40, "tree": "c" * 40},
                "scheduler": {"workers": 4},
            }
        }
        raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
        with self.assertRaises(Refusal):
            summarize_receipt(raw, "a" * 40, "b" * 40, "test")

    def test_rejects_oversized_channel(self):
        with self.assertRaises(Refusal):
            summarize_receipt(b"x" * (MAX_RECEIPT_BYTES + 1), "a" * 40, "b" * 40, "test")

    def test_missing_channel_downgrades_success(self):
        terminal, code, summary, digest = settle_producer_output(
            "succeeded", 0, b"", "a" * 40, "b" * 40, "test"
        )
        self.assertEqual((terminal, code, summary, digest), ("failed", 99, None, None))

    def test_failed_process_keeps_terminal(self):
        terminal, code, summary, digest = settle_producer_output(
            "failed", 3, b"", "a" * 40, "b" * 40, "test"
        )
        self.assertEqual(terminal, "failed")
        self.assertEqual(code, 3)
        self.assertIsNone(summary)


class WorkloadTests(unittest.TestCase):
    def test_identity_pins_workers_and_command(self):
        identity = workload_identity("a" * 40, "b" * 40, toolchain())
        self.assertEqual(identity["workers"], WORKERS)
        self.assertIn("--receipt-stdout", identity["command"])
        self.assertEqual(identity["toolchain"], toolchain())

    def test_fingerprint_moves_with_toolchain(self):
        first = command_fingerprint("a" * 40, "b" * 40, toolchain())
        altered = dict(toolchain(), pytest_version="9.1.2")
        self.assertNotEqual(
            first, command_fingerprint("a" * 40, "b" * 40, altered)
        )
        self.assertRegex(first, r"^sha256:[a-f0-9]{64}$")


class OuterReceiptTests(unittest.TestCase):
    def make(self, fingerprint):
        return make_receipt(
            "a" * 40,
            "b" * 40,
            fingerprint,
            toolchain(),
            healthy_observation(),
            healthy_observation(),
            "admit",
            {"termination_reason": "completed"},
            "sha256:" + "cd" * 32,
            "succeeded",
            0,
            84.0,
            True,
            True,
            8049,
            "sha256:" + "ef" * 32,
            100,
            "sha256:" + "aa" * 32,
            1,
            2,
        )

    def test_accepts_complete_receipt(self):
        fingerprint = command_fingerprint("a" * 40, "b" * 40, toolchain())
        self.assertTrue(valid_receipt(self.make(fingerprint), fingerprint))

    def test_rejects_wrong_fingerprint(self):
        fingerprint = command_fingerprint("a" * 40, "b" * 40, toolchain())
        self.assertFalse(valid_receipt(self.make(fingerprint), "sha256:" + "00" * 32))


class SkeletonBinaryTests(unittest.TestCase):
    def test_mirror_rewrites_resident_shebangs_and_links_natives(self):
        import os
        import stat
        import tempfile

        with tempfile.TemporaryDirectory(prefix="quarry-skel-test-") as tmp:
            root = Path(tmp)
            source = root / "source-bin"
            source.mkdir(parents=True)
            resident_python = str(root / "bin" / "python")
            (source / "ruff").write_bytes(b"\x7fELFfake-native-binary")
            (source / "ruff").chmod(0o755)
            (source / "pytest").write_bytes(
                f"#!{resident_python}\nimport sys\n".encode()
            )
            (source / "quarry").write_bytes(
                f"#!{resident_python} -E\nimport sys\n".encode()
            )
            (source / "system-tool").symlink_to("/bin/true")
            (source / "python3").symlink_to("/usr/bin/python3")
            (source / "Activate.ps1").write_text("echo hi")
            (source / "activate").write_text("echo hi")
            venv = root / "venv"
            (venv / "bin").mkdir(parents=True)
            mirrored = mirror_venv_binaries(venv, source)
            self.assertEqual(mirrored, ["pytest", "quarry", "ruff", "system-tool"])
            ruff = venv / "bin" / "ruff"
            # Inside the resident venv: copied, never symlinked (a symlink
            # would dangle inside the task boundary).
            self.assertFalse(ruff.is_symlink())
            self.assertEqual(ruff.read_bytes(), b"\x7fELFfake-native-binary")
            self.assertTrue(os.access(ruff, os.X_OK))
            pytest = venv / "bin" / "pytest"
            self.assertFalse(pytest.is_symlink())
            first = pytest.read_bytes().split(b"\n", 1)[0]
            self.assertEqual(first, b"#!/venv/bin/python")
            self.assertTrue(pytest.stat().st_mode & stat.S_IXUSR)
            quarry = venv / "bin" / "quarry"
            first = quarry.read_bytes().split(b"\n", 1)[0]
            self.assertEqual(first, b"#!/venv/bin/python -E")
            system = venv / "bin" / "system-tool"
            # Outside the venv (system path, visible in the boundary): linked.
            self.assertTrue(system.is_symlink())
            self.assertEqual(system.resolve(), Path("/bin/true").resolve())
            self.assertFalse((venv / "bin" / "python3").exists())
            self.assertFalse((venv / "bin" / "Activate.ps1").exists())
            self.assertFalse((venv / "bin" / "activate").exists())
            self.assertTrue(os.access(venv / "bin" / "pytest", os.X_OK))


if __name__ == "__main__":
    unittest.main(verbosity=2)
