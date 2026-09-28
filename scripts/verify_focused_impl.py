#!/usr/bin/env python3
"""Execute one fixed Glaeda verification profile without source credentials.

The caller supplies identities, never a command, argv, environment, executable, remote URL, or
mutable ref. Local workstation configuration supplies the exact resident repository, state,
Cargo, and rustup roots. Repository code runs only inside the closed bubblewrap/systemd boundary.
"""

from __future__ import annotations

from contextlib import nullcontext
import argparse
import fcntl
import hashlib
import json
import math
import os
import re
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import IO, NoReturn

import owned_linux_task as owned_task
import owned_linux_admission as owned_admission
from owned_linux_task import (
    Refusal, closed_environment, run_control, remove_task,
    public_crates_io_cache_arguments, unit_absent, stop_unit,
    MAX_CONTROL_OUTPUT_BYTES,
    MAX_SOURCE_OUTPUT_BYTES,
    FAILURE_TAIL_BYTES,
    MAX_MATERIALIZED_SOURCE_BYTES,
    MAX_MATERIALIZED_SOURCE_ENTRIES,
    CARGO_HOME_TMPFS_BYTES,
    TEMP_TMPFS_BYTES,
    PROJECT_HOME_TMPFS_BYTES,
)


SCHEMA_VERSION = 1
EXECUTION_IDENTITY_CLASS = "credentialless_project"
RUST_TOOLCHAIN = "1.97.1-x86_64-unknown-linux-gnu"
MAX_RECEIPT_BYTES = 32 * 1024
TARGET_TMPFS_BYTES = 6 * 1024 * 1024 * 1024
REQUIRED_TARGET_TMPFS_BYTES = 8 * 1024 * 1024 * 1024
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")
REPOSITORY_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
SEMANTIC_REQUEST_ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{7,63}$")
SEMANTIC_BINDING_DOCUMENT_TYPE = "glaeda-semantic-execution-binding"
SEMANTIC_BINDING_SCHEMA_VERSION = 1


class SemanticRequestConflict(Refusal):
    """One accepted semantic request identity was rebound to different physical work."""


@dataclass(frozen=True)
class Profile:
    profile_id: str
    profile_class: str
    resource_class: str
    deadline_seconds: int
    recipe_name: str
    document_slug: str
    build_tmpfs_bytes: int
    memory_high: str
    memory_max: str


def fixed_profile(
    profile_id: str,
    profile_class: str,
    resource_class: str,
    deadline_seconds: int,
    recipe_name: str,
    document_slug: str,
    build_tmpfs_bytes: int,
    memory_high: str,
    memory_max: str,
) -> Profile:
    return Profile(
        profile_id,
        profile_class,
        resource_class,
        deadline_seconds,
        recipe_name,
        document_slug,
        build_tmpfs_bytes,
        memory_high,
        memory_max,
    )


def profile_spec(profile: Profile) -> dict[str, object]:
    return {
        "schema_version": 1,
        "profile_id": profile.profile_id,
        "profile_class": profile.profile_class,
        "execution_identity_class": EXECUTION_IDENTITY_CLASS,
        "recipe": ["scripts/verify", profile.recipe_name],
        "rust_toolchain": RUST_TOOLCHAIN,
        "cargo_build_jobs": 4,
        "cargo_network": "offline",
        "git_optional_locks": False,
        "source_network": "none",
        "source_output": "controller_private_bounded_digest",
        "source_output_bytes": MAX_SOURCE_OUTPUT_BYTES,
        "source_state": "exact_read_only",
        "materialized_source_bytes_max": MAX_MATERIALIZED_SOURCE_BYTES,
        "build_state": "task_private_tmpfs",
        "build_tmpfs_bytes": profile.build_tmpfs_bytes,
        "package_cache": "host_public_crates_io_read_only",
        "resource_class": profile.resource_class,
        "deadline_seconds": profile.deadline_seconds,
        "systemd_properties": [
            "CPUQuota=400%",
            f"MemoryHigh={profile.memory_high}",
            f"MemoryMax={profile.memory_max}",
            "TasksMax=512",
            f"RuntimeMaxSec={profile.deadline_seconds}",
            "KillMode=mixed",
            "NoNewPrivileges=yes",
            "RestrictSUIDSGID=yes",
        ],
    }


