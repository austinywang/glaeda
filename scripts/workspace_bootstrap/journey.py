"""One-command, read-only Glaeda checkout readiness journey."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path

from .cli import blocked_internal_receipt
from .probe import child_environment
from .receipt import (
    RECEIPT_TYPE as WORKSPACE_RECEIPT_TYPE,
    SCHEMA_VERSION as WORKSPACE_SCHEMA_VERSION,
    build_receipt,
)

SCHEMA_VERSION = 2
RECEIPT_TYPE = "glaeda-front-door-readiness-receipt"
MAX_CHILD_OUTPUT_BYTES = 1024 * 1024
DOCTOR_TIMEOUT_SECONDS = 180
VALID_BOOTSTRAP_STATES = {"ready", "ready_with_declared_deviations", "blocked"}
VALID_DOCTOR_STATUSES = {"pass", "warn", "fail"}


class SafeArgumentParser(argparse.ArgumentParser):
    def error(self, _message: str) -> None:
        self.print_usage(sys.stderr)
        self.exit(2, "glaeda arguments are invalid\n")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = SafeArgumentParser(
        prog="./glaeda",
        description="Evaluate the checkout and host through one Glaeda readiness journey.",
    )
    parser.add_argument("command", nargs="?", choices=["doctor"])
    parser.add_argument("--output", choices=["human", "json"], default="human")
    return parser.parse_args(argv)


@dataclass(frozen=True)
class ChildResult:
    returncode: int | None
    stdout: bytes
    failure: str | None


def _terminate(process: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        try:
            process.kill()
        except OSError:
            pass


def _bounded_reader(
    stream: object,
    buffer: bytearray,
    exceeded: threading.Event,
) -> None:
    read = getattr(stream, "read")
    while True:
        chunk = read(65536)
        if not chunk:
            return
        remaining = MAX_CHILD_OUTPUT_BYTES + 1 - len(buffer)
        if remaining > 0:
            buffer.extend(chunk[:remaining])
        if len(buffer) > MAX_CHILD_OUTPUT_BYTES:
            exceeded.set()
            return


def run_bounded(
    argv: list[str],
    *,
    cwd: Path,
    environment: dict[str, str],
) -> ChildResult:
    program = shutil.which(argv[0], path=environment.get("PATH"))
    if program is None:
        return ChildResult(None, b"", "program_unavailable")
    try:
        process = subprocess.Popen(
            [os.path.abspath(program), *argv[1:]],
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError:
        return ChildResult(None, b"", "spawn_failed")

    assert process.stdout is not None
    assert process.stderr is not None
    stdout = bytearray()
    stderr = bytearray()
    exceeded = threading.Event()
    readers = [
        threading.Thread(
            target=_bounded_reader,
            args=(process.stdout, stdout, exceeded),
            daemon=True,
        ),
        threading.Thread(
            target=_bounded_reader,
            args=(process.stderr, stderr, exceeded),
            daemon=True,
        ),
    ]
    for reader in readers:
        reader.start()

    deadline = time.monotonic() + DOCTOR_TIMEOUT_SECONDS
    failure = None
    while process.poll() is None:
        if exceeded.is_set():
            failure = "output_limit_exceeded"
            _terminate(process)
            break
        if time.monotonic() >= deadline:
            failure = "timeout"
            _terminate(process)
            break
        time.sleep(0.02)

    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        _terminate(process)
        process.wait()
    for reader in readers:
        reader.join(timeout=2)
    process.stdout.close()
    process.stderr.close()

    if exceeded.is_set():
        failure = "output_limit_exceeded"
    return ChildResult(process.returncode, bytes(stdout), failure)


def _bootstrap_receipt() -> dict[str, object]:
    try:
        receipt = build_receipt("verify")
    except Exception:
        receipt = blocked_internal_receipt("verify")
    return receipt if _valid_bootstrap_receipt(receipt) else blocked_internal_receipt("verify")


def _valid_bootstrap_receipt(receipt: object) -> bool:
    if not isinstance(receipt, dict):
        return False
    if receipt.get("schema_version") != WORKSPACE_SCHEMA_VERSION:
        return False
    if receipt.get("receipt_type") != WORKSPACE_RECEIPT_TYPE:
        return False
    if receipt.get("state") not in VALID_BOOTSTRAP_STATES:
        return False
    source = receipt.get("source")
    return isinstance(source, dict)


def _doctor_environment() -> dict[str, str]:
    environment = child_environment()
    target = os.environ.get("CARGO_TARGET_DIR")
    if target:
        environment["CARGO_TARGET_DIR"] = target
    return environment


def _run_doctor(root: Path) -> tuple[dict[str, object] | None, str | None]:
    result = run_bounded(
        [
            "cargo",
            "run",
            "--locked",
            "--offline",
            "--quiet",
            "--",
            "--output",
            "json",
            "doctor",
        ],
        cwd=root,
        environment=_doctor_environment(),
    )
    if result.failure is not None:
        return None, f"doctor_{result.failure}"
    if len(result.stdout) > MAX_CHILD_OUTPUT_BYTES:
        return None, "doctor_output_limit_exceeded"
    try:
        document = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError):
        return None, "doctor_invalid_receipt"
    if not _valid_doctor_receipt(document):
        return None, "doctor_invalid_receipt"
    if result.returncode not in (0, 1):
        return None, "doctor_execution_failed"
    return document, None


def _valid_doctor_receipt(document: object) -> bool:
    if not isinstance(document, dict):
        return False
    if document.get("schema_version") != 1:
        return False
    if document.get("overall") not in VALID_DOCTOR_STATUSES:
        return False
    checks = document.get("checks")
    if not isinstance(checks, list):
        return False
    for check in checks:
        if not isinstance(check, dict):
            return False
        if not isinstance(check.get("id"), str) or check.get("status") not in VALID_DOCTOR_STATUSES:
            return False
    return True


def _issue_codes(receipt: dict[str, object], key: str) -> list[str]:
    entries = receipt.get(key)
    if not isinstance(entries, list):
        return ["invalid_bootstrap_receipt"]
    result = []
    for entry in entries:
        if isinstance(entry, dict) and isinstance(entry.get("code"), str):
            result.append(str(entry["code"]))
    return sorted(set(result))


def _source_identity(receipt: dict[str, object]) -> tuple[object, object, object, object, object]:
    source = receipt.get("source")
    if not isinstance(source, dict):
        return (None, None, None, None, None)
    return (
        source.get("commit"),
        source.get("tree"),
        source.get("clean_before"),
        source.get("clean_after"),
        source.get("cleanliness_unchanged"),
    )


def build_journey_report(
    before: dict[str, object],
    doctor: dict[str, object] | None,
    doctor_failure: str | None,
    after: dict[str, object] | None,
) -> dict[str, object]:
    bootstrap_state = str(before.get("state"))
    source = before.get("source") if isinstance(before.get("source"), dict) else {}
    deviations = _issue_codes(before, "deviations")
    blockers = _issue_codes(before, "blocking_reasons")
    doctor_overall = doctor.get("overall") if doctor is not None else None

    if bootstrap_state == "blocked":
        verdict = "blocked"
        next_action = "rerun_after_external_action"
        if not blockers:
            blockers = ["bootstrap_blocked"]
    elif doctor_failure is not None:
        verdict = "blocked"
        next_action = "rerun_after_external_action"
        blockers = sorted(set([*blockers, doctor_failure]))
    elif doctor is None:
        verdict = "blocked"
        next_action = "rerun_after_external_action"
        blockers = sorted(set([*blockers, "doctor_unavailable"]))
    elif doctor_overall == "fail":
        verdict = "blocked"
        next_action = "rerun_after_external_action"
        blockers = sorted(set([*blockers, "doctor_reported_failure"]))
    else:
        verdict = "ready"
        next_action = "none"

    if after is not None and _source_identity(before) != _source_identity(after):
        verdict = "blocked"
        next_action = "rerun_after_external_action"
        blockers = sorted(set([*blockers, "checkout_changed_during_journey"]))
    elif after is not None and after.get("state") == "blocked":
        verdict = "blocked"
        next_action = "rerun_after_external_action"
        blockers = sorted(set([*blockers, "checkout_changed_during_journey"]))

    return {
        "schema_version": SCHEMA_VERSION,
        "receipt_type": RECEIPT_TYPE,
        "verdict": verdict,
        "source": {
            "commit": source.get("commit"),
            "tree": source.get("tree"),
        },
        "bootstrap": {
            "state": bootstrap_state,
            "capability_fingerprint": before.get("capability_fingerprint"),
        },
        "doctor": {
            "evaluated": doctor is not None,
            "overall": doctor_overall,
        },
        "deviation_codes": deviations,
        "blocking_codes": blockers,
        "repair_classes": [],
        "next_action": next_action,
    }


def render_human(report: dict[str, object]) -> str:
    verdict = str(report["verdict"]).upper()
    doctor = report["doctor"]
    bootstrap = report["bootstrap"]
    assert isinstance(doctor, dict)
    assert isinstance(bootstrap, dict)
    lines = [
        f"Glaeda: {verdict}",
        f"Bootstrap: {bootstrap['state']}",
        f"Doctor: {doctor['overall'] if doctor['evaluated'] else 'not evaluated'}",
    ]
    blockers = report["blocking_codes"]
    deviations = report["deviation_codes"]
    assert isinstance(blockers, list)
    assert isinstance(deviations, list)
    if deviations:
        lines.append("Declared deviations: " + ", ".join(str(item) for item in deviations))
    if blockers:
        lines.append("Blockers: " + ", ".join(str(item) for item in blockers))
    if report["next_action"] == "none":
        lines.append("Next: ready for Glaeda work.")
    else:
        lines.append("Next: resolve the blocker above, then rerun ./glaeda.")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    parsed = parse_args(argv)
    before = _bootstrap_receipt()
    doctor = None
    doctor_failure = None
    after = None
    if before.get("state") != "blocked":
        root = Path(__file__).resolve().parents[2]
        doctor, doctor_failure = _run_doctor(root)
        after = _bootstrap_receipt()
    report = build_journey_report(before, doctor, doctor_failure, after)
    if parsed.output == "json":
        print(json.dumps(report, sort_keys=True, indent=2))
    else:
        sys.stdout.write(render_human(report))
    return 0 if report["verdict"] == "ready" else 1
