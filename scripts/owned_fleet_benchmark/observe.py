from __future__ import annotations

import hashlib, json, os, re, resource, subprocess, tempfile, time
from pathlib import Path
from typing import Any

from .model import FleetError, digest_json

MAX_SEMANTIC_RECEIPT_BYTES = 4 * 1024 * 1024

def run_shell(command: str, root: Path, environment: dict[str, str], timeout: int = 900) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["/bin/bash", "-c", command], cwd=root, env=environment, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout, check=False,
    )


def toolchain_observation(workload: dict[str, Any], root: Path, environment: dict[str, str]) -> dict[str, Any]:
    preflight = []
    for command in workload["toolchain"].get("preflight", []):
        started = time.monotonic()
        with tempfile.NamedTemporaryFile(prefix="glaeda-fleet-env-", delete=False) as env_file:
            env_path = Path(env_file.name)
        try:
            preflight_environment = environment.copy()
            preflight_environment["GITHUB_ENV"] = os.fspath(env_path)
            result = run_shell(command, root, preflight_environment)
            try:
                exported = env_path.read_text(encoding="utf-8")
            except OSError:
                exported = ""
            for line in exported.splitlines():
                if line.startswith("DEVELOPER_DIR="):
                    environment["DEVELOPER_DIR"] = line.split("=", 1)[1]
        finally:
            env_path.unlink(missing_ok=True)
        preflight.append({
            "command": command,
            "exit_code": result.returncode,
            "elapsed_seconds": round(time.monotonic() - started, 6),
            "output_sha256": "sha256:" + hashlib.sha256(result.stdout.encode()).hexdigest(),
        })
        if result.returncode != 0:
            raise FleetError(f"toolchain preflight failed: {command}")
    probes = []
    joined = []
    for command in workload["toolchain"].get("probes", []):
        result = run_shell(command, root, environment, timeout=120)
        output = result.stdout.strip()
        probes.append({
            "command": command,
            "exit_code": result.returncode,
            "output": output[:4096],
            "output_sha256": "sha256:" + hashlib.sha256(result.stdout.encode()).hexdigest(),
            "output_truncated": len(result.stdout.encode()) > 4096,
        })
        joined.append(output)
        if result.returncode != 0:
            raise FleetError(f"toolchain probe failed: {command}")
    required = workload["toolchain"].get("required_output_regex")
    if required and re.search(required, "\n".join(joined), re.MULTILINE) is None:
        raise FleetError("toolchain probe output does not meet the workload's frozen requirement")
    exact = {"policy": workload["toolchain"]["policy"], "probes": probes}
    return {"digest": digest_json(exact), "exact": exact, "preflight": preflight}


def child_cpu_seconds() -> tuple[float, float]:
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime, usage.ru_stime


def validate_semantic(
    kind: str,
    validation: dict[str, Any],
    exit_code: int,
    success_seen: bool,
    semantic_path: Path,
    workload: dict[str, Any],
) -> tuple[bool, str, str | None]:
    if exit_code != 0:
        return False, "command_exit_nonzero", None
    if kind == "output_and_exit":
        if not success_seen:
            return False, "success_oracle_missing", None
        return True, "exit_zero_and_success_oracle", None
    if not semantic_path.is_file():
        return False, "semantic_receipt_missing", None
    try:
        if semantic_path.stat().st_size > MAX_SEMANTIC_RECEIPT_BYTES:
            return False, "semantic_receipt_oversized", None
        raw = semantic_path.read_bytes()
    except OSError:
        return False, "semantic_receipt_unreadable", None
    semantic_digest = "sha256:" + hashlib.sha256(raw).hexdigest()
    try:
        value = json.loads(raw)
    except json.JSONDecodeError:
        return False, "semantic_receipt_malformed", semantic_digest
    if kind == "glaeda_verification_receipt":
        valid = (
            value.get("document_type") == "glaeda-local-verification-receipt"
            and value.get("source", {}).get("commit") == workload["commit"]
            and value.get("source", {}).get("tree") == workload["tree"]
            and value.get("source", {}).get("unchanged") is True
            and value.get("profile") == validation.get("expected_profile")
            and value.get("result", {}).get("exit_code") == 0
        )
        return valid, "glaeda_verification_receipt_accepted" if valid else "glaeda_verification_receipt_rejected", semantic_digest
    if kind == "quarry_parallel_receipt":
        source = value.get("plan", {}).get("key", {}).get("source", {})
        valid = (
            value.get("schema_version") == 2
            and value.get("receipt_kind") == "quarry-parallel-verification-receipt-v2"
            and value.get("result", {}).get("class") == "passed"
            and value.get("result", {}).get("termination_reason") == "completed"
            and value.get("cleanup", {}).get("status") == "passed"
            and value.get("evidence_scope") == "exact_head"
            and value.get("hosted_ci_evidence") is False
            and value.get("merge_authority") is False
            and source.get("commit") == workload["commit"]
            and source.get("tree") == workload["tree"]
            and value.get("verified_head") == workload["commit"]
            and value.get("plan", {}).get("key", {}).get("toolchain_id")
            == workload["toolchain"].get("known_verifier_toolchain_id")
        )
        return valid, "quarry_parallel_receipt_accepted" if valid else "quarry_parallel_receipt_rejected", semantic_digest
    raise FleetError(f"unknown validation kind: {kind}")
