from __future__ import annotations

import hashlib
import json
import os
import re
import shlex
import subprocess
import sys
from datetime import date
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CATALOG_PATH = ROOT / "benchmarks" / "fleet" / "workloads.v1.json"
STATE_SCHEMA_VERSION = 1
OPAQUE_TOKEN_RE = re.compile(r"^[a-z0-9][a-z0-9._:-]{0,95}$")


class FleetError(RuntimeError):
    pass


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def digest_json(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical_bytes(value)).hexdigest()


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise FleetError(f"cannot read JSON {path}: {exc}") from exc


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(tmp, path)


def _opaque_token(value: Any, field: str) -> str:
    if not isinstance(value, str) or OPAQUE_TOKEN_RE.fullmatch(value) is None:
        raise FleetError(f"{field} must be a bounded lowercase opaque token")
    return value


def _nonempty_text(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise FleetError(f"{field} must be a non-empty string")
    return value


def _positive_number(value: Any, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value <= 0:
        raise FleetError(f"{field} must be positive")
    return float(value)


def _positive_integer(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise FleetError(f"{field} must be a positive integer")
    return value


def _iso_date(value: Any, field: str) -> str:
    text = _nonempty_text(value, field)
    try:
        date.fromisoformat(text)
    except ValueError as exc:
        raise FleetError(f"{field} must be YYYY-MM-DD") from exc
    return text


def validate_catalog(value: dict[str, Any]) -> None:
    if (
        value.get("schema_version") != 1
        or value.get("document_type") != "glaeda-owned-fleet-workload-catalog"
    ):
        raise FleetError("unsupported fleet workload catalog")

    states = value.get("state_classes")
    contracts = value.get("state_evidence_contract")
    if (
        not isinstance(states, list)
        or not states
        or not isinstance(contracts, dict)
        or set(states) != set(contracts)
    ):
        raise FleetError("state class contract is incomplete")
    for state in states:
        contract = contracts[state]
        if not isinstance(contract, dict) or not contract:
            raise FleetError(f"state class {state} has no evidence contract")
        if any(type(item) is not bool for item in contract.values()):
            raise FleetError(f"state class {state} evidence values must be booleans")

    profiles = value.get("contention_profiles") or {}
    expected_jobs = {"large": 1, "medium": 2, "small": 4}
    if set(profiles) != set(expected_jobs):
        raise FleetError("contention profiles must be large/medium/small")
    for name, jobs in expected_jobs.items():
        if profiles[name] != {"jobs": jobs}:
            raise FleetError(
                "contention profiles define concurrency shape only; "
                "absolute CPU/RAM belongs in each experiment manifest"
            )

    workloads = value.get("workloads")
    if not isinstance(workloads, list) or not workloads:
        raise FleetError("workload catalog must contain workloads")
    ids: set[str] = set()
    for workload in workloads:
        wid = workload.get("id")
        try:
            _opaque_token(wid, "workload.id")
        except FleetError as exc:
            raise FleetError("workload IDs must be unique bounded opaque tokens") from exc
        if wid in ids:
            raise FleetError("workload IDs must be unique bounded opaque tokens")
        ids.add(wid)
        for field in (
            "repository",
            "commit",
            "tree",
            "platform",
            "toolchain",
            "resource_envelope",
        ):
            if field not in workload:
                raise FleetError(f"workload {wid} is missing {field}")
        if (
            not isinstance(workload["commit"], str)
            or not re.fullmatch(r"[0-9a-f]{40}", workload["commit"])
            or not isinstance(workload["tree"], str)
            or not re.fullmatch(r"[0-9a-f]{40}", workload["tree"])
        ):
            raise FleetError(f"workload {wid} must pin exact commit/tree")
        if ("operation" in workload) == ("variants" in workload):
            raise FleetError(
                f"workload {wid} must define exactly one operation or variants map"
            )


def catalog() -> dict[str, Any]:
    value = load_json(CATALOG_PATH)
    validate_catalog(value)
    return value


def find_workload(wid: str, variant: str | None = None):
    for workload in catalog()["workloads"]:
        if workload["id"] != wid:
            continue
        if "variants" in workload:
            selected = variant or "focused"
            if selected not in workload["variants"]:
                raise FleetError(f"unknown variant {selected!r} for {wid}")
            return workload, workload["variants"][selected], selected
        if variant is not None:
            raise FleetError(f"workload {wid} has no variants")
        return workload, workload["operation"], None
    raise FleetError(f"unknown workload: {wid}")


def validate_machine(value: dict[str, Any], require_complete: bool = False):
    if (
        value.get("schema_version") != 1
        or value.get("document_type") != "glaeda-owned-fleet-machine-receipt"
    ):
        raise FleetError("unsupported machine receipt")
    if value.get("status") not in {"partial", "complete"}:
        raise FleetError("machine receipt status must be partial or complete")
    _opaque_token(value.get("machine_id"), "machine_id")
    for field in (
        "serial_numbers_omitted",
        "hostname_omitted",
        "unrelated_host_details_omitted",
    ):
        if (value.get("privacy") or {}).get(field) is not True:
            raise FleetError(f"machine receipt privacy.{field} must be true")

    if require_complete and value["status"] != "complete":
        raise FleetError(
            "physical benchmark execution requires a complete machine receipt"
        )
    if value["status"] != "complete":
        return value

    _nonempty_text(value.get("machine_config"), "machine_config")
    purchase = value.get("purchase") or {}
    _iso_date(purchase.get("date"), "purchase.date")
    currency = _nonempty_text(purchase.get("currency"), "purchase.currency")
    if re.fullmatch(r"[A-Z]{3}", currency) is None:
        raise FleetError("purchase.currency must be an uppercase ISO-like currency code")
    _positive_number(purchase.get("all_in_price"), "purchase.all_in_price")

    cpu = value.get("cpu") or {}
    _nonempty_text(cpu.get("vendor"), "cpu.vendor")
    _nonempty_text(cpu.get("model"), "cpu.model")
    topology = cpu.get("topology") or {}
    physical = _positive_integer(
        topology.get("physical_cores"), "cpu.topology.physical_cores"
    )
    logical = _positive_integer(
        topology.get("logical_cpus"), "cpu.topology.logical_cpus"
    )
    if logical < physical:
        raise FleetError("cpu.topology.logical_cpus cannot be below physical_cores")
    classes = topology.get("classes")
    if not isinstance(classes, list):
        raise FleetError("cpu.topology.classes must be a list")
    class_core_total = 0
    for index, item in enumerate(classes):
        if not isinstance(item, dict):
            raise FleetError(f"cpu.topology.classes[{index}] must be an object")
        _nonempty_text(item.get("class"), f"cpu.topology.classes[{index}].class")
        class_core_total += _positive_integer(
            item.get("cores"), f"cpu.topology.classes[{index}].cores"
        )
    if classes and class_core_total != physical:
        raise FleetError("cpu.topology.classes core total must equal physical_cores")

    _positive_integer(value.get("ram_bytes"), "ram_bytes")
    storage = value.get("internal_storage") or {}
    _positive_integer(storage.get("capacity_bytes"), "internal_storage.capacity_bytes")
    _nonempty_text(storage.get("public_model"), "internal_storage.public_model")
    _nonempty_text(storage.get("bus"), "internal_storage.bus")
    _nonempty_text(storage.get("filesystem"), "internal_storage.filesystem")

    platform = value.get("platform") or {}
    os_name = _nonempty_text(platform.get("os"), "platform.os").lower()
    _nonempty_text(platform.get("os_version"), "platform.os_version")
    _nonempty_text(platform.get("kernel"), "platform.kernel")
    if not any(platform.get(key) for key in ("xcode", "rust_toolchain", "python")):
        raise FleetError(
            "complete machine receipt must record at least one applicable host toolchain"
        )
    if os_name in {"macos", "darwin"}:
        _nonempty_text(platform.get("xcode"), "platform.xcode")

    _nonempty_text(value.get("power_policy"), "power_policy")
    _nonempty_text(value.get("filesystem"), "filesystem")
    _nonempty_text(value.get("network_class"), "network_class")

    power = value.get("power") or {}
    power_status = power.get("measurement_status")
    if power_status == "measured":
        idle = _positive_number(power.get("idle_watts"), "power.idle_watts")
        load = _positive_number(power.get("load_watts"), "power.load_watts")
        if load < idle:
            raise FleetError("power.load_watts must be at least power.idle_watts")
        _nonempty_text(power.get("measurement_method"), "power.measurement_method")
    elif power_status not in {"pending", "unavailable"}:
        raise FleetError(
            "complete machine power status must be pending, measured, or unavailable"
        )

    thermal = value.get("thermal") or {}
    thermal_status = thermal.get("observation_status")
    if thermal_status == "measured":
        _positive_number(
            thermal.get("sustained_minutes"), "thermal.sustained_minutes"
        )
        _positive_number(
            thermal.get("max_temperature_c"), "thermal.max_temperature_c"
        )
        if type(thermal.get("throttling_observed")) is not bool:
            raise FleetError("thermal.throttling_observed must be boolean")
    elif thermal_status not in {"pending", "unavailable"}:
        raise FleetError(
            "complete machine thermal status must be pending, measured, or unavailable"
        )
    return value


def machine_comparison_identity(machine: dict[str, Any]) -> dict[str, Any]:
    validate_machine(machine, require_complete=True)
    return {
        "schema_version": 1,
        "machine_id": machine["machine_id"],
        "cpu": machine["cpu"],
        "ram_bytes": machine["ram_bytes"],
        "internal_storage": machine["internal_storage"],
        "platform": machine["platform"],
        "power_policy": machine["power_policy"],
        "filesystem": machine["filesystem"],
        "network_class": machine["network_class"],
    }


def machine_comparison_digest(machine: dict[str, Any]) -> str:
    return digest_json(machine_comparison_identity(machine))


def validate_state_evidence(value: dict[str, Any]):
    value_catalog = catalog()
    if (
        value.get("schema_version") != STATE_SCHEMA_VERSION
        or value.get("document_type") != "glaeda-owned-fleet-state-evidence"
    ):
        raise FleetError("unsupported state evidence")
    state = value.get("state_class")
    if state not in value_catalog["state_evidence_contract"]:
        raise FleetError("unknown state class")
    observed = value.get("observed")
    required = value_catalog["state_evidence_contract"][state]
    if not isinstance(observed, dict) or set(observed) != set(required):
        raise FleetError(f"state evidence {state} must contain the exact evidence keys")
    for key, expected in required.items():
        if observed.get(key) is not expected:
            raise FleetError(
                f"state evidence {state} requires {key}={str(expected).lower()}"
            )
    _opaque_token(value.get("preparation_id"), "preparation_id")
    return value


def state_template(state: str):
    value_catalog = catalog()
    if state not in value_catalog["state_evidence_contract"]:
        raise FleetError("unknown state class")
    return {
        "schema_version": 1,
        "document_type": "glaeda-owned-fleet-state-evidence",
        "state_class": state,
        "preparation_id": f"replace-with-{state}-preparation-id",
        "observed": dict(value_catalog["state_evidence_contract"][state]),
        "notes": (
            "Keep paths, hostnames, serials, credentials and unrelated host details out."
        ),
    }


def render(text: str, state_dir: Path, semantic: Path) -> str:
    return text.format(
        state_dir=shlex.quote(os.fspath(state_dir)),
        semantic_receipt=shlex.quote(os.fspath(semantic)),
    )


GIT_PROBE_ENV = {
    "LC_ALL": "C",
    "LANG": "C",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
}


def git_identity(root: Path):
    result = subprocess.run(
        ["/usr/bin/git", "rev-parse", "HEAD", "HEAD^{tree}"],
        cwd=root,
        env=GIT_PROBE_ENV,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=15,
    )
    lines = result.stdout.splitlines()
    if result.returncode or len(lines) != 2:
        raise FleetError("git could not identify HEAD and tree")
    return lines[0], lines[1]


def verify_source(root: Path, workload: dict[str, Any]) -> None:
    head, tree = git_identity(root)
    if head != workload["commit"] or tree != workload["tree"]:
        raise FleetError(
            f"source mismatch: expected {workload['commit']} / "
            f"{workload['tree']}, got {head} / {tree}"
        )
    status = subprocess.run(
        [
            "/usr/bin/git",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
        cwd=root,
        env=GIT_PROBE_ENV,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    if status.returncode or status.stdout:
        raise FleetError(
            "benchmark source worktree must remain clean at the exact pinned commit/tree"
        )


def platform_allowed(kind: str) -> bool:
    return (
        (kind == "linux" and sys.platform.startswith("linux"))
        or (kind == "macos" and sys.platform == "darwin")
        or (
            kind == "linux-or-macos"
            and (sys.platform.startswith("linux") or sys.platform == "darwin")
        )
    )


BENCHMARK_ENV_ALLOWLIST = (
    "PATH",
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "DEVELOPER_DIR",
    "XDG_RUNTIME_DIR",
)


def env_for(workload: dict[str, Any], state_dir: Path):
    environment = {
        key: os.environ[key]
        for key in BENCHMARK_ENV_ALLOWLIST
        if key in os.environ
    }
    environment.update(
        {
            "LANG": "C",
            "LC_ALL": "C",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
        }
    )
    for key, value in (workload.get("environment") or {}).items():
        environment[key] = value.format(state_dir=os.fspath(state_dir))
    return environment
