#!/usr/bin/env python3
"""Project one bounded, expiring owned-workstation capability snapshot."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time
from typing import Any


SCHEMA = "glaeda-owned-workstation-capability/v1"
MAX_SNAPSHOT_BYTES = 4096
MAX_TTL_SECONDS = 1800
MAX_PROJECTS = 8
MAX_OBSERVATION_BYTES = 32 * 1024
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
OID_RE = re.compile(r"[0-9a-f]{40}\Z")
NODE_RE = re.compile(r"[a-z0-9][a-z0-9-]{1,63}\Z")
PROJECT_RE = re.compile(r"github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")


class SnapshotError(RuntimeError):
    """A bounded capability projection refusal."""


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def python_evidence(path: Path) -> dict[str, str]:
    resolved = path.resolve(strict=True)
    if not resolved.is_file():
        raise SnapshotError("Python interpreter must resolve to a regular file")
    completed = subprocess.run(
        [str(resolved), "--version"],
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=5,
        env={"LC_ALL": "C", "PATH": "/usr/bin:/bin"},
    )
    version = (completed.stdout + completed.stderr).decode("ascii", errors="replace").strip()
    match = re.fullmatch(r"Python (3\.14\.[0-9]+(?:[+a-z0-9.-]*)?)", version, re.IGNORECASE)
    if completed.returncode != 0 or match is None:
        raise SnapshotError("Owned-workstation capability requires Python 3.14.x")
    return {"executableSha256": sha256_file(resolved), "version": match.group(1)}


def workspace_receipt(repository_root: Path) -> dict[str, Any]:
    completed = subprocess.run(
        [str(repository_root / "scripts" / "bootstrap"), "--output", "json"],
        cwd=repository_root,
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    if completed.returncode not in (0, 1):
        raise SnapshotError("Workspace capability observation failed")
    try:
        value = json.loads(completed.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SnapshotError("Workspace capability observation was not JSON") from error
    if not isinstance(value, dict):
        raise SnapshotError("Workspace capability observation must be an object")
    return value


def terminate_process_group(process: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except OSError:
        if process.poll() is None:
            process.kill()
    try:
        process.wait(timeout=1)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def bounded_process_stdout(
    argv: list[str],
    *,
    timeout: float,
    max_stdout_bytes: int,
    env: dict[str, str],
) -> tuple[int, bytes]:
    # Every failure below raises SnapshotError with a constant message. argv
    # carries the checkout path, and OSError and TimeoutExpired both render it
    # into str(error), which main() prints to stderr as JSON.
    try:
        process = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env,
            start_new_session=True,
        )
    except OSError as error:
        raise SnapshotError("Bounded observation could not be started") from error
    if process.stdout is None:
        terminate_process_group(process)
        raise SnapshotError("Bounded observer stdout is unavailable")
    output = bytearray()
    deadline = time.monotonic() + timeout
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(argv, timeout)
            if not selector.select(remaining):
                raise subprocess.TimeoutExpired(argv, timeout)
            chunk = process.stdout.read1(
                min(8192, max_stdout_bytes + 1 - len(output))
            )
            if not chunk:
                break
            output.extend(chunk)
            if len(output) > max_stdout_bytes:
                raise SnapshotError("Repo-query project observation exceeded output limit")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(argv, timeout)
        return process.wait(timeout=remaining), bytes(output)
    except subprocess.TimeoutExpired:
        # Not re-raised: TimeoutExpired stringifies its argv, which holds the
        # checkout path. Keep the message constant.
        terminate_process_group(process)
        raise SnapshotError("Bounded observation timed out") from None
    except BaseException:
        # Anything else, including a selector OSError or KeyboardInterrupt,
        # must still kill and reap the child rather than orphan it on the
        # checkout.
        terminate_process_group(process)
        raise
    finally:
        selector.close()
        process.stdout.close()


def canonical_repository(repository: str) -> str:
    if not REPOSITORY_RE.fullmatch(repository):
        raise SnapshotError("Repo-query capability repository is invalid")
    return repository.casefold()


def repo_query_project_observation(observer: Path, checkout: Path) -> dict[str, Any]:
    try:
        resolved_observer = observer.resolve(strict=True)
        resolved_checkout = checkout.resolve(strict=True)
    except OSError as error:
        raise SnapshotError("Repo-query project observation input is unavailable") from error
    if not resolved_observer.is_file() or not resolved_checkout.is_dir():
        raise SnapshotError("Repo-query project observation input is invalid")
    returncode, stdout = bounded_process_stdout(
        [
            str(resolved_observer),
            "--checkout",
            str(resolved_checkout),
            "--output",
            "json",
        ],
        timeout=15,
        max_stdout_bytes=MAX_OBSERVATION_BYTES,
        env={"LC_ALL": "C", "PATH": "/usr/bin:/bin"},
    )
    if returncode != 0:
        raise SnapshotError("Repo-query project observation failed")
    try:
        report = json.loads(stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SnapshotError("Repo-query project observation was not bounded JSON") from error
    return repo_query_project_from_report(report)


def repo_query_project_from_report(report: object) -> dict[str, Any]:
    document = exact_object(report, "project observation")
    if (
        document.get("document_type") != "glaeda-project-observation"
        or document.get("schema_version") != 1
        or document.get("authority") != "observation_only"
    ):
        raise SnapshotError("Repo-query project observation contract changed")
    observation = exact_object(document.get("observation"), "project observation payload")
    repository = observation.get("primary_project")
    if (
        observation.get("schema_version") != 2
        or observation.get("identity_generation") != "glaeda_v2"
        or observation.get("source_ambiguous") is not False
        or observation.get("owner_matches_parent") is not True
        or not isinstance(repository, str)
        or not PROJECT_RE.fullmatch(repository)
    ):
        raise SnapshotError("Repo-query project identity is not canonical")
    return {
        "heatClass": "resident_cold",
        "repository": canonical_repository(repository.removeprefix("github.com/")),
        "source": {
            "commitOid": oid(observation.get("commit"), "project source commit"),
            "treeOid": oid(observation.get("tree"), "project source tree"),
        },
        "sourceObjectClass": "exact_commit_and_tree_present",
        "verificationProfiles": ["repo-query/v1"],
    }


def normalize_repo_query_project(value: object) -> dict[str, Any]:
    project = exact_object(value, "repo-query capability project")
    if set(project) != {
        "heatClass",
        "repository",
        "source",
        "sourceObjectClass",
        "verificationProfiles",
    }:
        raise SnapshotError("Repo-query capability project fields changed")
    repository = project.get("repository")
    source = exact_object(project.get("source"), "repo-query capability source")
    if (
        project.get("heatClass") != "resident_cold"
        or not isinstance(repository, str)
        or not REPOSITORY_RE.fullmatch(repository)
        or set(source) != {"commitOid", "treeOid"}
        or project.get("sourceObjectClass") != "exact_commit_and_tree_present"
        or project.get("verificationProfiles") != ["repo-query/v1"]
    ):
        raise SnapshotError("Repo-query capability project is invalid")
    return {
        "heatClass": "resident_cold",
        "repository": canonical_repository(repository),
        "source": {
            "commitOid": oid(source.get("commitOid"), "project source commit"),
            "treeOid": oid(source.get("treeOid"), "project source tree"),
        },
        "sourceObjectClass": "exact_commit_and_tree_present",
        "verificationProfiles": ["repo-query/v1"],
    }


def build_snapshot(
    receipt: dict[str, Any],
    *,
    node_id: str,
    node_generation: int,
    os_class: str,
    architecture_class: str,
    glaeda_runtime_sha256: str,
    python: dict[str, str],
    profile_generation: str,
    repo_query_projects: list[dict[str, Any]] | None = None,
    verification_profile_generations: dict[str, str] | None = None,
    observed_at: dt.datetime,
    ttl_seconds: int,
) -> dict[str, Any]:
    if not NODE_RE.fullmatch(node_id):
        raise SnapshotError("Node ID is invalid")
    if not 1 <= node_generation <= 2**31 - 1:
        raise SnapshotError("Node generation is invalid")
    if os_class not in {"linux", "macos"}:
        raise SnapshotError("OS class is invalid")
    if architecture_class not in {"x86_64", "arm64"}:
        raise SnapshotError("Architecture class is invalid")
    if not SHA256_RE.fullmatch(glaeda_runtime_sha256):
        raise SnapshotError("Glaeda runtime digest is invalid")
    if not SHA256_RE.fullmatch(profile_generation):
        raise SnapshotError("Profile generation is invalid")
    verification_profile_generations = verification_profile_generations or {}
    repo_query_projects = [
        normalize_repo_query_project(project) for project in (repo_query_projects or [])
    ]
    profile_classes = {
        "verify-focused/v1": "verify_focused",
        "verify-required/v1": "verify_required",
    }
    if (
        any(profile_id not in profile_classes for profile_id in verification_profile_generations)
        or any(
            not SHA256_RE.fullmatch(generation)
            for generation in verification_profile_generations.values()
        )
    ):
        raise SnapshotError("Verification profile generation is invalid")
    if not 30 <= ttl_seconds <= MAX_TTL_SECONDS:
        raise SnapshotError("Snapshot TTL must be between 30 and 1800 seconds")
    if observed_at.tzinfo is None or observed_at.utcoffset() != dt.timedelta(0):
        raise SnapshotError("Observation time must be UTC")

    source = exact_object(receipt.get("source"), "workspace source")
    commit_oid = oid(source.get("commit"), "source commit")
    tree_oid = oid(source.get("tree"), "source tree")
    fingerprint = sha256(receipt.get("capability_fingerprint"), "workspace capability")
    state = receipt.get("state")
    if state not in {"ready", "ready_with_declared_deviations", "blocked"}:
        raise SnapshotError("Workspace capability state is invalid")
    repository = exact_object(receipt.get("repository_root"), "repository root").get("repository")
    if repository != "teamleaderleo/glaeda":
        raise SnapshotError("Workspace repository identity is not Glaeda")

    caches = receipt.get("declared_cache_paths")
    if not isinstance(caches, list) or len(caches) > 8:
        raise SnapshotError("Workspace cache evidence is invalid")
    cargo_target = next((entry for entry in caches if isinstance(entry, dict) and entry.get("name") == "cargo-target"), None)
    hot = bool(
        cargo_target
        and cargo_target.get("exists") is True
        and cargo_target.get("directory") is True
        and cargo_target.get("ownership") == "current-user"
        and cargo_target.get("symlink_alias_detected") is False
    )
    verification_profiles = receipt.get("next_verification_profiles")
    if (
        not isinstance(verification_profiles, list)
        or not verification_profiles
        or len(verification_profiles) > 8
        or any(not isinstance(value, str) or len(value) > 80 for value in verification_profiles)
    ):
        raise SnapshotError("Workspace verification profiles are invalid")

    observed = observed_at.isoformat(timespec="milliseconds").replace("+00:00", "Z")
    expires = (observed_at + dt.timedelta(seconds=ttl_seconds)).isoformat(
        timespec="milliseconds"
    ).replace("+00:00", "Z")
    projects = [{
        "heatClass": "resident_hot" if hot else "resident_cold",
        "repository": "teamleaderleo/glaeda",
        "source": {"commitOid": commit_oid, "treeOid": tree_oid},
        "sourceObjectClass": "exact_commit_and_tree_present",
        "verificationProfiles": sorted(
            set(verification_profiles) | set(verification_profile_generations)
        ),
    }, *repo_query_projects]
    if len(projects) > MAX_PROJECTS:
        raise SnapshotError("Owned-workstation capability has too many projects")
    repository_keys = [str(project.get("repository")).casefold() for project in projects]
    if len(repository_keys) != len(set(repository_keys)):
        raise SnapshotError("Owned-workstation capability has duplicate projects")
    projects.sort(key=lambda project: str(project.get("repository")).casefold())

    snapshot = {
        "admission": {
            "activeWorkloadsClass": "unobserved",
            "availabilityClass": "available" if state != "blocked" else "blocked",
            "pressureClass": "unobserved",
        },
        "advisoryOnly": True,
        "authorizesDispatch": False,
        "authorizesExecution": False,
        "expiresAt": expires,
        "node": {
            "architectureClass": architecture_class,
            "generation": node_generation,
            "id": node_id,
            "osClass": os_class,
        },
        "observedAt": observed,
        "producer": {
            "glaedaRuntimeSha256": glaeda_runtime_sha256,
            "python": python,
            "workspaceCapabilitySha256": fingerprint,
        },
        "profiles": [{
            "class": "repo_query",
            "id": "repo-query/v1",
            "versionSha256": profile_generation,
        }, *[
            {
                "class": profile_classes[profile_id],
                "id": profile_id,
                "versionSha256": generation,
            }
            for profile_id, generation in sorted(verification_profile_generations.items())
        ]],
        "projects": projects,
        "schema": SCHEMA,
    }
    if len(canonical_json(snapshot)) > MAX_SNAPSHOT_BYTES:
        raise SnapshotError("Owned-workstation capability snapshot exceeds 4096 bytes")
    return snapshot


def exact_object(value: object, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise SnapshotError(f"{label} must be an object")
    return value


def oid(value: object, label: str) -> str:
    if not isinstance(value, str) or not OID_RE.fullmatch(value):
        raise SnapshotError(f"{label} is invalid")
    return value


def sha256(value: object, label: str) -> str:
    if not isinstance(value, str) or not SHA256_RE.fullmatch(value):
        raise SnapshotError(f"{label} digest is invalid")
    return value


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("--node-id", required=True)
    value.add_argument("--node-generation", required=True, type=int)
    value.add_argument("--os-class", required=True, choices=("linux", "macos"))
    value.add_argument("--architecture", required=True, choices=("x86_64", "arm64"))
    runtime = value.add_mutually_exclusive_group(required=True)
    runtime.add_argument("--glaeda-runtime")
    runtime.add_argument("--glaeda-runtime-sha256")
    value.add_argument("--python-interpreter", required=True)
    value.add_argument("--profile-generation", required=True)
    value.add_argument("--project-observer")
    value.add_argument("--repo-query-checkout", action="append", default=[])
    value.add_argument("--verify-focused-generation")
    value.add_argument("--verify-required-generation")
    value.add_argument("--ttl-seconds", type=int, default=180)
    return value


def main() -> int:
    try:
        args = parser().parse_args()
        root = Path(__file__).resolve().parent.parent
        if args.glaeda_runtime_sha256 is not None:
            runtime_sha256 = sha256(args.glaeda_runtime_sha256, "Glaeda runtime")
        else:
            runtime = Path(args.glaeda_runtime).resolve(strict=True)
            if not runtime.is_file():
                raise SnapshotError("Glaeda runtime must resolve to a regular file")
            runtime_sha256 = sha256_file(runtime)
        if bool(args.project_observer) != bool(args.repo_query_checkout):
            raise SnapshotError(
                "Additional repo-query checkouts require one exact project observer"
            )
        repo_query_projects = [
            repo_query_project_observation(Path(args.project_observer), Path(checkout))
            for checkout in args.repo_query_checkout
        ]
        snapshot = build_snapshot(
            workspace_receipt(root),
            node_id=args.node_id,
            node_generation=args.node_generation,
            os_class=args.os_class,
            architecture_class=args.architecture,
            glaeda_runtime_sha256=runtime_sha256,
            python=python_evidence(Path(args.python_interpreter)),
            profile_generation=args.profile_generation,
            repo_query_projects=repo_query_projects,
            verification_profile_generations={
                profile_id: generation
                for profile_id, generation in (
                    ("verify-focused/v1", args.verify_focused_generation),
                    ("verify-required/v1", args.verify_required_generation),
                )
                if generation is not None
            },
            observed_at=dt.datetime.now(dt.UTC),
            ttl_seconds=args.ttl_seconds,
        )
        sys.stdout.buffer.write(canonical_json(snapshot))
        return 0
    except (OSError, SnapshotError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"error": str(error)}, sort_keys=True, separators=(",", ":")), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
