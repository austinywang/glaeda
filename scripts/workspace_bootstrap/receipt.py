"""Capability receipt construction for the Glaeda workspace preflight."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

from .probe import (
    EXPECTED_REPOSITORY,
    OPTIONAL_COMMANDS,
    PROFILE_NAMES,
    REQUIRED_COMMANDS,
    available_memory_and_swap,
    cache_declarations,
    cache_observation_issues,
    clean_checkout,
    git_identity,
    git_sha,
    package_marker_matches,
    public_issue,
    public_repository_identity,
    publication_readiness,
    python_observation,
    recommended_concurrency,
    repository_root,
    tool_observation,
)

SCHEMA_VERSION = 2
RECEIPT_TYPE = "glaeda-workspace-capability-receipt"


def capability_fingerprint(receipt: dict[str, object]) -> str:
    stable_keys = (
        "schema_version",
        "receipt_type",
        "state",
        "operation",
        "repository_root",
        "required_tools",
        "optional_tools",
        "verification_backends",
        "formatter_capabilities",
        "declared_cache_paths",
        "git_identity",
        "publication_readiness",
        "next_verification_profiles",
        "deviations",
        "blocking_reasons",
    )
    stable = {key: receipt[key] for key in stable_keys}
    source = receipt["source"]
    resources = receipt["resources"]
    assert isinstance(source, dict)
    assert isinstance(resources, dict)
    stable["source"] = {
        key: source[key] for key in ("commit", "tree", "clean_before", "clean_after")
    }
    stable["recommended_concurrency"] = resources["recommended_concurrency"]
    payload = json.dumps(stable, sort_keys=True, separators=(",", ":")).encode()
    return "sha256:" + hashlib.sha256(payload).hexdigest()


def build_receipt(operation: str) -> dict[str, object]:
    deviations: list[dict[str, str]] = []
    blocking: list[dict[str, str]] = []
    cwd = Path.cwd().resolve()
    root = repository_root(cwd)
    if root is None:
        root = cwd
        blocking.append(
            public_issue(
                "repository_root_unresolved",
                "the current directory is not inside a Git worktree",
            )
        )

    cwd_is_root = cwd == root
    if not cwd_is_root:
        blocking.append(
            public_issue(
                "working_directory_not_repository_root",
                "run ./scripts/bootstrap from the repository root",
            )
        )

    marker = package_marker_matches(root)
    lockfile_present = (root / "Cargo.lock").is_file()
    if not marker:
        blocking.append(
            public_issue(
                "repository_marker_mismatch",
                "Cargo.toml does not identify the expected package",
            )
        )
    if not lockfile_present:
        blocking.append(
            public_issue(
                "repository_lockfile_missing",
                "the required Cargo.lock file is unavailable",
            )
        )

    repository = public_repository_identity(root)
    if repository is None:
        deviations.append(
            public_issue(
                "repository_remote_unidentified",
                "origin could not be reduced to a public GitHub repository identity",
            )
        )
    elif repository != EXPECTED_REPOSITORY:
        deviations.append(
            public_issue(
                "repository_remote_differs",
                "origin identifies a Glaeda fork or alternate repository",
            )
        )

    commit = git_sha(root, "HEAD")
    tree = git_sha(root, "HEAD^{tree}")
    if not commit or not tree:
        blocking.append(
            public_issue(
                "source_identity_unavailable",
                "the checkout lacks an exact commit and tree identity",
            )
        )

    clean_before = clean_checkout(root)
    if clean_before is None:
        blocking.append(
            public_issue(
                "checkout_cleanliness_unavailable",
                "Git could not evaluate checkout cleanliness",
            )
        )
    elif not clean_before:
        blocking.append(
            public_issue(
                "checkout_not_clean",
                "the checkout contains tracked or untracked changes",
            )
        )

    required = [python_observation()]
    required.extend(
        tool_observation(name, command, True, root)
        for name, command in REQUIRED_COMMANDS
    )
    optional = [
        tool_observation(name, command, False, root)
        for name, command in OPTIONAL_COMMANDS
    ]
    for observed in required:
        if not observed["available"]:
            blocking.append(
                public_issue(
                    f"required_tool_{observed['name']}_unavailable",
                    f"required tool {observed['name']} is unavailable",
                )
            )
        elif observed["version"] is None:
            blocking.append(
                public_issue(
                    f"required_tool_{observed['name']}_version_unavailable",
                    f"required tool {observed['name']} did not expose a bounded version",
                )
            )
    for observed in optional:
        if not observed["available"]:
            deviations.append(
                public_issue(
                    f"optional_tool_{observed['name']}_unavailable",
                    f"optional tool {observed['name']} is unavailable",
                )
            )
        elif observed["version"] is None:
            deviations.append(
                public_issue(
                    f"optional_tool_{observed['name']}_version_unavailable",
                    f"optional tool {observed['name']} did not expose a bounded version",
                )
            )

    available = {
        str(item["name"]): bool(item["available"]) for item in required + optional
    }
    verification_backends = [
        {
            "name": "cargo-test",
            "required": True,
            "available": bool(available.get("cargo") and marker and lockfile_present),
            "scope_support": ["library", "integration-test", "package", "all-targets"],
        },
        {
            "name": "cargo-nextest",
            "required": False,
            "available": bool(available.get("cargo-nextest") and marker),
            "scope_support": ["library", "integration-test", "package", "all-targets"],
        },
    ]
    formatter_capabilities = [
        {
            "domain": "rust",
            "formatter": "rustfmt",
            "available": bool(available.get("rustfmt")),
            "canonical_check": "cargo fmt --all -- --check",
        }
    ]

    caches = cache_declarations(root)
    cache_deviations, cache_blocking = cache_observation_issues(caches)
    deviations.extend(cache_deviations)
    blocking.extend(cache_blocking)

    available_memory, available_swap = available_memory_and_swap()
    cpus = max(1, os.cpu_count() or 1)
    concurrency = recommended_concurrency(available_memory, cpus)
    if available_memory is None:
        deviations.append(
            public_issue(
                "available_memory_unobserved",
                "available memory could not be observed through the supported Linux interface",
            )
        )
    elif available_memory < 2048:
        deviations.append(
            public_issue(
                "low_available_memory",
                "available memory is below the repository two-gibibyte build guideline",
            )
        )
    if available_swap is None:
        deviations.append(
            public_issue(
                "available_swap_unobserved",
                "available swap could not be observed through the supported Linux interface",
            )
        )

    identity = git_identity(root, operation in ("commit", "publish"))
    if identity["evaluated"] and not identity["ready"]:
        blocking.append(
            public_issue(
                "git_identity_unready",
                "the requested operation requires configured Git author name and email",
            )
        )
    publication = publication_readiness(operation, repository, identity)
    if operation == "publish":
        blocking.append(
            public_issue(
                "publication_authorization_unproven",
                "this slice does not probe or use remote publication credentials",
            )
        )

    clean_after = clean_checkout(root)
    if clean_after is None:
        blocking.append(
            public_issue(
                "checkout_postcondition_unavailable",
                "Git could not evaluate checkout cleanliness after the probe",
            )
        )
    elif not clean_after:
        blocking.append(
            public_issue(
                "checkout_postcondition_dirty",
                "the checkout is not clean after the probe",
            )
        )
    cleanliness_unchanged = clean_before is not None and clean_before == clean_after
    if not cleanliness_unchanged:
        blocking.append(
            public_issue(
                "checkout_cleanliness_changed",
                "checkout cleanliness changed during the probe",
            )
        )

    state = (
        "blocked"
        if blocking
        else "ready_with_declared_deviations"
        if deviations
        else "ready"
    )
    observed_versions = {
        str(item["name"]): item["version"]
        for item in required + optional
        if item["available"] and item["version"]
    }
    receipt: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "receipt_type": RECEIPT_TYPE,
        "state": state,
        "operation": operation,
        "repository_root": {
            "kind": "git-worktree",
            "repository": repository,
            "expected_repository": EXPECTED_REPOSITORY,
            "required_marker": "Cargo.toml",
            "required_lockfile": "Cargo.lock",
            "working_directory": "repository-root",
            "cwd_is_repository_root": cwd_is_root,
            "private_path_exposed": False,
        },
        "source": {
            "commit": commit,
            "tree": tree,
            "clean_before": clean_before,
            "clean_after": clean_after,
            "cleanliness_unchanged": cleanliness_unchanged,
        },
        "required_tools": required,
        "optional_tools": optional,
        "observed_tool_versions": observed_versions,
        "verification_backends": verification_backends,
        "formatter_capabilities": formatter_capabilities,
        "declared_cache_paths": caches,
        "resources": {
            "available_memory_mib": available_memory,
            "available_swap_mib": available_swap,
            "logical_cpu_count": cpus,
            "recommended_concurrency": concurrency,
        },
        "git_identity": identity,
        "publication_readiness": publication,
        "next_verification_profiles": PROFILE_NAMES,
        "deviations": deviations,
        "blocking_reasons": blocking,
    }
    receipt["capability_fingerprint"] = capability_fingerprint(receipt)
    return receipt
