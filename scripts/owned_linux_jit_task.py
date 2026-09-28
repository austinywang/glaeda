"""Fixed Big Red GitHub Actions JIT task adapter.

This is an internal semantic adapter, not a remote command surface. The Rust
controller supplies one already-reviewed assignment identity, payload generation,
and task path. The GitHub JIT secret is inherited on stdin and never decoded,
retained, or emitted by this process.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys

import owned_linux_admission as admission
import owned_linux_task as task


SCHEMA_VERSION = 1
PROFILE = "github-actions-trusted-v1"
NETWORK = task.TaskNetwork.GITHUB_ACTIONS_TRUSTED_EGRESS
RUNNER_VERSION = "2.336.0"
RUNNER_ARCHITECTURE = "x64"
RUNNER_ARCHIVE_SHA256 = "sha256:04cf0be1aff4c3ec3554466c39124ca250e3effd8873bb7e8d68535aa9505d5d"
LAUNCHER_SHA256 = "sha256:4bf42a6f6c772d4d5769627b1c6d80633ef75b41dcdc72f975beda80bc605421"
CPU_QUOTA_PERCENT = 400
MEMORY_MAX_BYTES = 8 * 1024**3
MEMORY_SWAP_MAX_BYTES = 0
TASKS_MAX = 1024
MAX_DEADLINE_SECONDS = 6 * 60 * 60
MAX_PAYLOAD_BYTES = 1024 * 1024 * 1024
MAX_PAYLOAD_ENTRIES = 50_000
TASK_DOCUMENT = "task.json"
EGRESS_GUARD_DOCUMENT_TYPE = "glaeda-owned-linux-egress-authority"
EGRESS_GUARD_ENFORCEMENT = "external_reviewed_host_firewall_or_gateway"
MAX_EGRESS_GUARD_BYTES = 4096
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
UNIT_RE = re.compile(r"^glaeda-gha-[0-9a-f]{32}\.service$")
# This runner keeps the existing focused capacity demand. Name it once so prepare,
# resume and recovery bind the same reviewed value instead of three defaults.
DEMAND = admission.VERIFY_FOCUSED_DEMAND


def canonical(value: object) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8") + b"\n"


def _digest(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def _validated_digest(value: str, label: str) -> str:
    if not DIGEST_RE.fullmatch(value):
        raise task.Refusal(f"{label} is invalid")
    return value


def _read_stable_regular(
    path: Path,
    *,
    max_bytes: int,
    allowed_uids: set[int],
    exact_modes: set[int] | None = None,
    forbidden_mode_bits: int = 0,
    require_nonempty: bool = False,
    problem: str,
) -> bytes:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        raise task.Refusal(problem) from error
    try:
        before = os.fstat(descriptor)
        mode = stat.S_IMODE(before.st_mode)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid not in allowed_uids
            or before.st_nlink != 1
            or (exact_modes is not None and mode not in exact_modes)
            or mode & forbidden_mode_bits
            or before.st_size < 0
            or before.st_size > max_bytes
            or (require_nonempty and before.st_size == 0)
        ):
            raise task.Refusal(problem)
        chunks: list[bytes] = []
        total = 0
        while True:
            remaining = max_bytes + 1 - total
            if remaining <= 0:
                raise task.Refusal(problem)
            chunk = os.read(descriptor, min(8192, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > max_bytes:
                raise task.Refusal(problem)
        after = os.fstat(descriptor)
        if (
            before.st_dev != after.st_dev
            or before.st_ino != after.st_ino
            or before.st_uid != after.st_uid
            or before.st_gid != after.st_gid
            or before.st_mode != after.st_mode
            or before.st_nlink != after.st_nlink
            or before.st_size != after.st_size
            or before.st_mtime_ns != after.st_mtime_ns
            or before.st_ctime_ns != after.st_ctime_ns
            or total != after.st_size
        ):
            raise task.Refusal(problem)
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _exact_path(value: str, *, directory: bool, private_parent: bool = False) -> Path:
    path = Path(value)
    if not path.is_absolute() or path.resolve() != path or path.is_symlink():
        raise task.Refusal("owned runner path is not exact")
    target = path.parent if private_parent else path
    if private_parent:
        if not target.is_dir() or target.is_symlink():
            raise task.Refusal("owned runner parent is unavailable")
        info = target.stat(follow_symlinks=False)
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
            raise task.Refusal("owned runner parent is not private")
    elif directory and not path.is_dir():
        raise task.Refusal("owned runner directory is unavailable")
    elif not directory and not path.is_file():
        raise task.Refusal("owned runner file is unavailable")
    return path


def _verify_launcher(path: Path) -> None:
    raw = _read_stable_regular(
        path,
        max_bytes=64 * 1024,
        allowed_uids={0, os.getuid()},
        forbidden_mode_bits=0o022,
        problem="owned runner launcher is unsafe",
    )
    if _digest(raw) != LAUNCHER_SHA256:
        raise task.Refusal("owned runner launcher changed")


def _validate_egress_guard_document(raw: bytes) -> dict[str, object]:
    if len(raw) > MAX_EGRESS_GUARD_BYTES:
        raise task.Refusal("owned runner egress authority is too large")
    value = admission.decode(raw)
    expected = {
        "class": NETWORK.value,
        "document_type": EGRESS_GUARD_DOCUMENT_TYPE,
        "enforcement": EGRESS_GUARD_ENFORCEMENT,
        "private_or_link_local_egress": False,
        "schema_version": SCHEMA_VERSION,
    }
    if value != expected or canonical(value) != raw:
        raise task.Refusal("owned runner egress authority is invalid")
    return value


def _verify_egress_guard(path: Path, expected_digest: str) -> None:
    raw = _read_stable_regular(
        path,
        max_bytes=MAX_EGRESS_GUARD_BYTES,
        allowed_uids={0},
        exact_modes={0o400, 0o444},
        require_nonempty=True,
        problem="owned runner egress authority is unsafe",
    )
    if _digest(raw) != expected_digest:
        raise task.Refusal("owned runner egress authority changed")
    _validate_egress_guard_document(raw)


def payload_tree_digest(root: Path) -> str:
    """Digest one protected runner tree, excluding only overmounted empty state dirs."""
    root_info = root.stat(follow_symlinks=False)
    if (
        root_info.st_uid not in (0, os.getuid())
        or stat.S_IMODE(root_info.st_mode) & 0o022
    ):
        raise task.Refusal("owned runner payload root is writable by another principal")
    mutable_mountpoints = {"_work", "_diag", "target"}
    h = hashlib.sha256()
    entries = 0
    total = 0
    for parent, directories, files in os.walk(root, topdown=True, followlinks=False):
        parent_path = Path(parent)
        directories.sort()
        files.sort()
        for name in directories + files:
            path = parent_path / name
            rel = path.relative_to(root).as_posix()
            info = path.lstat()
            entries += 1
            if entries > MAX_PAYLOAD_ENTRIES:
                raise task.Refusal("owned runner payload exceeds entry ceiling")
            if info.st_uid not in (0, os.getuid()) or info.st_mode & 0o022:
                raise task.Refusal("owned runner payload contains writable foreign state")
            mode = stat.S_IMODE(info.st_mode)
            if stat.S_ISDIR(info.st_mode):
                if rel in mutable_mountpoints:
                    try:
                        next(path.iterdir())
                    except StopIteration:
                        pass
                    else:
                        raise task.Refusal("owned runner payload state mountpoint is not empty")
                record = f"d {mode:o} {rel}\n".encode()
                h.update(record)
            elif stat.S_ISREG(info.st_mode):
                total += info.st_size
                if total > MAX_PAYLOAD_BYTES:
                    raise task.Refusal("owned runner payload exceeds byte ceiling")
                file_hash = hashlib.sha256()
                with path.open("rb") as stream:
                    while block := stream.read(1024 * 1024):
                        file_hash.update(block)
                h.update(
                    f"f {mode:o} {rel} {file_hash.hexdigest()}\n".encode()
                )
            elif stat.S_ISLNK(info.st_mode):
                target = os.readlink(path)
                resolved = (path.parent / target).resolve()
                try:
                    resolved.relative_to(root)
                except ValueError as error:
                    raise task.Refusal("owned runner payload symlink escapes payload") from error
                h.update(f"l {mode:o} {rel} {target}\n".encode())
            else:
                raise task.Refusal("owned runner payload contains unsupported entry")
    for required in ("bin/Runner.Listener", "_work", "_diag", "target"):
        if not (root / required).exists():
            raise task.Refusal("owned runner payload is incomplete")
    return "sha256:" + h.hexdigest()


def _identity(
    arguments: argparse.Namespace, *, require_egress_authority: bool = True
) -> tuple[str, dict[str, object]]:
    _validated_digest(arguments.command_fingerprint, "command fingerprint")
    _validated_digest(arguments.binding_sha256, "task binding")
    _validated_digest(arguments.payload_tree_sha256, "payload tree identity")
    _validated_digest(arguments.egress_guard_sha256, "egress authority identity")
    if not UNIT_RE.fullmatch(arguments.unit):
        raise task.Refusal("owned runner unit is invalid")
    payload = _exact_path(arguments.payload_root, directory=True)
    launcher = _exact_path(arguments.launcher, directory=False)
    _verify_launcher(launcher)
    if require_egress_authority:
        egress_guard = _exact_path(arguments.egress_guard, directory=False)
        _verify_egress_guard(egress_guard, arguments.egress_guard_sha256)
    observed_tree = payload_tree_digest(payload)
    if observed_tree != arguments.payload_tree_sha256:
        raise task.Refusal("owned runner payload generation changed")
    identity = {
        "schema_version": SCHEMA_VERSION,
        "profile": PROFILE,
        "network": NETWORK.value,
        "runner_version": RUNNER_VERSION,
        "runner_architecture": RUNNER_ARCHITECTURE,
        "runner_archive_sha256": RUNNER_ARCHIVE_SHA256,
        "payload_tree_sha256": observed_tree,
        "launcher_sha256": LAUNCHER_SHA256,
        "egress_guard_sha256": arguments.egress_guard_sha256,
        "command_fingerprint": arguments.command_fingerprint,
        "unit": arguments.unit,
        "binding_sha256": arguments.binding_sha256,
    }
    return _digest(canonical(identity)), identity


def _task_root(arguments: argparse.Namespace) -> Path:
    root = _exact_path(arguments.task_root, directory=False, private_parent=True)
    token = arguments.unit.removeprefix("glaeda-gha-").removesuffix(".service")
    if root.name != token:
        raise task.Refusal("owned runner task path is not bound to its unit")
    return root


def _task_document(root: Path) -> dict[str, object]:
    raw = _read_stable_regular(
        root / TASK_DOCUMENT,
        max_bytes=admission.MAX_DOCUMENT,
        allowed_uids={os.getuid()},
        exact_modes={0o600},
        require_nonempty=True,
        problem="owned runner task document is unsafe",
    )
    value = admission.decode(raw)
    if not isinstance(value, dict) or canonical(value) != raw:
        raise task.Refusal("owned runner task document is invalid")
    return value


def _write_private(path: Path, value: dict[str, object]) -> None:
    raw = canonical(value)
    fd = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
        0o600,
    )
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        raise


def _expected_document(task_identity: str, identity: dict[str, object]) -> dict[str, object]:
    return {
        "document_type": "glaeda-owned-linux-jit-task",
        "schema_version": SCHEMA_VERSION,
        "task_identity_sha256": task_identity,
        **identity,
    }


def _validate_task_state(
    arguments: argparse.Namespace, task_identity: str, identity: dict[str, object]
) -> Path:
    root = _task_root(arguments)
    if not root.is_dir() or root.is_symlink():
        raise task.Refusal("owned runner task is absent")
    if _task_document(root) != _expected_document(task_identity, identity):
        raise task.Refusal("owned runner task identity changed")
    for name in ("work", "diag", "home"):
        path = root / name
        info = path.stat(follow_symlinks=False)
        if (
            not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) != 0o700
        ):
            raise task.Refusal("owned runner private state changed")
    return root


def _expected_reservation(
    arguments: argparse.Namespace, generation: str
) -> dict[str, object]:
    """The exact durable reservation identity this runner may own.

    Every term the record carries is reconstructed here, including the reviewed
    demand, so prepare, resume, probe, phase and recovery cannot drift apart.
    """
    return {
        "schema_version": admission.RESERVATION_SCHEMA_VERSION,
        "command_fingerprint": arguments.command_fingerprint,
        "unit": arguments.unit,
        "generation": generation,
        "binding_sha256": arguments.binding_sha256,
        "demand": admission.demand_record(DEMAND),
    }


def _reservation(arguments: argparse.Namespace) -> admission.Reservation:
    store = admission.Store(arguments.admission_root)
    try:
        with store.lock("policy.lock"):
            current = admission.policy(store)
            existing = store.read("reservation.json")
        expected = _expected_reservation(arguments, current["generation"])
        if existing is None:
            return admission.Reservation(
                arguments.admission_root,
                arguments.command_fingerprint,
                arguments.unit,
                arguments.binding_sha256,
                DEMAND,
            )
        if existing == {**expected, "phase": "preparing"}:
            return admission.Reservation.resume(
                arguments.admission_root,
                arguments.command_fingerprint,
                arguments.unit,
                arguments.binding_sha256,
                DEMAND,
            )
        raise task.Refusal("owned runner reservation requires recovery")
    finally:
        store.close()


def _repairable_preparing_root(
    root: Path, task_identity: str, identity: dict[str, object]
) -> None:
    info = root.stat(follow_symlinks=False)
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) != 0o700
    ):
        raise task.Refusal("owned runner partial task root is unsafe")
    allowed = {"work", "diag", "home", TASK_DOCUMENT}
    for child in root.iterdir():
        if child.name not in allowed or child.is_symlink():
            raise task.Refusal("owned runner partial task contains unknown state")
        child_info = child.stat(follow_symlinks=False)
        if child_info.st_uid != os.getuid():
            raise task.Refusal("owned runner partial task ownership changed")
        if child.name == TASK_DOCUMENT:
            if _task_document(root) != _expected_document(task_identity, identity):
                raise task.Refusal("owned runner partial task identity changed")
        elif (
            not stat.S_ISDIR(child_info.st_mode)
            or stat.S_IMODE(child_info.st_mode) != 0o700
        ):
            raise task.Refusal("owned runner partial task state is unsafe")


def _reservation_phase(arguments: argparse.Namespace) -> str:
    store = admission.Store(arguments.admission_root)
    try:
        with store.lock("policy.lock"):
            current = admission.policy(store)
            value = store.read("reservation.json")
            expected = _expected_reservation(arguments, current["generation"])
            if value == {**expected, "phase": "preparing"}:
                return "preparing"
            if value == {**expected, "phase": "launching"}:
                return "launching"
            raise task.Refusal("owned runner reservation identity changed")
    finally:
        store.close()


def prepare(arguments: argparse.Namespace) -> int:
    task_identity, identity = _identity(arguments)
    root = _task_root(arguments)
    reservation = _reservation(arguments)
    with reservation:
        try:
            if root.exists():
                try:
                    _validate_task_state(arguments, task_identity, identity)
                except (task.Refusal, OSError, ValueError, json.JSONDecodeError):
                    _repairable_preparing_root(root, task_identity, identity)
                    task.remove_task(root)
            if not root.exists():
                task.prepare_task(root)
                for name in ("work", "diag", "home"):
                    (root / name).mkdir(mode=0o700)
                _write_private(root / TASK_DOCUMENT, _expected_document(task_identity, identity))
            _validate_task_state(arguments, task_identity, identity)
        except BaseException:
            if root.exists():
                _repairable_preparing_root(root, task_identity, identity)
                task.remove_task(root)
            reservation.release()
            raise
    emit(
        {
            "document_type": "glaeda-owned-linux-jit-prepare-receipt",
            "schema_version": SCHEMA_VERSION,
            "task_identity_sha256": task_identity,
            "profile": PROFILE,
            "network": NETWORK.value,
            "payload_tree_sha256": arguments.payload_tree_sha256,
            "reservation_phase": "preparing",
        }
    )
    return 0


def probe(arguments: argparse.Namespace) -> int:
    task_identity, identity = _identity(arguments, require_egress_authority=False)
    root = _task_root(arguments)
    store = admission.Store(arguments.admission_root)
    try:
        with store.lock("policy.lock"):
            current = admission.policy(store)
            reservation = store.read("reservation.json")
        expected = _expected_reservation(arguments, current["generation"])
        if not root.exists() and reservation is None:
            state = "absent"
            phase = None
        elif root.is_dir() and reservation in (
            {**expected, "phase": "preparing"},
            {**expected, "phase": "launching"},
        ):
            _validate_task_state(arguments, task_identity, identity)
            state = "present"
            phase = reservation["phase"]
        else:
            raise task.Refusal("owned runner task ownership is ambiguous")
    finally:
        store.close()
    emit(
        {
            "document_type": "glaeda-owned-linux-jit-probe",
            "schema_version": SCHEMA_VERSION,
            "task_identity_sha256": task_identity,
            "state": state,
            "reservation_phase": phase,
        }
    )
    return 0


def observe(arguments: argparse.Namespace) -> int:
    task_identity, identity = _identity(arguments)
    _validate_task_state(arguments, task_identity, identity)
    phase = _reservation_phase(arguments)
    if not task.unit_absent(arguments.unit):
        raise task.Refusal("owned runner process tree remains active")
    emit(
        {
            "document_type": "glaeda-owned-linux-jit-observation",
            "schema_version": SCHEMA_VERSION,
            "task_identity_sha256": task_identity,
            "reservation_phase": phase,
            "settled": True,
        }
    )
    return 0


def _sandbox(arguments: argparse.Namespace, root: Path) -> list[str]:
    payload = Path(arguments.payload_root)
    launcher = Path(arguments.launcher)
    return task.sandbox_command(
        payload,
        payload,
        payload,
        arguments.unit,
        systemd_properties=[
            f"CPUQuota={CPU_QUOTA_PERCENT}%",
            f"MemoryMax={MEMORY_MAX_BYTES}",
            f"MemorySwapMax={MEMORY_SWAP_MAX_BYTES}",
            f"TasksMax={TASKS_MAX}",
            f"RuntimeMaxSec={MAX_DEADLINE_SECONDS}",
            "KillMode=control-group",
            "TimeoutStopSec=30s",
            "NoNewPrivileges=yes",
            "UMask=0077",
        ],
        build_tmpfs_bytes=64 * 1024 * 1024,
        mount_arguments=[
            "--ro-bind", os.fspath(payload), "/runner",
            "--bind", os.fspath(root / "work"), "/runner/_work",
            "--bind", os.fspath(root / "diag"), "/runner/_diag",
            "--dir", "/home/runner",
            "--bind", os.fspath(root / "home"), "/home/runner",
            "--ro-bind", os.fspath(launcher), "/runner-launcher",
            "--chdir", "/runner",
        ],
        recipe_arguments=[
            "/usr/bin/env",
            "-i",
            "HOME=/home/runner",
            "PATH=/usr/bin:/bin",
            "GIT_CONFIG_GLOBAL=/dev/null",
            "GIT_CONFIG_NOSYSTEM=1",
            "LC_ALL=C",
            "/bin/bash",
            "/runner-launcher",
        ],
        network=NETWORK,
        source_read_only=True,
    )


def launch(arguments: argparse.Namespace) -> int:
    task_identity, identity = _identity(arguments)
    root = _validate_task_state(arguments, task_identity, identity)
    if type(arguments.deadline_seconds) is not int or not 1 <= arguments.deadline_seconds <= MAX_DEADLINE_SECONDS:
        raise task.Refusal("owned runner deadline is outside reviewed profile")
    reservation = admission.Reservation.resume(
        arguments.admission_root,
        arguments.command_fingerprint,
        arguments.unit,
        arguments.binding_sha256,
        DEMAND,
    )
    with reservation:
        observation = task.execute(
            _sandbox(arguments, root),
            unit=arguments.unit,
            deadline_seconds=arguments.deadline_seconds,
            label="owned-linux-jit-runner",
            launch_guard=reservation.launch,
            inherit_stdin=True,
            emit_failure_tail=False,
        )
    terminal, code, _, settled, _, _ = observation
    # The secret-bearing runner command is deliberately silent. GitHub owns the
    # canonical job/log surface; durable terminal evidence arrives through the
    # Scale Set delivery ledger, and probe/cleanup have separate bounded receipts.
    return 0 if terminal == "succeeded" and code == 0 and settled else 70


def cleanup(arguments: argparse.Namespace) -> int:
    task_identity, identity = _identity(arguments, require_egress_authority=False)
    root = _task_root(arguments)

    def settle() -> None:
        if not task.unit_absent(arguments.unit):
            task.stop_unit(arguments.unit)
        if not task.unit_absent(arguments.unit):
            raise task.Refusal("owned runner process tree remains active")
        if root.exists():
            _validate_task_state(arguments, task_identity, identity)
            task.remove_task(root)
        if root.exists() or root.is_symlink():
            raise task.Refusal("owned runner private state cleanup is incomplete")

    admission.recover(
        arguments.admission_root,
        arguments.command_fingerprint,
        arguments.unit,
        arguments.binding_sha256,
        settle,
        DEMAND,
    )
    # An already-released exact cleanup may still have task-private state after
    # a controller crash between durable release and final process return.
    if root.exists():
        settle()
    emit(
        {
            "document_type": "glaeda-owned-linux-jit-cleanup-receipt",
            "schema_version": SCHEMA_VERSION,
            "task_identity_sha256": task_identity,
            "settled": True,
            "task_state_absent": True,
            "capacity_released": True,
        }
    )
    return 0


def emit(value: object) -> None:
    sys.stdout.buffer.write(canonical(value))
    sys.stdout.buffer.flush()


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    subparsers = result.add_subparsers(dest="command", required=True)
    for name in ("probe", "prepare", "observe", "launch", "cleanup"):
        command = subparsers.add_parser(name)
        command.add_argument("--admission-root", required=True)
        command.add_argument("--task-root", required=True)
        command.add_argument("--payload-root", required=True)
        command.add_argument("--payload-tree-sha256", required=True)
        command.add_argument("--launcher", required=True)
        command.add_argument("--egress-guard", required=True)
        command.add_argument("--egress-guard-sha256", required=True)
        command.add_argument("--command-fingerprint", required=True)
        command.add_argument("--unit", required=True)
        command.add_argument("--binding-sha256", required=True)
        if name == "launch":
            command.add_argument("--deadline-seconds", type=int, required=True)
    return result


def main() -> int:
    arguments = parser().parse_args()
    try:
        return {
            "probe": probe,
            "prepare": prepare,
            "observe": observe,
            "launch": launch,
            "cleanup": cleanup,
        }[arguments.command](arguments)
    except (task.Refusal, OSError, ValueError, json.JSONDecodeError):
        print("owned Linux JIT task refused", file=sys.stderr)
        return 64


if __name__ == "__main__":
    raise SystemExit(main())