FOCUSED_PROFILE = fixed_profile(
    "verify-focused/v1",
    "verify_focused",
    "big-red-focused",
    600,
    "focused",
    "verify-focused",
    TARGET_TMPFS_BYTES,
    "6G",
    "8G",
)
REQUIRED_PROFILE = fixed_profile(
    "verify-required/v1",
    "verify_required",
    "big-red-required",
    1200,
    "required",
    "verify-required",
    REQUIRED_TARGET_TMPFS_BYTES,
    "10G",
    "12G",
)
REQUIRED_ADMISSION_DEMAND = owned_admission.AdmissionDemand(
    memory_bytes=12 * 1024**3,
    minimum_logical_cpus=8,
)

# Compatibility aliases for the focused profile and its existing tests/consumers.
PROFILE_ID = FOCUSED_PROFILE.profile_id
PROFILE_CLASS = FOCUSED_PROFILE.profile_class
RESOURCE_CLASS = FOCUSED_PROFILE.resource_class
DEADLINE_SECONDS = FOCUSED_PROFILE.deadline_seconds
PROFILE_SPEC = profile_spec(FOCUSED_PROFILE)
REQUIRED_PROFILE_SPEC = profile_spec(REQUIRED_PROFILE)

ISOLATION_SPEC = {
    "filesystem": "read_only_source_task_private_tmpfs",
    "network": "none",
    "ambient_environment": "cleared",
    "publisher_credentials": "absent",
    "control_credentials": "absent",
    "ssh_agent": "absent",
    "sudo_admin": "absent",
    "unrelated_writable_projects": "absent",
    "build_state": "task_private_tmpfs",
    "package_cache": "host_public_crates_io_read_only",
}
RECEIPT_KEYS = (
    "document_type",
    "schema_version",
    "authority",
    "command_fingerprint",
    "source",
    "profile",
    "execution_identity_class",
    "isolation",
    "result",
    "contains_private_content",
    "contains_credentials",
    "authorizes_work",
    "authorizes_effects",
    "authorizes_redispatch",
)
RESULT_KEYS = (
    "terminal_class",
    "exit_code",
    "elapsed_seconds",
    "started_at_unix_millis",
    "settled_at_unix_millis",
    "process_tree_settled",
    "task_cleanup_complete",
    "raw_output",
    "output_bytes",
    "output_sha256",
    "cpu_seconds",
    "max_rss_kib",
    "resource_accounting",
)


@dataclass(frozen=True)
class Request:
    repository: str
    commit: str
    tree: str
    profile_generation: str
    command_fingerprint: str
    profile: Profile = FOCUSED_PROFILE


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def exact_keys(value: dict[str, object], expected: tuple[str, ...]) -> bool:
    return set(value) == set(expected)


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def profile_generation(profile: Profile = FOCUSED_PROFILE) -> str:
    return sha256(canonical_bytes(profile_spec(profile)))


def admission_demand(profile: Profile) -> owned_admission.AdmissionDemand:
    """Map one reviewed semantic verification profile to its local physical demand."""
    if profile == FOCUSED_PROFILE:
        return owned_admission.VERIFY_FOCUSED_DEMAND
    if profile == REQUIRED_PROFILE:
        return REQUIRED_ADMISSION_DEMAND
    raise Refusal("verification profile has no reviewed local admission demand")


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


