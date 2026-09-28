#!/usr/bin/env python3
"""Project one bounded, expiring owned-workstation capability snapshot."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Any


SCHEMA = "glaeda-owned-workstation-capability/v1"
MAX_SNAPSHOT_BYTES = 4096
MAX_TTL_SECONDS = 1800
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
OID_RE = re.compile(r"[0-9a-f]{40}\Z")
NODE_RE = re.compile(r"[a-z0-9][a-z0-9-]{1,63}\Z")


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
        "projects": [{
            "heatClass": "resident_hot" if hot else "resident_cold",
            "repository": "teamleaderleo/glaeda",
            "source": {"commitOid": commit_oid, "treeOid": tree_oid},
            "sourceObjectClass": "exact_commit_and_tree_present",
            "verificationProfiles": sorted(
                set(verification_profiles) | set(verification_profile_generations)
            ),
        }],
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
        snapshot = build_snapshot(
            workspace_receipt(root),
            node_id=args.node_id,
            node_generation=args.node_generation,
            os_class=args.os_class,
            architecture_class=args.architecture,
            glaeda_runtime_sha256=runtime_sha256,
            python=python_evidence(Path(args.python_interpreter)),
            profile_generation=args.profile_generation,
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
