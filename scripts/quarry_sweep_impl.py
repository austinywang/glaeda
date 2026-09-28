#!/usr/bin/env python3
"""Glaeda-managed Quarry research-sweep batch on owned Linux compute.

One admitted task runs a fixed, deterministic Quarry backtest sweep
(seven in-repo sample configs, sequential, offline) inside the shared
task-private systemd/bubblewrap boundary and returns one bounded canonical
receipt. This is a physical-experiment adapter, not a scheduler: no queue,
no daemon, no remote CLI, no caller-selected command, mounts, or resource
properties.

Workload authority stays with the Quarry repository: the driver calls
Quarry's exact ``run_backtest`` code path per config. Glaeda owns admission,
isolation, bounded capture, cleanup observation, and the outer receipt.
Per-config report bytes are hashed; only digests and a closed terminal set
enter the receipt. Semantic equality is proven by digest comparison against
the cold direct path, never by parsing human output.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import owned_linux_task as owned_task
from owned_linux_task import Refusal

SCHEMA_VERSION = 1
DOCUMENT_TYPE = "glaeda-quarry-sweep-receipt"
WORKLOAD_ID = "quarry-sweep/v1"
REPOSITORY = "teamleaderleo/quarry"
PYTHON_MAJOR = 3
PYTHON_MINOR = 14

SWEEP_CONFIGS = (
    "configs/channel_breakout_sample.json",
    "configs/ma_cross_pinned_sample.json",
    "configs/ma_cross_sample.json",
    "configs/time_series_momentum_sample.json",
    "configs/venue_constrained_sample.json",
    "configs/volatility_target_sample.json",
    "configs/zscore_mean_reversion_sample.json",
)

# Conservative experiment grant: 2 of 16 CPUs, 4 GiB of 30 GiB, no network.
# VPN lanes and interactive RDP keep the remaining headroom by construction.
SYSTEMD_PROPERTIES = (
    "CPUQuota=200%",
    "MemoryHigh=3G",
    "MemoryMax=4G",
    "TasksMax=128",
    "RuntimeMaxSec=600",
    "KillMode=mixed",
    "NoNewPrivileges=yes",
    "RestrictSUIDSGID=yes",
)
BUILD_TMPFS_BYTES = 256 * 1024 * 1024
DEADLINE_SECONDS = 600
MAX_EVIDENCE_BYTES = 64 * 1024

# Admission ceilings. Pressure refuses; reserves never become zero.
CPU_PRESSURE_AVG10_MAX = 30.0
MEM_PRESSURE_AVG10_MAX = 10.0
IO_PRESSURE_AVG10_MAX = 30.0
MEM_AVAILABLE_RESERVE_KIB = 8 * 1024 * 1024  # 4 GiB job grant + 4 GiB owner reserve
CPU_RESERVE = 4  # owner CPUs kept clear above the 2-CPU grant

OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
TERMINALS = ("succeeded", "failed", "timed_out", "cleanup_incomplete")

DRIVER = """\
import hashlib
import io
import json
import os
import sys
import time
from contextlib import redirect_stdout

sys.path.insert(0, os.environ["QUARRY_SRC"])
from quarry.cli import run_backtest

configs = json.loads(os.environ["QUARRY_SWEEP_CONFIGS"])
output_dir = os.environ["QUARRY_OUTPUT_DIR"]
records = []
for name in configs:
    buffer = io.StringIO()
    started = time.monotonic()
    terminal = "succeeded"
    try:
        with redirect_stdout(buffer):
            run_backtest(name)
    except Exception as exc:
        terminal = "failed"
        buffer.write("config_error: " + type(exc).__name__ + "\\n")
    elapsed = time.monotonic() - started
    text = buffer.getvalue()
    records.append({
        "config": name,
        "terminal": terminal,
        "elapsed_seconds": round(elapsed, 6),
        "output_bytes": len(text.encode("utf-8")),
        "output_sha256": "sha256:" + hashlib.sha256(text.encode("utf-8")).hexdigest(),
    })
    sys.stdout.write(text)
    if not text.endswith("\\n"):
        sys.stdout.write("\\n")
evidence = {"schema_version": 1, "records": records}
raw = (json.dumps(evidence, sort_keys=True, separators=(",", ":")) + "\\n").encode("utf-8")
with open(os.path.join(output_dir, "evidence.json"), "wb") as stream:
    stream.write(raw)