def private_state_directory(raw: str) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        raise Refusal("state root must be absolute")
    parent = path.parent.resolve(strict=True)
    if parent != path.parent or not parent.is_dir() or parent.is_symlink():
        raise Refusal("state root parent is unavailable")
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    resolved = path.resolve(strict=True)
    metadata = path.lstat()
    if (
        resolved != path
        or not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise Refusal("state root is not a private current-user directory")
    return path


def normalize_request(
    arguments: argparse.Namespace, profile: Profile = FOCUSED_PROFILE
) -> Request:
    repository = arguments.repository
    commit = arguments.commit
    tree = arguments.tree
    generation = arguments.profile_generation
    fingerprint = arguments.command_fingerprint
    if not REPOSITORY_PATTERN.fullmatch(repository):
        raise Refusal("source repository identity is invalid")
    if not OID_PATTERN.fullmatch(commit) or not OID_PATTERN.fullmatch(tree):
        raise Refusal("source commit/tree identity is invalid")
    if generation != profile_generation(profile):
        raise Refusal(f"profile generation does not match {profile.profile_id}")
    if not SHA256_PATTERN.fullmatch(fingerprint):
        raise Refusal("command fingerprint is invalid")
    return Request(repository, commit, tree, generation, fingerprint, profile)


def git_text(repository_root: Path, *arguments: str) -> str:
    return run_control(["/usr/bin/git", *arguments], repository_root).decode(
        "ascii", errors="strict"
    ).strip()


def verify_resident_source(repository_root: Path, request: Request) -> None:
    commit, tree = git_text(
        repository_root, "rev-parse", f"{request.commit}^{{commit}}", f"{request.commit}^{{tree}}"
    ).splitlines()
    if commit != request.commit or tree != request.tree:
        raise Refusal("resident Git source does not match the exact request")
    remotes = git_text(repository_root, "remote", "get-url", "--all", "origin").splitlines()
    admitted = {
        f"git@github.com:{request.repository}.git",
        f"https://github.com/{request.repository}.git",
        f"https://github.com/{request.repository}",
    }
    if len(remotes) != 1 or remotes[0] not in admitted:
        raise Refusal("resident Git origin does not match the exact repository")


def ensure_private_child(parent: Path, name: str) -> Path:
    path = parent / name
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        metadata = path.lstat()
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            raise Refusal("command state contains an unsafe filesystem object")
    return path


def open_lock(directory: Path) -> IO[bytes]:
    descriptor = os.open(
        directory / "lock",
        os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size != 0
    ):
        os.close(descriptor)
        raise Refusal("command lock is not one private stable empty file")
    lock = os.fdopen(descriptor, "r+b", buffering=0)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        lock.close()
        raise Refusal("the exact verification command is already active") from error
    return lock


def read_document(path: Path) -> dict[str, object] | None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return None
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size > MAX_RECEIPT_BYTES
    ):
        raise Refusal("verification state contains an unsafe document")
    raw = path.read_bytes()
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise Refusal("verification state is corrupt") from error
    if not isinstance(value, dict) or canonical_bytes(value) + b"\n" != raw:
        raise Refusal("verification state is noncanonical")
    return value


def publish_document(path: Path, value: dict[str, object], *, replace: bool) -> None:
    raw = canonical_bytes(value) + b"\n"
    if len(raw) > MAX_RECEIPT_BYTES:
        raise Refusal("verification document exceeds its fixed ceiling")
    temporary = path.parent / f".{path.name}.creating-{os.getpid()}"
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb", closefd=True) as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        if replace:
            os.replace(temporary, path)
        else:
            try:
                os.link(temporary, path, follow_symlinks=False)
            except FileExistsError as error:
                raise Refusal("terminal verification document already exists") from error
            temporary.unlink()
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def bind_semantic_request(state_root: Path, request_id: str, request: Request) -> None:
    """Durably bind one accepted provider-neutral request identity before physical launch.

    The binding is private Glaeda decision state, separate from transport correlation. Exact replay
    is accepted; reusing one semantic identity for different physical work fails closed. The same
    state root is shared by direct Git and owner-local callers that intentionally carry the same
    accepted semantic request identity.
    """
    if not isinstance(request_id, str) or SEMANTIC_REQUEST_ID_PATTERN.fullmatch(request_id) is None:
        raise Refusal("semantic request identity is invalid")
    namespace = ensure_private_child(state_root, "semantic-requests")
    request_root = ensure_private_child(namespace, request_id)
    expected = {
        "document_type": SEMANTIC_BINDING_DOCUMENT_TYPE,
        "schema_version": SEMANTIC_BINDING_SCHEMA_VERSION,
        "request_id": request_id,
        "command_fingerprint": request.command_fingerprint,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "profile": {
            "id": request.profile.profile_id,
            "generation": request.profile_generation,
        },
    }
    with open_lock(request_root):
        path = request_root / "binding.json"
        existing = read_document(path)
        if existing is None:
            publish_document(path, expected, replace=False)
            return
        if existing != expected:
            raise SemanticRequestConflict(
                "semantic request identity conflicts with existing physical binding"
            )


