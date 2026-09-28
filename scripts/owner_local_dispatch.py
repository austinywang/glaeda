#!/usr/bin/env python3
"""Execute one provider-neutral owner-local request through reviewed Glaeda front doors.

The caller supplies only glaeda-semantic-request/v1 JSON on stdin. Host paths and executable
selection come from a private local installation plus fixed Glaeda-owned defaults. This module is
an adapter; verify-focused/v1 remains the source-executing owner.
"""
from __future__ import annotations

from dataclasses import dataclass
import json
import os
from pathlib import Path
import pwd
import re
import stat
import subprocess
import sys
from typing import NoReturn

import owned_linux_task as owned_task
import provider_neutral_request as semantic
import verify_focused_impl as focused


INSTALLATION_DOCUMENT_TYPE = "glaeda-owner-local-installation"
INSTALLATION_SCHEMA_VERSION = 1
MAX_INSTALLATION_BYTES = 16 * 1024
MAX_CONTROL_OUTPUT_BYTES = 192 * 1024
MAX_ERROR_OUTPUT_BYTES = 64 * 1024
MAX_REPOSITORIES = 32
INSTALLATION_KEYS = {
    "document_type",
    "schema_version",
    "repositories",
}
REPOSITORY_KEYS = {"repository", "checkout"}
AMBIGUOUS_PROBLEM = "previous physical execution is ambiguous; redispatch refused"
HOME = Path(pwd.getpwuid(os.getuid()).pw_dir)
CONTROL_ROOT = Path(__file__).resolve().parent.parent
INSTALLATION_PATH = HOME / ".config" / "glaeda" / "owner-local-v1.json"
REPO_QUERY_PROGRAM = HOME / ".local" / "bin" / "glaeda-repo-query"
SHARED_VERIFY_STATE_ROOT = (
    HOME / ".local" / "state" / "glaeda" / "provider-neutral-verify-v1"
)
ADMISSION_ROOT = (
    HOME / ".local" / "state" / "glaeda" / "direct-owned-admission-v1"
)
CARGO_ROOT = HOME / ".cargo"
RUSTUP_ROOT = HOME / ".rustup"


class LocalRefusal(RuntimeError):
    def __init__(self, code: str, problem: str):
        super().__init__(problem)
        self.code = code


@dataclass(frozen=True)
class RepositoryBinding:
    repository: str
    checkout: Path


@dataclass(frozen=True)
class Installation:
    repositories: tuple[RepositoryBinding, ...]

    def checkout_for(self, repository: str) -> Path:
        matches = [item.checkout for item in self.repositories if item.repository == repository]
        if len(matches) != 1:
            raise LocalRefusal(
                "source_unavailable",
                "requested repository has no unique owner-local resident binding",
            )
        return matches[0]


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def _exact_absolute_path(value: object, label: str, *, directory: bool) -> Path:
    if not isinstance(value, str) or not value.startswith("/") or "\x00" in value:
        raise LocalRefusal("installation_invalid", f"{label} is invalid")
    path = Path(value)
    try:
        metadata = path.lstat()
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise LocalRefusal("installation_invalid", f"{label} is unavailable") from error
    if resolved != path or stat.S_ISLNK(metadata.st_mode):
        raise LocalRefusal("installation_invalid", f"{label} is not canonical")
    writable_by_others = bool(stat.S_IMODE(metadata.st_mode) & 0o022)
    if directory:
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or writable_by_others
        ):
            raise LocalRefusal(
                "installation_invalid",
                f"{label} is not an owner-controlled directory",
            )
    elif (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or writable_by_others
        or not os.access(path, os.X_OK)
    ):
        raise LocalRefusal("installation_invalid", f"{label} is not an owned executable")
    return path


def _read_private_file(path: Path) -> bytes:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        raise LocalRefusal(
            "installation_unavailable",
            "owner-local installation is unavailable",
        ) from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_nlink != 1
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_size > MAX_INSTALLATION_BYTES
        ):
            raise LocalRefusal(
                "installation_invalid",
                "owner-local installation file is invalid",
            )
        raw = os.read(descriptor, MAX_INSTALLATION_BYTES + 1)
        after = os.fstat(descriptor)
        if (
            len(raw) > MAX_INSTALLATION_BYTES
            or before.st_dev != after.st_dev
            or before.st_ino != after.st_ino
            or before.st_size != after.st_size
            or before.st_mtime_ns != after.st_mtime_ns
        ):
            raise LocalRefusal(
                "installation_invalid",
                "owner-local installation changed while reading",
            )
        return raw
    finally:
        os.close(descriptor)


