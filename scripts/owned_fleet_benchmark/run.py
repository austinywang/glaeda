from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import signal
import subprocess
import threading
import time
import uuid
from pathlib import Path

from .model import (
    FleetError,
    digest_json,
    env_for,
    find_workload,
    load_json,
    machine_comparison_digest,
    platform_allowed,
    render,
    validate_machine,
    validate_state_evidence,
    verify_source,
    write_json,
)
from .observe import child_cpu_seconds, toolchain_observation, validate_semantic
from .resource import (
    Sampler as ResourceSampler,
    du_bytes,
    filesystem_type,
    swap_used_bytes,
)

RUN_SCHEMA_VERSION = 1
MAX_CAPTURE_BYTES = 64 * 1024 * 1024
TOKEN_RE = re.compile(r"^[a-z0-9][a-z0-9._:-]{0,95}$")


def _token(value: str, field: str) -> str:
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise FleetError(f"{field} must be a bounded lowercase token")
    return value


def _nonnegative(value: float | int, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise FleetError(f"{field} must be non-negative")
    return float(value)


def _path_is_within(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def _validate_direct_runtime(machine: dict, backend_id: str) -> None:
    system = platform.system().lower()
    if system == "darwin":
        expected_backend = "native-macos"
        expected_machine_os = {"macos", "darwin"}
    elif system == "linux":
        expected_backend = "native-linux"
        expected_machine_os = {"linux"}
    else:
        raise FleetError(f"direct benchmark execution is unsupported on {system or 'unknown'}")

    if backend_id != expected_backend:
        raise FleetError(
            f"the direct runner can mint only {expected_backend} on this runtime; "
            "guest/container/remote backends require a separately reviewed adapter"
        )

    machine_platform = machine.get("platform") or {}
    machine_os = str(machine_platform.get("os", "")).lower()
    if machine_os not in expected_machine_os:
        raise FleetError(
            "machine receipt OS class does not match the direct execution runtime"
        )

    machine_kernel = machine_platform.get("kernel")
    if machine_kernel != platform.release():
        raise FleetError(
            "machine receipt kernel does not match the direct execution runtime"
        )

    expected_cpus = (machine.get("cpu") or {}).get("topology", {}).get("logical_cpus")
    observed_cpus = os.cpu_count()
    if (
        isinstance(expected_cpus, int)
        and isinstance(observed_cpus, int)
        and expected_cpus != observed_cpus
    ):
        raise FleetError(
            "machine receipt logical CPU count does not match the direct execution runtime"
        )


def _runtime_observation(backend_id: str, repo_root: Path, state_dir: Path) -> dict:
    os_release_digest = None
    os_release = Path("/etc/os-release")
    if os_release.is_file():
        try:
            os_release_digest = (
                "sha256:" + hashlib.sha256(os_release.read_bytes()).hexdigest()
            )
        except OSError:
            os_release_digest = None
    exact = {
        "backend_id": backend_id,
        "system": platform.system().lower(),
        "kernel_release": platform.release(),
        "architecture": platform.machine().lower(),
        "os_release_digest": os_release_digest,
    }
    return {
        "digest": digest_json(exact),
        "exact": exact,
        "source_filesystem": filesystem_type(repo_root),
        "state_filesystem": filesystem_type(state_dir),
    }


def _result_class(
    *,
    validated: bool,
    semantic_valid: bool,
    source_unchanged: bool,
    exit_code: int,
    timed_out: bool,
) -> str:
    if validated:
        return "validated"
    if timed_out:
        return "timed_out"
    if exit_code != 0:
        return "command_failed"
    if not source_unchanged:
        return "source_changed"
    if not semantic_valid:
        return "semantic_mismatch"
    return "unknown"


def run_benchmark(args: argparse.Namespace) -> int:
    workload, operation, variant = find_workload(args.workload, args.variant)
    machine = validate_machine(load_json(args.machine), require_complete=True)
    state = validate_state_evidence(load_json(args.state_evidence))
    repo_root = args.repo_root.resolve()
    state_dir = args.state_dir.resolve()
    output = args.output.resolve()
    backend_id = _token(args.backend_id, "backend_id")
    _validate_direct_runtime(machine, backend_id)
    resource_policy_id = _token(args.resource_policy_id, "resource_policy_id")
    source_storage_id = _token(args.source_storage_id, "source_storage_id")
    state_storage_id = _token(args.state_storage_id, "state_storage_id")
    resource_policy_status = args.resource_policy_status
    resource_policy_evidence_id = args.resource_policy_evidence_id
    if resource_policy_status == "enforced":
        raise FleetError(
            "the direct benchmark runner cannot mint enforced resource-policy evidence; "
            "run through a separately reviewed enforcement adapter and reduce its receipts"
        )
    if resource_policy_evidence_id is not None:
        raise FleetError(
            "the direct runner is diagnostic-only and cannot claim a resource-policy evidence ID"
        )
    if args.timeout_seconds <= 0:
        raise FleetError("timeout_seconds must be positive")
    if args.cpu_millis is not None and args.cpu_millis <= 0:
        raise FleetError("cpu_millis must be positive when supplied")
    if args.memory_limit_bytes is not None and args.memory_limit_bytes <= 0:
        raise FleetError("memory_limit_bytes must be positive when supplied")
    if (args.cpu_millis is None) != (args.memory_limit_bytes is None):
        raise FleetError(
            "cpu_millis and memory_limit_bytes must be supplied together or both omitted"
        )

    if resource_policy_status == "enforced" and args.cpu_millis is None:
        raise FleetError(
            "enforced resource policy requires explicit CPU and memory limits"
        )

    if not repo_root.is_dir():
        raise FleetError("repository root does not exist")
    for candidate, label in ((state_dir, "state_dir"), (output, "output")):
        if _path_is_within(candidate, repo_root):
            raise FleetError(f"{label} must be outside the exact source worktree")

    semantic_receipt = output.with_suffix(output.suffix + ".semantic.json")
    log_path = output.with_suffix(output.suffix + ".log")
    for candidate, label in (
        (output, "output"),
        (semantic_receipt, "semantic receipt"),
        (log_path, "log"),
    ):
        if _path_is_within(candidate, state_dir):
            raise FleetError(
                f"{label} must stay outside state_dir so receipt/log bytes "
                "cannot contaminate storage-growth measurements"
            )

    if not platform_allowed(workload["platform"]):
        raise FleetError(
            f"workload {workload['id']} requires platform {workload['platform']}"
        )

    output.parent.mkdir(parents=True, exist_ok=True)

    request_known_ns = time.monotonic_ns()
    request_known_unix_ms = time.time_ns() // 1_000_000
    verify_source(repo_root, workload)
    state_dir.mkdir(parents=True, exist_ok=True)
    environment = env_for(workload, state_dir)
    toolchain = toolchain_observation(workload, repo_root, environment)
    command = render(operation["command"], state_dir, semantic_receipt)

    # A prior successful sample must never authorize a later sample. Repository
    # semantic receipts are run-private evidence and begin absent.
    semantic_receipt.unlink(missing_ok=True)

    disk_before = du_bytes(state_dir)
    swap_before = swap_used_bytes()
    user_before, sys_before = child_cpu_seconds()
    first_pattern = re.compile(operation.get("first_useful_regex", r"$^"))
    validation = operation["validation"]
    success_pattern = (
        re.compile(validation.get("success_regex", r"$^"))
        if validation["kind"] == "output_and_exit"
        else None
    )
    first_useful_ns: int | None = None
    success_seen = False
    hasher = hashlib.sha256()
    output_bytes = 0
    captured_bytes = 0

    command_start_ns = time.monotonic_ns()
    command_started_unix_ms = time.time_ns() // 1_000_000
    proc = subprocess.Popen(
        ["/bin/bash", "-c", command],
        cwd=repo_root,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=False,
        start_new_session=True,
    )
    sampler = ResourceSampler(proc.pid)
    sampler.start()
    timed_out = threading.Event()

    def terminate_for_timeout() -> None:
        if proc.poll() is not None:
            return
        timed_out.set()
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    deadline = threading.Timer(args.timeout_seconds, terminate_for_timeout)
    deadline.daemon = True
    deadline.start()
    try:
        with log_path.open("wb") as log:
            assert proc.stdout is not None
            while True:
                chunk = proc.stdout.readline()
                if not chunk:
                    break
                hasher.update(chunk)
                output_bytes += len(chunk)
                if captured_bytes < MAX_CAPTURE_BYTES:
                    kept = chunk[: MAX_CAPTURE_BYTES - captured_bytes]
                    log.write(kept)
                    captured_bytes += len(kept)
                decoded = chunk.decode("utf-8", errors="replace")
                now = time.monotonic_ns()
                if first_useful_ns is None and first_pattern.search(decoded):
                    first_useful_ns = now
                if success_pattern is not None and success_pattern.search(decoded):
                    success_seen = True

    finally:
        deadline.cancel()
    exit_code = proc.wait()
    command_exit_ns = time.monotonic_ns()
    sampler.stop()
    user_after, sys_after = child_cpu_seconds()
    swap_after = swap_used_bytes()
    disk_after = du_bytes(state_dir)

    source_unchanged = False
    try:
        verify_source(repo_root, workload)
        source_unchanged = True
    except FleetError:
        source_unchanged = False

    semantic_valid, validation_reason, semantic_digest = validate_semantic(
        validation["kind"],
        validation,
        exit_code,
        success_seen,
        semantic_receipt,
        workload,
    )
    validated = semantic_valid and source_unchanged
    result_class = _result_class(
        validated=validated,
        semantic_valid=semantic_valid,
        source_unchanged=source_unchanged,
        exit_code=exit_code,
        timed_out=timed_out.is_set(),
    )
    if result_class == "source_changed":
        validation_reason = "source_changed_after_execution"
    final_result_ns = time.monotonic_ns()
    final_result_unix_ms = time.time_ns() // 1_000_000
    if first_useful_ns is None and validated:
        first_useful_ns = final_result_ns

    command_wall_s = (command_exit_ns - command_start_ns) / 1_000_000_000
    cpu_user = max(0.0, user_after - user_before)
    cpu_system = max(0.0, sys_after - sys_before)
    cpu_total = cpu_user + cpu_system

    load_watts = args.load_watts
    if load_watts is None:
        load_watts = (machine.get("power") or {}).get("load_watts")
    if load_watts is not None:
        if isinstance(load_watts, bool) or not isinstance(load_watts, (int, float)):
            raise FleetError("load_watts must be numeric")
        if load_watts <= 0:
            raise FleetError("load_watts must be positive")
    execution_wh = (
        float(load_watts) * command_wall_s / 3600
        if isinstance(load_watts, (int, float))
        else None
    )

    runtime = _runtime_observation(backend_id, repo_root, state_dir)
    machine_digest = machine_comparison_digest(machine)
    receipt = {
        "schema_version": RUN_SCHEMA_VERSION,
        "document_type": "glaeda-owned-fleet-benchmark-receipt",
        "authority": "performance_observation_only",
        "sample_id": f"sample-{uuid.uuid4().hex}",
        "machine_id": machine["machine_id"],
        "machine_receipt_digest": digest_json(machine),
        "machine_comparison_digest": machine_digest,
        "workload": {
            "id": workload["id"],
            "label": workload["label"],
            "variant": variant,
            "repository": workload["repository"],
            "commit": workload["commit"],
            "tree": workload["tree"],
            "semantic_profile": operation["semantic_profile"],
            "operation_digest": digest_json(
                {
                    "command": operation["command"],
                    "validation": validation,
                    "semantic_profile": operation["semantic_profile"],
                }
            ),
        },
        "state": {
            "class": state["state_class"],
            "preparation_id": state["preparation_id"],
            "evidence_digest": digest_json(state),
        },
        "storage": {
            "source_tier": args.source_storage_tier,
            "source_id": source_storage_id,
            "state_tier": args.state_storage_tier,
            "state_id": state_storage_id,
            "source_filesystem": runtime["source_filesystem"],
            "state_filesystem": runtime["state_filesystem"],
        },
        "toolchain": toolchain,
        "execution": {
            "backend_id": backend_id,
            "runtime": runtime,
            "resource_policy_id": resource_policy_id,
            "resource_policy_status": resource_policy_status,
            "resource_policy_evidence_id": resource_policy_evidence_id,
            "declared_cpu_millis": args.cpu_millis,
            "declared_memory_limit_bytes": args.memory_limit_bytes,
            "timeout_seconds": float(args.timeout_seconds),
        },
        "resource_envelope": workload["resource_envelope"],
        "timing": {
            "request_known_unix_millis": request_known_unix_ms,
            "command_started_unix_millis": command_started_unix_ms,
            "final_result_unix_millis": final_result_unix_ms,
            "request_known_monotonic_ns": request_known_ns,
            "command_start_monotonic_ns": command_start_ns,
            "command_exit_monotonic_ns": command_exit_ns,
            "final_result_monotonic_ns": final_result_ns,
        },
        "milestones": {
            "request_known_to_command_start_ms": round(
                (command_start_ns - request_known_ns) / 1_000_000, 3
            ),
            "command_start_to_first_useful_result_ms": (
                round((first_useful_ns - command_start_ns) / 1_000_000, 3)
                if first_useful_ns is not None
                else None
            ),
            "command_start_to_command_exit_ms": round(
                (command_exit_ns - command_start_ns) / 1_000_000, 3
            ),
            "request_known_to_final_result_ms": round(
                (final_result_ns - request_known_ns) / 1_000_000, 3
            ),
        },
        "result": {
            "class": result_class,
            "validated": validated,
            "exit_code": exit_code,
            "semantic_validation": validation_reason,
            "source_unchanged": source_unchanged,
            "failure_count": (
                1 if result_class in {"command_failed", "timed_out"} else 0
            ),
            "timed_out": timed_out.is_set(),
        },
        "resources": {
            "child_user_cpu_seconds": round(cpu_user, 6),
            "child_system_cpu_seconds": round(cpu_system, 6),
            "aggregate_cpu_utilization_percent": (
                round(cpu_total / command_wall_s * 100, 2)
                if command_wall_s > 0
                else None
            ),
            "peak_aggregate_rss_kib": sampler.peak_rss_kib,
            "swap_used_start_bytes": swap_before,
            "swap_used_max_observed_bytes": sampler.swap_max_bytes,
            "swap_used_end_bytes": swap_after,
            "memory_psi_some_avg10_max_observed": sampler.psi_some_avg10_max,
            "state_bytes_start": disk_before,
            "state_bytes_end": disk_after,
            "state_growth_bytes": disk_after - disk_before,
            "max_temperature_c_observed": sampler.max_temperature_c,
            "load_watts": load_watts,
            "estimated_execution_watt_hours": (
                round(execution_wh, 6) if execution_wh is not None else None
            ),
        },
        # The direct runner owns no upstream scheduler/controller lifecycle.
        # Queue begins at its own request-known boundary and it cannot observe
        # fallback or reset events outside this process.
        "queue_delay_ms": 0.0,
        "events": {
            "fallback_count": 0,
            "reset_count": 0,
        },
        "artifacts": {
            "log_sha256": "sha256:" + hasher.hexdigest(),
            "log_bytes": output_bytes,
            "log_capture_truncated": output_bytes > captured_bytes,
            "semantic_receipt_sha256": semantic_digest,
        },
    }
    write_json(output, receipt)
    print(
        json.dumps(
            {
                "receipt": os.fspath(output),
                "validated": validated,
                "workload": workload["id"],
                "variant": variant,
                "backend_id": backend_id,
                "final_ms": receipt["milestones"]["request_known_to_final_result_ms"],
            },
            sort_keys=True,
        )
    )
    return 0 if validated else 3