def matches_request(document: dict[str, object], request: Request) -> bool:
    source = document.get("source")
    profile = document.get("profile")
    return (
        document.get("schema_version") == SCHEMA_VERSION
        and document.get("command_fingerprint") == request.command_fingerprint
        and isinstance(source, dict)
        and source.get("repository") == request.repository
        and source.get("commit") == request.commit
        and source.get("tree") == request.tree
        and isinstance(profile, dict)
        and profile.get("id") == request.profile.profile_id
        and profile.get("generation") == request.profile_generation
    )


def valid_terminal_receipt(document: dict[str, object], request: Request) -> bool:
    source = document.get("source")
    profile = document.get("profile")
    result = document.get("result")
    isolation = document.get("isolation")
    terminal = result.get("terminal_class") if isinstance(result, dict) else None
    cleanup_complete = (
        result.get("task_cleanup_complete") if isinstance(result, dict) else None
    )
    return (
        exact_keys(document, RECEIPT_KEYS)
        and matches_request(document, request)
        and document.get("document_type")
        == f"glaeda-{request.profile.document_slug}-receipt"
        and document.get("authority") == "physical_execution_observation"
        and document.get("execution_identity_class") == EXECUTION_IDENTITY_CLASS
        and document.get("contains_private_content") is False
        and document.get("contains_credentials") is False
        and document.get("authorizes_work") is False
        and document.get("authorizes_effects") is False
        and document.get("authorizes_redispatch") is False
        and source
        == {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        }
        and profile
        == {
            "id": request.profile.profile_id,
            "class": request.profile.profile_class,
            "generation": request.profile_generation,
            "resource_class": request.profile.resource_class,
            "deadline_seconds": request.profile.deadline_seconds,
        }
        and isinstance(result, dict)
        and exact_keys(result, RESULT_KEYS)
        and terminal in {"succeeded", "failed", "timed_out", "cleanup_incomplete"}
        and isinstance(result.get("exit_code"), int)
        and not isinstance(result.get("exit_code"), bool)
        and (terminal != "succeeded" or result["exit_code"] == 0)
        and isinstance(result.get("elapsed_seconds"), (int, float))
        and not isinstance(result.get("elapsed_seconds"), bool)
        and math.isfinite(result["elapsed_seconds"])
        and result["elapsed_seconds"] >= 0
        and isinstance(result.get("started_at_unix_millis"), int)
        and not isinstance(result.get("started_at_unix_millis"), bool)
        and isinstance(result.get("settled_at_unix_millis"), int)
        and not isinstance(result.get("settled_at_unix_millis"), bool)
        and result["settled_at_unix_millis"] >= result["started_at_unix_millis"] >= 0
        and result.get("process_tree_settled") is True
        and isinstance(cleanup_complete, bool)
        and (terminal == "cleanup_incomplete") == (cleanup_complete is False)
        and result.get("raw_output") == "not_published"
        and isinstance(result.get("output_bytes"), int)
        and not isinstance(result.get("output_bytes"), bool)
        and result["output_bytes"] >= 0
        and isinstance(result.get("output_sha256"), str)
        and SHA256_PATTERN.fullmatch(result["output_sha256"]) is not None
        and result.get("cpu_seconds") is None
        and result.get("max_rss_kib") is None
        and result.get("resource_accounting")
        == "cgroup_enforced_metrics_not_exported_v1"
        and isolation == ISOLATION_SPEC
    )


def valid_intent(document: dict[str, object], request: Request) -> bool:
    return (
        matches_request(document, request)
        and document.get("document_type")
        == f"glaeda-{request.profile.document_slug}-intent"
        and document.get("phase") in {"prepared", "executing"}
    )