def load_installation(path: Path = INSTALLATION_PATH) -> Installation:
    raw = _read_private_file(path)
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise LocalRefusal(
            "installation_invalid",
            "owner-local installation is not valid JSON",
        ) from error
    if (
        not isinstance(value, dict)
        or set(value) != INSTALLATION_KEYS
        or canonical_bytes(value) + b"\n" != raw
        or value.get("document_type") != INSTALLATION_DOCUMENT_TYPE
        or type(value.get("schema_version")) is not int
        or value.get("schema_version") != INSTALLATION_SCHEMA_VERSION
    ):
        raise LocalRefusal(
            "installation_invalid",
            "owner-local installation is outside the closed schema",
        )
    items = value.get("repositories")
    if not isinstance(items, list) or not 1 <= len(items) <= MAX_REPOSITORIES:
        raise LocalRefusal(
            "installation_invalid",
            "owner-local repository bindings are invalid",
        )
    repositories: list[RepositoryBinding] = []
    seen: set[str] = set()
    for item in items:
        if not isinstance(item, dict) or set(item) != REPOSITORY_KEYS:
            raise LocalRefusal(
                "installation_invalid",
                "owner-local repository binding is invalid",
            )
        repository = item["repository"]
        if (
            not isinstance(repository, str)
            or semantic.REPOSITORY_PATTERN.fullmatch(repository) is None
            or repository in seen
        ):
            raise LocalRefusal(
                "installation_invalid",
                "owner-local repository identity is invalid",
            )
        seen.add(repository)
        repositories.append(
            RepositoryBinding(
                repository,
                _exact_absolute_path(
                    item["checkout"],
                    "resident checkout",
                    directory=True,
                ),
            )
        )
    return Installation(tuple(repositories))


def installed_repo_query_program() -> Path:
    return _exact_absolute_path(
        os.fspath(REPO_QUERY_PROGRAM),
        "repo query program",
        directory=False,
    )


def _control_error(code: str) -> dict[str, object]:
    return {
        "document_type": "glaeda-owner-local-error",
        "schema_version": 1,
        "code": code,
        "authority": {
            "authorizes_execution": False,
            "authorizes_redispatch": False,
        },
    }


def _semantic_refusal(
    request: semantic.SemanticRequest,
    code: str,
) -> dict[str, object]:
    return semantic.refused_receipt(
        request,
        semantic.ContractRefusal(code, code),
    )


