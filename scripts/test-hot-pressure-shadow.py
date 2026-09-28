#!/usr/bin/env python3
"""Contract tests for scripts/hot-pressure-shadow."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SHADOW = ROOT / "scripts" / "hot-pressure-shadow"
KEY = f"sha256:{'1' * 64}"
OTHER_KEY = f"sha256:{'2' * 64}"


def receipt(
    elapsed: float,
    cpu_some: float | None,
    *,
    key: str = KEY,
    exit_code: int = 0,
) -> dict[str, object]:
    pressure = {
        kind: {
            pressure_class: {
                "total_microseconds_delta": (
                    None
                    if kind == "cpu"
                    and pressure_class == "some"
                    and cpu_some is None
                    else 0
                ),
                "stall_fraction_of_command_elapsed": (
                    cpu_some
                    if kind == "cpu" and pressure_class == "some"
                    else (None if cpu_some is None else 0.0)
                ),
            }
            for pressure_class in ("some", "full")
        }
        for kind in ("cpu", "memory", "io")
    }
    return {
        "schema_version": 6,
        "document_type": "glaeda-hot-run-measurement",
        "authority": "developer_observation_only",
        "comparison_key": key,
        "elapsed_seconds": elapsed,
        "preparation_elapsed_seconds": 0.25,
        "command_plus_preparation_elapsed_seconds": elapsed + 0.25,
        "user_cpu_seconds": elapsed * 2,
        "system_cpu_seconds": elapsed / 2,
        "max_rss_kib": 1024,
        "exit_code": exit_code,
        "completion_reason": "exited",
        "machine_observation": {
            "interval": {
                "duration_basis": "command_elapsed",
                "elapsed_seconds": elapsed,
                "memory": {
                    "available_bytes_delta": -1024,
                    "swap_used_bytes_delta": 0,
                },
                "pressure": pressure,
            }
        },
    }


def write(path: Path, value: dict[str, object], *, pretty: bool = False) -> None:
    path.write_text(
        json.dumps(value, indent=2 if pretty else None), encoding="utf-8"
    )


def run_shadow(
    current: Path, baselines: list[Path], output: str = "json"
) -> subprocess.CompletedProcess[str]:
    arguments = [
        sys.executable,
        os.fspath(SHADOW),
        "--output",
        output,
        "--current",
        os.fspath(current),
    ]
    for baseline in baselines:
        arguments.extend(("--baseline", os.fspath(baseline)))
    return subprocess.run(
        arguments,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )


class HotPressureShadowTests(unittest.TestCase):
    def test_zero_and_one_baseline_are_explicitly_insufficient(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = root / "current.json"
            baseline = root / "baseline.json"
            write(current, receipt(8.0, 0.1))
            write(baseline, receipt(4.0, 0.01))

            empty = run_shadow(current, [])
            self.assertEqual(empty.returncode, 0, empty.stderr)
            empty_report = json.loads(empty.stdout)
            self.assertEqual(empty_report["history_state"], "insufficient_history")
            self.assertEqual(empty_report["baseline_samples"], 0)
            self.assertEqual(empty_report["shadow_finding"], "not_evaluated")
            self.assertEqual(
                empty_report["metrics"]["cpu_some_stall_fraction"]["relation"],
                "unknown",
            )

            single = run_shadow(current, [baseline])
            self.assertEqual(single.returncode, 0, single.stderr)
            single_report = json.loads(single.stdout)
            self.assertEqual(single_report["history_state"], "insufficient_history")
            self.assertEqual(single_report["baseline_samples"], 1)
            self.assertEqual(
                single_report["metrics"]["command_elapsed_seconds"][
                    "observed_lower"
                ],
                4.0,
            )
            self.assertEqual(
                single_report["metrics"]["command_elapsed_seconds"]["relation"],
                "unknown",
            )

    def test_exact_history_reports_slower_with_higher_pressure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = root / "current.json"
            first = root / "first.json"
            second = root / "second.json"
            write(current, receipt(8.0, 0.10))
            write(first, receipt(4.0, 0.01))
            write(second, receipt(5.0, 0.02))

            result = run_shadow(current, [first, second])
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["history_state"], "observed_range")
            self.assertEqual(report["shadow_finding"], "slower_with_higher_pressure")
            elapsed = report["metrics"]["command_plus_preparation_elapsed_seconds"]
            self.assertEqual(elapsed["observed_lower"], 4.25)
            self.assertEqual(elapsed["observed_upper"], 5.25)
            self.assertEqual(elapsed["relation"], "above_observed_range")
            cpu = report["metrics"]["cpu_some_stall_fraction"]
            self.assertEqual(cpu["observed_lower"], 0.01)
            self.assertEqual(cpu["observed_upper"], 0.02)
            self.assertEqual(cpu["relation"], "above_observed_range")
            self.assertEqual(report["limits"]["policy_authority"], "none")

    def test_missing_pressure_remains_partial(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = [root / f"sample-{index}.json" for index in range(3)]
            write(paths[0], receipt(8.0, None))
            write(paths[1], receipt(4.0, 0.01))
            write(paths[2], receipt(5.0, 0.02))

            result = run_shadow(paths[0], paths[1:])
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["shadow_finding"], "partial_observation")
            self.assertIsNone(
                report["metrics"]["cpu_some_stall_fraction"]["current"]
            )
            self.assertEqual(
                report["metrics"]["cpu_some_stall_fraction"]["relation"],
                "unknown",
            )

    def test_mixed_failed_and_duplicate_inputs_are_refused_path_free(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = root / "current-private-name.json"
            first = root / "first-private-name.json"
            second = root / "second-private-name.json"
            write(current, receipt(8.0, 0.1))
            write(first, receipt(4.0, 0.01))
            write(second, receipt(5.0, 0.02, key=OTHER_KEY))

            mixed = run_shadow(current, [first, second])
            self.assertEqual(mixed.returncode, 2)
            self.assertIn("comparison keys do not match", mixed.stderr)
            self.assertNotIn(os.fspath(root), mixed.stderr)

            write(second, receipt(5.0, 0.02, exit_code=1))
            failed = run_shadow(current, [first, second])
            self.assertEqual(failed.returncode, 2)
            self.assertIn("cannot enter history", failed.stderr)
            self.assertNotIn(os.fspath(root), failed.stderr)

            write(second, receipt(4.0, 0.01), pretty=True)
            duplicate = run_shadow(current, [first, second])
            self.assertEqual(duplicate.returncode, 2)
            self.assertIn("duplicate measurement", duplicate.stderr)
            self.assertNotIn(os.fspath(root), duplicate.stderr)

    def test_human_and_json_share_the_same_path_free_facts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = root / "current.json"
            first = root / "first.json"
            second = root / "second.json"
            write(current, receipt(4.5, 0.015))
            write(first, receipt(4.0, 0.01))
            write(second, receipt(5.0, 0.02))

            encoded = run_shadow(current, [first, second], "json")
            human = run_shadow(current, [first, second], "human")
            self.assertEqual(encoded.returncode, 0, encoded.stderr)
            self.assertEqual(human.returncode, 0, human.stderr)
            report = json.loads(encoded.stdout)
            self.assertEqual(report["shadow_finding"], "within_or_below_observed_range")
            self.assertIn("finding: within_or_below_observed_range", human.stdout)
            self.assertIn(f"comparison key: {KEY}", human.stdout)
            self.assertIn("cpu_some_stall_fraction: current=0.015", human.stdout)
            self.assertNotIn(os.fspath(root), encoded.stdout)
            self.assertNotIn(os.fspath(root), human.stdout)

    def test_symlink_input_is_refused_without_exposing_the_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real = root / "real.json"
            alias = root / "private-alias.json"
            write(real, receipt(4.0, 0.01))
            alias.symlink_to(real)

            result = run_shadow(alias, [])
            self.assertEqual(result.returncode, 2)
            self.assertIn("regular no-follow file", result.stderr)
            self.assertNotIn(os.fspath(root), result.stderr)

    def test_duplicate_json_keys_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            current = Path(directory) / "current.json"
            encoded = json.dumps(receipt(4.0, 0.01))
            current.write_text(
                encoded[:-1] + ', "schema_version": 6}', encoding="utf-8"
            )

            result = run_shadow(current, [])
            self.assertEqual(result.returncode, 2)
            self.assertIn("duplicate JSON keys", result.stderr)
            self.assertNotIn(directory, result.stderr)


if __name__ == "__main__":
    unittest.main()