def materialize(repository_root: Path, task_root: Path, request: Request) -> Path:
    return owned_task.materialize(repository_root, task_root, request.commit, request.tree)


def required_host_plan_evidence_arguments(profile: Profile) -> list[str]:
    if profile != REQUIRED_PROFILE:
        return []
    return [
        "--ro-bind",
        "/usr/lib/os-release",
        "/etc/os-release",
        "--ro-bind-try",
        "/etc/nsswitch.conf",
        "/etc/nsswitch.conf",
        "--ro-bind-try",
        "/etc/passwd",
        "/etc/passwd",
        "--ro-bind-try",
        "/etc/group",
        "/etc/group",
        "--ro-bind-try",
        "/etc/subuid",
        "/etc/subuid",
        "--ro-bind-try",
        "/etc/subgid",
        "/etc/subgid",
        "--ro-bind-try",
        "/etc/containers",
        "/etc/containers",
        "--dir",
        "/var",
        "--dir",
        "/var/lib",
        "--ro-bind",
        "/var/lib/dpkg",
        "/var/lib/dpkg",
    ]


def sandbox_command(
    source: Path,
    task_root: Path,
    cargo_root: Path,
    rustup_root: Path,
    unit_name: str,
    profile: Profile = FOCUSED_PROFILE,
) -> list[str]:
    recipe_arguments = (
        [
            "--chdir",
            "/workspace/source",
            "--clearenv",
            "--setenv",
            "PATH",
            "/cargo/bin:/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/home/project",
            "--setenv",
            "CARGO_HOME",
            "/cargo-home",
            "--setenv",
            "CARGO_TARGET_DIR",
            "/workspace/source/target",
            "--setenv",
            "CARGO_NET_OFFLINE",
            "true",
            "--setenv",
            "CARGO_BUILD_JOBS",
            "4",
            "--setenv",
            "RUSTUP_HOME",
            "/rustup",
            "--setenv",
            "RUSTUP_TOOLCHAIN",
            RUST_TOOLCHAIN,
            "--setenv",
            "GIT_CONFIG_NOSYSTEM",
            "1",
            "--setenv",
            "GIT_CONFIG_GLOBAL",
            "/dev/null",
            "--setenv",
            "LC_ALL",
            "C",
            "--setenv",
            "GIT_OPTIONAL_LOCKS",
            "0",
            "--",
            "/workspace/source/scripts/verify",
            profile.recipe_name,
        ]
    )
    spec = profile_spec(profile)
    return owned_task.sandbox_command(
        source, cargo_root, rustup_root, unit_name,
        systemd_properties=spec["systemd_properties"],
        build_tmpfs_bytes=profile.build_tmpfs_bytes,
        mount_arguments=(required_host_plan_evidence_arguments(profile)
                         + public_crates_io_cache_arguments(cargo_root)),
        recipe_arguments=recipe_arguments,
        network=owned_task.TaskNetwork.NONE,
    )


def admission_binding(request: Request, command_root: Path) -> str:
    info = command_root.stat()
    return sha256(canonical_bytes({
        "command_root": os.fspath(command_root), "device": info.st_dev, "inode": info.st_ino,
        "repository": request.repository, "commit": request.commit, "tree": request.tree,
        "profile_id": request.profile.profile_id, "profile_generation": request.profile_generation,
    }))