def _run(
    argv: list[str],
    *,
    timeout: float,
    maximum_output: int,
) -> subprocess.CompletedProcess[bytes]:
    try:
        completed = subprocess.run(
            argv,
            cwd=CONTROL_ROOT,
            env=owned_task.closed_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise LocalRefusal("local_control_unavailable", "local control command failed") from error
    if (
        len(completed.stdout) > maximum_output
        or len(completed.stderr) > MAX_ERROR_OUTPUT_BYTES
    ):
        raise LocalRefusal(
            "local_control_invalid",
            "local control command exceeded its output ceiling",
        )
    return completed


def _decode_json_output(raw: bytes, label: str) -> dict[str, object]:
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise LocalRefusal("local_control_invalid", f"{label} is not valid JSON") from error
    if not isinstance(value, dict):
        raise LocalRefusal("local_control_invalid", f"{label} is not an object")
    return value


def observe_admission() -> dict[str, object]:
    completed = _run(
        [
            sys.executable,
            os.fspath(CONTROL_ROOT / "scripts" / "owned-admission-observe"),
            "--root",
            os.fspath(ADMISSION_ROOT),
        ],
        timeout=10,
        maximum_output=4096,
    )
    if completed.returncode != 0 or completed.stderr:
        raise LocalRefusal(
            "admission_unavailable",
            "owned admission observation failed",
        )
    return _decode_json_output(completed.stdout, "owned admission observation")


def run_repo_query(
    compiled: semantic.CompiledRequest,
    installation: Installation,
) -> dict[str, object]:
    request = compiled.request
    assert request.source is not None
    checkout = installation.checkout_for(request.source.repository)
    completed = _run(
        [
            os.fspath(installed_repo_query_program()),
            "--checkout",
            os.fspath(checkout),
            "--project",
            f"github.com/{request.source.repository}",
            "--base",
            str(request.parameters["base_commit"]),
            "--head",
            request.source.commit,
            "--tree",
            request.source.tree,
            "--max-patch-bytes",
            str(request.parameters["max_patch_bytes"]),
            "--output",
            "json",
        ],
        timeout=60,
        maximum_output=semantic.MAX_REPO_QUERY_RESULT_BYTES,
    )
    if completed.returncode != 0 or completed.stderr:
        return _semantic_refusal(request, "repo_query_refused")
    try:
        report = _decode_json_output(completed.stdout, "repo query result")
        return semantic.repo_query_receipt(compiled, report)
    except (LocalRefusal, semantic.ContractRefusal):
        return _semantic_refusal(request, "repo_query_result_invalid")


def _verifier_problem(stderr: bytes) -> str | None:
    try:
        value = json.loads(stderr, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError):
        return None
    if (
        not isinstance(value, dict)
        or value.get("document_type") != "glaeda-verify-focused-error"
        or type(value.get("schema_version")) is not int
        or value.get("schema_version") != 1
        or value.get("authority") != "none"
        or not isinstance(value.get("problem"), str)
    ):
        return None
    return value["problem"]


def bind_verify_request(compiled: semantic.CompiledRequest) -> dict[str, object] | None:
    """Persist the accepted semantic identity before any admission wait or physical launch."""
    request = compiled.request
    if request.operation != semantic.OP_VERIFY_NAMED or compiled.internal is None:
        return _semantic_refusal(request, "internal_contract_error")
    try:
        state_root = focused.private_state_directory(os.fspath(SHARED_VERIFY_STATE_ROOT))
        focused.bind_semantic_request(state_root, request.request_id, compiled.internal)
    except focused.SemanticRequestConflict:
        return _semantic_refusal(request, "request_conflict")
    except (owned_task.Refusal, OSError):
        return _semantic_refusal(request, "semantic_state_unavailable")
    return None


def run_verify(
    compiled: semantic.CompiledRequest,
    installation: Installation,
) -> dict[str, object]:
    request = compiled.request
    assert request.source is not None
    assert compiled.internal is not None
    observation = observe_admission()
    try:
        semantic.validate_status_observation(observation)
    except semantic.ContractRefusal:
        return _semantic_refusal(request, "admission_unavailable")
    outcome = observation["outcome"]
    reason = observation["reason"]
    if outcome == "wait":
        return semantic.waiting_receipt(compiled, reason)
    if outcome != "ready":
        return _semantic_refusal(request, "admission_unavailable")

    checkout = installation.checkout_for(request.source.repository)
    internal = compiled.internal
    completed = _run(
        [
            sys.executable,
            os.fspath(CONTROL_ROOT / "scripts" / "verify-focused"),
            "run",
            "--repository-root",
            os.fspath(checkout),
            "--state-root",
            os.fspath(SHARED_VERIFY_STATE_ROOT),
            "--cargo-root",
            os.fspath(CARGO_ROOT),
            "--rustup-root",
            os.fspath(RUSTUP_ROOT),
            "--repository",
            request.source.repository,
            "--commit",
            request.source.commit,
            "--tree",
            request.source.tree,
            "--profile-generation",
            internal.profile_generation,
            "--command-fingerprint",
            internal.command_fingerprint,
            "--semantic-request-id",
            request.request_id,
            "--admission-root",
            os.fspath(ADMISSION_ROOT),
        ],
        timeout=internal.profile.deadline_seconds + 90,
        maximum_output=MAX_CONTROL_OUTPUT_BYTES,
    )
    if completed.returncode != 0:
        if _verifier_problem(completed.stderr) == AMBIGUOUS_PROBLEM:
            return semantic.ambiguous_receipt(compiled)
        return _semantic_refusal(request, "verification_refused")
    if completed.stderr:
        return _semantic_refusal(request, "verification_result_invalid")
    try:
        workload_receipt = _decode_json_output(
            completed.stdout,
            "verification receipt",
        )
        return semantic.terminal_verify_receipt(compiled, workload_receipt)
    except (LocalRefusal, semantic.ContractRefusal):
        return _semantic_refusal(request, "verification_result_invalid")


def execute(
    request: semantic.SemanticRequest,
    installation_path: Path = INSTALLATION_PATH,
) -> dict[str, object]:
    try:
        compiled = semantic.compile_request(request)
    except semantic.ContractRefusal as error:
        return semantic.refused_receipt(request, error)

    if request.operation == semantic.OP_CAPABILITIES:
        return semantic.capabilities_receipt(compiled)

    if request.operation == semantic.OP_STATUS:
        try:
            return semantic.status_receipt(compiled, observe_admission())
        except (LocalRefusal, semantic.ContractRefusal):
            return _semantic_refusal(request, "admission_unavailable")

    if request.operation == semantic.OP_VERIFY_NAMED:
        refusal = bind_verify_request(compiled)
        if refusal is not None:
            return refusal

    try:
        installation = load_installation(installation_path)
    except LocalRefusal as error:
        return _semantic_refusal(request, error.code)

    if request.operation == semantic.OP_REPO_QUERY:
        try:
            return run_repo_query(compiled, installation)
        except LocalRefusal as error:
            return _semantic_refusal(request, error.code)
    if request.operation == semantic.OP_VERIFY_NAMED:
        try:
            return run_verify(compiled, installation)
        except LocalRefusal as error:
            return _semantic_refusal(request, error.code)
    return _semantic_refusal(request, "unsupported_operation")


def main() -> int:
    raw = sys.stdin.buffer.read(semantic.MAX_REQUEST_BYTES + 1)
    try:
        request = semantic.decode_request(raw)
    except semantic.ContractRefusal as error:
        sys.stderr.buffer.write(canonical_bytes(_control_error(error.code)) + b"\n")
        return 2
    receipt = execute(request)
    sys.stdout.buffer.write(semantic.canonical_bytes(receipt) + b"\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