sys.stdout.write("SWEEP-EVIDENCE " + raw.decode("utf-8"))
"""


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def driver_sha256() -> str:
    return sha256(DRIVER.encode("utf-8"))


def workload_identity(commit: str, tree: str) -> dict[str, object]:
    return {
        "schema_version": 1,
        "workload_id": WORKLOAD_ID,
        "repository": REPOSITORY,
        "commit": commit,
        "tree": tree,
        "configs": list(SWEEP_CONFIGS),
        "driver_sha256": driver_sha256(),
        "python": "cpython-3.14",
        "grant": {
            "cpu_quota": "200%",
            "memory_high": "3G",
            "memory_max": "4G",
            "tasks_max": 128,
            "deadline_seconds": DEADLINE_SECONDS,
            "network": "none",
        },
    }


def command_fingerprint(commit: str, tree: str) -> str:
    return sha256(canonical_bytes(workload_identity(commit, tree)))


def parse_pressure_value(line: str) -> float:
    """Parse one /proc/pressure `some avg10=` value; unknown text stays unknown."""
    match = re.search(r"avg10=([0-9]+\.[0-9]+)", line)
    if match is None:
        raise Refusal("pressure observation is unavailable")
    return float(match.group(1))


def read_pressure() -> dict[str, float]:
    values = {}
    for kind in ("cpu", "memory", "io"):
        try:
            text = Path(f"/proc/pressure/{kind}").read_text(encoding="ascii")
        except OSError as error:
            raise Refusal("pressure observation is unavailable") from error
        first = text.splitlines()[0] if text.splitlines() else ""
        values[kind] = parse_pressure_value(first)
    return values


def read_mem_available_kib() -> int:
    try:
        text = Path("/proc/meminfo").read_text(encoding="ascii")
    except OSError as error:
        raise Refusal("memory observation is unavailable") from error
    match = re.search(r"^MemAvailable:\s+([0-9]+) kB$", text, re.MULTILINE)
    if match is None:
        raise Refusal("memory observation is unavailable")
    return int(match.group(1))


def count_cpus() -> int:
    count = os.cpu_count()
    if count is None or count < 1:
        raise Refusal("CPU observation is unavailable")
    return count


def read_vpn_up(interface: str = "tailscale0") -> bool:
    """True only when the VPN interface carries IFF_UP. Unknown raises."""
    try:
        flags = Path(f"/sys/class/net/{interface}/flags").read_text(encoding="ascii")
    except OSError as error:
        raise Refusal("VPN lane observation is unavailable") from error
    try:
        return bool(int(flags.strip(), 16) & 0x1)
    except ValueError as error:
        raise Refusal("VPN lane observation is unavailable") from error


def tcp_listen_ports(text: str) -> set[int]:
    """Parse /proc/net/tcp{,6} local ports in LISTEN state (0A)."""
    ports = set()
    for line in text.splitlines()[1:]:
        fields = line.split()
        if len(fields) < 4 or fields[3] != "0A":
            continue
        try:
            ports.add(int(fields[1].rsplit(":", 1)[1], 16))
        except (ValueError, IndexError):
            continue
    return ports


def rdp_listening() -> bool:
    """Observe (never require) the interactive RDP listener on TCP 3389."""
    try:
        text4 = Path("/proc/net/tcp").read_text(encoding="ascii")
        text6 = Path("/proc/net/tcp6").read_text(encoding="ascii")
    except OSError:
        return False
    return 3389 in tcp_listen_ports(text4) | tcp_listen_ports(text6)


def observe_node() -> dict[str, object]:
    pressure = read_pressure()
    return {
        "pressure_avg10": pressure,
        "mem_available_kib": read_mem_available_kib(),
        "cpus": count_cpus(),
        "vpn_up": read_vpn_up(),
        "rdp_listening": rdp_listening(),
    }


def decide_admission(observation: dict[str, object]) -> tuple[str, str]:
    """Return (disposition, reason). Only exact healthy observations admit."""
    pressure = observation.get("pressure_avg10")
    mem_kib = observation.get("mem_available_kib")
    cpus = observation.get("cpus")
    vpn_up = observation.get("vpn_up")
    if (
        not isinstance(pressure, dict)
        or type(mem_kib) is not int
        or type(cpus) is not int
        or type(vpn_up) is not bool
    ):
        raise Refusal("admission observation is incomplete")
    try:
        cpu = float(pressure["cpu"])
        mem = float(pressure["memory"])
        io = float(pressure["io"])
    except (KeyError, TypeError, ValueError) as error:
        raise Refusal("admission observation is incomplete") from error
    if not vpn_up:
        return ("refuse", "vpn_lane_impaired")
    if cpu > CPU_PRESSURE_AVG10_MAX or mem > MEM_PRESSURE_AVG10_MAX or io > IO_PRESSURE_AVG10_MAX:
        return ("wait", "pressure_high")
    if mem_kib < MEM_AVAILABLE_RESERVE_KIB:
        return ("wait", "memory_reserve_unmet")
    if cpus < 2 + CPU_RESERVE:
        return ("wait", "cpu_reserve_unmet")
    return ("admit", "compatible")


def exact_directory(raw: str, label: str) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        raise Refusal(f"{label} must be an absolute directory")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise Refusal(f"{label} is unavailable") from error
    if resolved != path or not path.is_dir() or path.is_symlink():
        raise Refusal(f"{label} must be one canonical plain directory")
    return path


def git_text(repository_root: Path, *arguments: str) -> str:
    return owned_task.run_control(["/usr/bin/git", *arguments], repository_root).decode(
        "ascii", errors="strict"
    ).strip()


def verify_resident_source(repository_root: Path, commit: str, tree: str) -> None:
    if not OID_PATTERN.fullmatch(commit) or not OID_PATTERN.fullmatch(tree):
        raise Refusal("source commit/tree identity is invalid")
    observed = git_text(
        repository_root, "rev-parse", f"{commit}^{{commit}}", f"{commit}^{{tree}}"
    ).splitlines()
    if observed != [commit, tree]:
        raise Refusal("resident Quarry source does not match the exact request")
    remotes = git_text(repository_root, "remote", "get-url", "--all", "origin").splitlines()
    admitted = {
        f"git@github.com:{REPOSITORY}.git",
        f"https://github.com:{REPOSITORY}.git",
        f"https://github.com/{REPOSITORY}.git",
        f"https://github.com/{REPOSITORY}",
    }
    if len(remotes) != 1 or remotes[0] not in admitted:
        raise Refusal("resident Quarry origin does not match the exact repository")
    status = owned_task.run_control(
        ["/usr/bin/git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
        repository_root,
    )
    if status:
        raise Refusal("resident Quarry checkout is not clean")
    version = subprocess.run(
        ["/usr/bin/python3", "--version"],
        env=owned_task.closed_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=30,
        check=False,
    )
    text = version.stdout.decode("ascii", errors="replace").strip()
    match = re.fullmatch(rf"Python {PYTHON_MAJOR}\.{PYTHON_MINOR}\.[0-9]+", text)
    if version.returncode != 0 or match is None:
        raise Refusal("canonical Python 3.14 toolchain is unavailable")


def sweep_environment(source: Path, output_dir: Path) -> dict[str, str]:
    return {
        "PATH": "/usr/bin:/bin",
        "HOME": "/home/project",
        "LC_ALL": "C",
        "QUARRY_SRC": os.fspath(source / "src"),
        "QUARRY_SWEEP_CONFIGS": json.dumps(
            list(SWEEP_CONFIGS), separators=(",", ":")
        ),
        "QUARRY_OUTPUT_DIR": os.fspath(output_dir),
    }


def run_driver_direct(
    repository_root: Path, output_dir: Path
) -> tuple[str, int, float, int, str, bytes]:
    """Cold path: same driver bytes, same interpreter, no sandbox or grant."""
    started = time.monotonic()
    try:
        completed = subprocess.run(
            ["/usr/bin/python3", "-c", DRIVER],
            cwd=repository_root,
            env=sweep_environment(repository_root, output_dir),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=DEADLINE_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        elapsed = time.monotonic() - started
        return ("timed_out", 124, elapsed, 0, sha256(b""), b"cold sweep exceeded its deadline")
    elapsed = time.monotonic() - started
    stdout = completed.stdout
    terminal = "succeeded" if completed.returncode == 0 else "failed"
    return (
        terminal,
        completed.returncode,
        elapsed,
        len(stdout),
        sha256(stdout),
        completed.stderr,
    )


def sandbox_command(
    source: Path,
    output_dir: Path,
    cargo_root: Path,
    rustup_root: Path,
    unit_name: str,
) -> list[str]:
    recipe = [
        "--chdir",
        "/workspace/source",
        "--clearenv",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "HOME",
        "/home/project",
        "--setenv",
        "LC_ALL",
        "C",
        "--setenv",
        "QUARRY_SRC",
        "/workspace/source/src",
        "--setenv",
        "QUARRY_SWEEP_CONFIGS",
        json.dumps(list(SWEEP_CONFIGS), separators=(",", ":")),
        "--setenv",
        "QUARRY_OUTPUT_DIR",
        "/workspace/output",
        "--",
        "/usr/bin/python3",
        "-c",
        DRIVER,
    ]
    return owned_task.sandbox_command(
        source,
        cargo_root,
        rustup_root,
        unit_name,
        systemd_properties=list(SYSTEMD_PROPERTIES),
        build_tmpfs_bytes=BUILD_TMPFS_BYTES,
        mount_arguments=[
            "--dir",
            "/workspace/output",
            "--bind",
            os.fspath(output_dir),
            "/workspace/output",
        ],
        recipe_arguments=recipe,
        network=owned_task.TaskNetwork.NONE,
    )


def read_evidence(output_dir: Path, label: str) -> tuple[dict[str, object], str]:
    path = output_dir / "evidence.json"
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise Refusal(f"{label} produced no evidence file") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_size > MAX_EVIDENCE_BYTES
    ):
        raise Refusal(f"{label} evidence file is unsafe")
    raw = path.read_bytes()
    try:
        value = json.loads(raw)
    except (UnicodeError, ValueError) as error:
        raise Refusal(f"{label} evidence is not JSON") from error
    if not valid_evidence(value):
        raise Refusal(f"{label} evidence does not match the exact workload")
    return value, sha256(raw)


def valid_evidence(value: object) -> bool:
    if not isinstance(value, dict) or set(value) != {"schema_version", "records"}:
        return False
    if value["schema_version"] != 1 or not isinstance(value["records"], list):
        return False
    records = value["records"]
    if [r.get("config") for r in records if isinstance(r, dict)] != list(SWEEP_CONFIGS):
        return False
    for record in records:
        if not isinstance(record, dict) or set(record) != {
            "config",
            "terminal",
            "elapsed_seconds",
            "output_bytes",
            "output_sha256",
        }:
            return False
        if record["terminal"] not in ("succeeded", "failed"):
            return False
        if (
            type(record["output_bytes"]) is not int
            or record["output_bytes"] < 0
            or not SHA256_PATTERN.fullmatch(record["output_sha256"])
            or not isinstance(record["elapsed_seconds"], (int, float))
        ):
            return False
    return True


def receipt(
    commit: str,
    tree: str,
    fingerprint: str,
    admission_before: dict[str, object],
    admission_after: dict[str, object],
    admission_disposition: str,
    evidence: dict[str, object] | None,
    evidence_sha256: str | None,
    terminal: str,
    exit_code: int,
    elapsed: float,
    settled: bool,
    cleanup_complete: bool,
    output_bytes: int,
    output_sha256: str,
    started_at_ms: int,
    settled_at_ms: int,
) -> dict[str, object]:
    return {
        "document_type": DOCUMENT_TYPE,
        "schema_version": SCHEMA_VERSION,
        "authority": "physical_execution_observation",
        "command_fingerprint": fingerprint,
        "source": {"repository": REPOSITORY, "commit": commit, "tree": tree},
        "toolchain": {"interpreter": "/usr/bin/python3", "requirement": "cpython-3.14"},
        "workload": {
            "id": WORKLOAD_ID,
            "configs": list(SWEEP_CONFIGS),
            "driver_sha256": driver_sha256(),
        },
        "grant": {
            "cpu_quota": "200%",
            "memory_high": "3G",
            "memory_max": "4G",
            "tasks_max": 128,
            "deadline_seconds": DEADLINE_SECONDS,
            "network": "none",
        },
        "admission": {
            "disposition": admission_disposition,
            "before": admission_before,
            "after": admission_after,
        },
        "evidence": evidence,
        "evidence_sha256": evidence_sha256,
        "result": {
            "terminal_class": terminal,
            "exit_code": exit_code,
            "elapsed_seconds": round(elapsed, 6),
            "started_at_unix_millis": started_at_ms,
            "settled_at_unix_millis": settled_at_ms,
            "process_tree_settled": settled,
            "task_cleanup_complete": cleanup_complete,
            "output_bytes": output_bytes,
            "output_sha256": output_sha256,
        },
        "contains_private_content": False,
        "contains_credentials": False,
        "authorizes_work": False,
        "authorizes_effects": False,
        "authorizes_redispatch": False,
    }


RECEIPT_KEYS = (
    "document_type",
    "schema_version",
    "authority",
    "command_fingerprint",
    "source",
    "toolchain",
    "workload",
    "grant",
    "admission",
    "evidence",
    "evidence_sha256",
    "result",
    "contains_private_content",
    "contains_credentials",
    "authorizes_work",
    "authorizes_effects",
    "authorizes_redispatch",
)


def valid_receipt(document: object, fingerprint: str) -> bool:
    if not isinstance(document, dict) or set(document) != set(RECEIPT_KEYS):
        return False
    if (
        document["document_type"] != DOCUMENT_TYPE
        or document["schema_version"] != SCHEMA_VERSION
        or document["authority"] != "physical_execution_observation"
        or document["command_fingerprint"] != fingerprint
    ):
        return False
    result = document.get("result")
    if not isinstance(result, dict) or result.get("terminal_class") not in TERMINALS:
        return False
    if document.get("evidence") is not None and not valid_evidence(document["evidence"]):
        return False
    for key in (
        "contains_private_content",
        "contains_credentials",
        "authorizes_work",
        "authorizes_effects",
        "authorizes_redispatch",
    ):
        if document.get(key) is not False:
            return False
    return True


def emit(document: dict[str, object]) -> None:
    sys.stdout.buffer.write(canonical_bytes(document) + b"\n")


def refuse(error: BaseException) -> int:
    message = str(error) if isinstance(error, Refusal) else "quarry sweep failed"
    sys.stderr.write(
        json.dumps(
            {
                "document_type": "glaeda-quarry-sweep-error",
                "schema_version": SCHEMA_VERSION,
                "authority": "none",
                "problem": message[:500],
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    )
    return 75


def run_cold(repository_root: Path, commit: str, tree: str) -> int:
    verify_resident_source(repository_root, commit, tree)
    before = observe_node()
    disposition, reason = decide_admission(before)
    if disposition != "admit":
        raise Refusal(f"sweep admission {disposition}: {reason}")
    with tempfile.TemporaryDirectory(prefix="glaeda-quarry-sweep-cold-") as raw:
        output_dir = Path(raw)
        started_at_ms = time.time_ns() // 1_000_000
        terminal, code, elapsed, out_bytes, digest, stderr = run_driver_direct(
            repository_root, output_dir
        )
        try:
            evidence, evidence_digest = read_evidence(output_dir, "cold")
        except (Refusal, OSError):
            evidence, evidence_digest = None, None
            if terminal == "succeeded":
                terminal = "failed"
        settled_at_ms = time.time_ns() // 1_000_000
        after = observe_node()
        document = receipt(
            commit,
            tree,
            command_fingerprint(commit, tree),
            before,
            after,
            "admit",
            evidence,
            evidence_digest,
            terminal,
            code,
            elapsed,
            True,
            True,
            out_bytes,
            digest,
            started_at_ms,
            settled_at_ms,
        )
        emit(document)
        if stderr and terminal != "succeeded":
            sys.stderr.buffer.write(stderr[:8192])
        return 0


def run_managed(
    repository_root: Path,
    cargo_root: Path,
    rustup_root: Path,
    commit: str,
    tree: str,
) -> int:
    verify_resident_source(repository_root, commit, tree)
    before = observe_node()
    disposition, reason = decide_admission(before)
    if disposition != "admit":
        raise Refusal(f"sweep admission {disposition}: {reason}")
    fingerprint = command_fingerprint(commit, tree)
    unit = f"glaeda-quarry-sweep-{fingerprint[7:19]}.service"
    if not owned_task.unit_absent(unit):
        raise Refusal("a sweep unit with the exact workload identity is already present")
    parent = Path(tempfile.mkdtemp(prefix="glaeda-quarry-sweep-"))
    task_root = parent / "task"
    output_dir = task_root / "output"
    terminal = "failed"
    code = 99
    elapsed = 0.0
    settled = False
    cleanup_complete = False
    out_bytes = 0
    digest = sha256(b"")
    evidence: dict[str, object] | None = None
    evidence_digest: str | None = None
    started_at_ms = time.time_ns() // 1_000_000
    try:
        owned_task.prepare_task(task_root)
        output_dir.mkdir(mode=0o700)
        source = owned_task.materialize(repository_root, task_root, commit, tree)
        command = sandbox_command(source, output_dir, cargo_root, rustup_root, unit)
        terminal, code, elapsed, settled, out_bytes, digest = owned_task.execute(
            command, unit=unit, deadline_seconds=DEADLINE_SECONDS, label="quarry-sweep"
        )
        if not settled:
            raise Refusal("physical process-tree settlement is incomplete")
        try:
            evidence, evidence_digest = read_evidence(output_dir, "managed")
        except (Refusal, OSError):
            evidence, evidence_digest = None, None
            if terminal == "succeeded":
                terminal = "failed"
    finally:
        try:
            owned_task.remove_task(task_root)
            cleanup_complete = True
        except (Refusal, OSError):
            terminal = "cleanup_incomplete"
        try:
            parent.rmdir()
        except OSError:
            terminal = "cleanup_incomplete"
            cleanup_complete = False
        if not owned_task.unit_absent(unit):
            owned_task.stop_unit(unit)
            if not owned_task.unit_absent(unit):
                terminal = "cleanup_incomplete"
                cleanup_complete = False
        settled = settled and owned_task.unit_absent(unit)
    settled_at_ms = time.time_ns() // 1_000_000
    after = observe_node()
    document = receipt(
        commit,
        tree,
        fingerprint,
        before,
        after,
        "admit",
        evidence,
        evidence_digest,
        terminal,
        code,
        elapsed,
        settled,
        cleanup_complete,
        out_bytes,
        digest,
        started_at_ms,
        settled_at_ms,
    )
    emit(document)
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subcommands = root.add_subparsers(dest="command", required=True)
    workload = subcommands.add_parser("workload", help="emit the workload identity")
    workload.add_argument("--commit", required=True)
    workload.add_argument("--tree", required=True)
    cold = subcommands.add_parser("cold", help="run the sweep directly (control arm)")
    cold.add_argument("--repository-root", required=True)
    cold.add_argument("--commit", required=True)
    cold.add_argument("--tree", required=True)
    managed = subcommands.add_parser("run", help="run the sweep under Glaeda admission")
    managed.add_argument("--repository-root", required=True)
    managed.add_argument("--cargo-root", required=True)
    managed.add_argument("--rustup-root", required=True)
    managed.add_argument("--commit", required=True)
    managed.add_argument("--tree", required=True)
    return root


def main() -> int:
    arguments = parser().parse_args()
    try:
        if arguments.command == "workload":
            if not OID_PATTERN.fullmatch(arguments.commit) or not OID_PATTERN.fullmatch(
                arguments.tree
            ):
                raise Refusal("source commit/tree identity is invalid")
            emit(
                {
                    "document_type": "glaeda-quarry-sweep-workload",
                    "schema_version": SCHEMA_VERSION,
                    "workload": workload_identity(arguments.commit, arguments.tree),
                    "command_fingerprint": command_fingerprint(
                        arguments.commit, arguments.tree
                    ),
                }
            )
            return 0
        repository_root = exact_directory(arguments.repository_root, "resident repository")
        if arguments.command == "cold":
            return run_cold(repository_root, arguments.commit, arguments.tree)
        cargo_root = exact_directory(arguments.cargo_root, "Cargo root")
        rustup_root = exact_directory(arguments.rustup_root, "rustup root")
        return run_managed(
            repository_root, cargo_root, rustup_root, arguments.commit, arguments.tree
        )
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        return refuse(error)


if __name__ == "__main__":
    raise SystemExit(main())