def sync_directory(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def unit_name(request: Request) -> str:
    return f"glaeda-{request.profile.document_slug}-{request.command_fingerprint[7:39]}.service"


def execute_profile(
    source: Path,
    task_root: Path,
    cargo_root: Path,
    rustup_root: Path,
    request: Request,
    admission=None,
) -> tuple[str, int, float, bool, int, str]:
    profile = request.profile
    unit = unit_name(request)
    return owned_task.execute(
        sandbox_command(source, task_root, cargo_root, rustup_root, unit, profile),
        unit=unit, deadline_seconds=profile.deadline_seconds, label=profile.document_slug,
        **({"launch_guard": admission.launch} if admission is not None else {}),
    )


def receipt(
    request: Request,
    terminal: str,
    exit_code: int,
    elapsed: float,
    process_tree_settled: bool,
    cleanup_complete: bool,
    output_bytes: int,
    output_sha256: str,
    started_at_ms: int,
    settled_at_ms: int,
) -> dict[str, object]:
    return {
        "document_type": f"glaeda-{request.profile.document_slug}-receipt",
        "schema_version": SCHEMA_VERSION,
        "authority": "physical_execution_observation",
        "command_fingerprint": request.command_fingerprint,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "profile": {
            "id": request.profile.profile_id,
            "class": request.profile.profile_class,
            "generation": request.profile_generation,
            "resource_class": request.profile.resource_class,
            "deadline_seconds": request.profile.deadline_seconds,
        },
        "execution_identity_class": EXECUTION_IDENTITY_CLASS,
        "isolation": dict(ISOLATION_SPEC),
        "result": {
            "terminal_class": terminal,
            "exit_code": exit_code,
            "elapsed_seconds": round(elapsed, 6),
            "started_at_unix_millis": started_at_ms,
            "settled_at_unix_millis": settled_at_ms,
            "process_tree_settled": process_tree_settled,
            "task_cleanup_complete": cleanup_complete,
            "raw_output": "not_published",
            "output_bytes": output_bytes,
            "output_sha256": output_sha256,
            "cpu_seconds": None,
            "max_rss_kib": None,
            "resource_accounting": "cgroup_enforced_metrics_not_exported_v1",
        },
        "contains_private_content": False,
        "contains_credentials": False,
        "authorizes_work": False,
        "authorizes_effects": False,
        "authorizes_redispatch": False,
    }


def emit(document: dict[str, object]) -> None:
    sys.stdout.buffer.write(canonical_bytes(document) + b"\n")


def run(arguments: argparse.Namespace, profile: Profile = FOCUSED_PROFILE) -> int:
    request = normalize_request(arguments, profile)
    repository_root = exact_directory(arguments.repository_root, "resident repository")
    cargo_root = exact_directory(arguments.cargo_root, "Cargo root")
    rustup_root = exact_directory(arguments.rustup_root, "rustup root")
    state_root = private_state_directory(arguments.state_root)
    semantic_request_id = vars(arguments).get("semantic_request_id")
    if semantic_request_id is not None:
        bind_semantic_request(state_root, semantic_request_id, request)
    verify_resident_source(repository_root, request)
    command_root = ensure_private_child(state_root, request.command_fingerprint[7:])
    with open_lock(command_root):
        receipt_path = command_root / "receipt.json"
        intent_path = command_root / "intent.json"
        existing = read_document(receipt_path)
        if existing is not None:
            if not valid_terminal_receipt(existing, request):
                raise Refusal("durable receipt conflicts with the exact command")
            admission_root = getattr(arguments, "admission_root", None)
            if arguments.reconcile_only and admission_root is not None:
                def observe_settled():
                    if (not existing["result"]["task_cleanup_complete"]
                            or not unit_absent(unit_name(request))
                            or (command_root / "task").exists()
                            or (command_root / "task").is_symlink()):
                        raise Refusal("exact reservation cleanup is incomplete")
                    old_intent = read_document(intent_path)
                    if old_intent is not None:
                        if not valid_intent(old_intent, request):
                            raise Refusal("reservation recovery intent conflicts")
                        intent_path.unlink()
                        sync_directory(command_root)
                owned_admission.recover(admission_root, request.command_fingerprint,
                                        unit_name(request), admission_binding(request, command_root),
                                        observe_settled, admission_demand(profile))
            emit(existing)
            return 0
        intent = read_document(intent_path)
        if arguments.reconcile_only:
            raise Refusal("no terminal receipt exists for exact reconciliation")
        if intent is not None:
            if not valid_intent(intent, request):
                raise Refusal("durable intent conflicts with the exact command")
            raise Refusal("previous physical execution is ambiguous; redispatch refused")

        admission_root = getattr(arguments, "admission_root", None)
        demand = admission_demand(profile) if admission_root is not None else None
        unit = unit_name(request)
        gate = (owned_admission.Reservation(admission_root, request.command_fingerprint, unit,
                                            admission_binding(request, command_root), demand)
                if admission_root is not None else nullcontext())
        with gate as admission:
            try:
                task_root = command_root / "task"
                owned_task.prepare_task(task_root)
                source = materialize(repository_root, task_root, request)
                base = {
                    "document_type": f"glaeda-{profile.document_slug}-intent",
                    "schema_version": SCHEMA_VERSION,
                    "command_fingerprint": request.command_fingerprint,
                    "source": {
                        "repository": request.repository,
                        "commit": request.commit,
                        "tree": request.tree,
                    },
                    "profile": {"id": profile.profile_id, "generation": request.profile_generation},
                }
                publish_document(intent_path, {**base, "phase": "prepared"}, replace=False)
                publish_document(intent_path, {**base, "phase": "executing"}, replace=True)
                started_at_ms = time.time_ns() // 1_000_000
                terminal, exit_code, elapsed, settled, output_bytes, output_sha256 = execute_profile(
                    source, task_root, cargo_root, rustup_root, request,
                    **({"admission": admission} if admission is not None else {}),
                )
                if not settled:
                    raise Refusal("physical process-tree settlement is incomplete; redispatch refused")
                cleanup_complete = False
                try:
                    remove_task(task_root)
                    cleanup_complete = True
                except (Refusal, OSError):
                    terminal = "cleanup_incomplete"
                settled_at_ms = time.time_ns() // 1_000_000
                document = receipt(
                    request,
                    terminal,
                    exit_code,
                    elapsed,
                    settled,
                    cleanup_complete,
                    output_bytes,
                    output_sha256,
                    started_at_ms,
                    settled_at_ms,
                )
                publish_document(receipt_path, document, replace=False)
                intent_path.unlink()
                if admission is not None and cleanup_complete:
                    sync_directory(command_root)
                    admission.release()
                emit(document)
                return 0
            finally:
                # A refused final check has never called Popen. Remove only this exact
                # attempt's task/intent before freeing its reservation. Any cleanup
                # error preserves the reservation; post-launch ambiguity always does.
                if admission is not None and admission.owned and not admission.launch_attempted:
                    remove_task(command_root / "task")
                    if intent_path.exists():
                        intent_path.unlink()
                    sync_directory(command_root)
                    admission.release()



def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subcommands = root.add_subparsers(dest="command", required=True)
    subcommands.add_parser("profile", help="emit the fixed path-free profile identity")
    execute = subcommands.add_parser("run", help="execute or reconcile one exact command")
    execute.add_argument("--repository-root", required=True)
    execute.add_argument("--state-root", required=True)
    execute.add_argument("--cargo-root", required=True)
    execute.add_argument("--rustup-root", required=True)
    execute.add_argument("--repository", required=True)
    execute.add_argument("--commit", required=True)
    execute.add_argument("--tree", required=True)
    execute.add_argument("--profile-generation", required=True)
    execute.add_argument("--command-fingerprint", required=True)
    execute.add_argument(
        "--semantic-request-id",
        help="accepted provider-neutral request identity; binds exact physical work across transports",
    )
    execute.add_argument("--reconcile-only", action="store_true")
    execute.add_argument("--admission-root", help="operator-installed reviewed launch gate; never caller-selected")
    return root


def refuse(error: BaseException, profile: Profile = FOCUSED_PROFILE) -> NoReturn:
    message = str(error) if isinstance(error, Refusal) else f"{profile.document_slug} execution failed"
    print(
        json.dumps(
            {
                "document_type": f"glaeda-{profile.document_slug}-error",
                "schema_version": SCHEMA_VERSION,
                "authority": "none",
                "problem": message[:500],
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
        file=sys.stderr,
    )
    raise SystemExit(75)


def main(profile: Profile = FOCUSED_PROFILE) -> int:
    arguments = parser().parse_args()
    try:
        if arguments.command == "profile":
            emit({**profile_spec(profile), "profile_generation": profile_generation(profile)})
            return 0
        return run(arguments, profile)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        refuse(error, profile)
