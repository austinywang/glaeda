"""Pure mapping from trusted runner context to issue #153 observations.

This module never acquires runner identity. A Glaeda-owned adapter must open
protected durable state, validate provenance, and pass the resulting typed
context directly to :func:`map_validated_runner_context`.
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass, field
from pathlib import Path

from .probe import public_issue

IDENTIFIER_RE = re.compile(r"^[A-Za-z0-9._-]{1,96}$")
REPOSITORY_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
TRUSTED_SOURCE_KIND = "glaeda-protected-state-v2"

CAPABILITY_ID_BY_TOOL = {
    "python3": "python3",
    "git": "git",
    "cargo": "cargo",
    "rustc": "rustc",
    "rustfmt": "rustfmt",
    "clippy": "clippy",
    "cargo-nextest": "nextest",
    "just": "just",
    "podman": "podman",
}
BACKEND_CAPABILITY_IDS = {
    "cargo-test": "cargo",
    "cargo-nextest": "nextest",
}
FORMATTER_CAPABILITY_IDS = {"rustfmt": "rustfmt"}


@dataclass(frozen=True)
class DescriptorIdentity:
    """Stable descriptor identity observed by the trusted adapter."""

    device: int
    inode: int


@dataclass(frozen=True)
class RunnerContextProvenance:
    """Descriptor-derived provenance facts supplied by Glaeda-owned code.

    These fields are observations, not a file-path trust mechanism. The future
    producer must derive them while holding descriptors opened below the
    canonical protected state root.
    """

    source_kind: str
    evidence_digest: str
    state_root_descriptor_relative: bool
    installation_descriptor_opened: bool
    installation_regular_file: bool
    installation_owner_uid: int
    expected_owner_uid: int
    installation_mode: int
    installation_link_count: int
    state_parents_owner_matches: bool
    state_parents_mode_private: bool
    state_parents_symlink_free: bool
    workspace_resolved_from_durable_state: bool
    cache_resolved_from_durable_state: bool
    identity_bound_from_durable_state: bool
    filesystem_observed_independently: bool
    workspace_is_directory: bool
    cache_is_directory: bool
    workspace_owner_uid: int
    cache_owner_uid: int
    workspace_alias_free: bool
    cache_alias_free: bool
    cache_contained_in_workspace: bool
    descriptor_identity_before: DescriptorIdentity
    descriptor_identity_after: DescriptorIdentity
    path_identity_after: DescriptorIdentity
    repository_controlled_source: bool = False


@dataclass(frozen=True)
class ValidatedRunnerCacheContext:
    """Cache identity selected from protected durable state."""

    cache_id: str
    owner_workspace_id: str
    namespace_digest: str
    present: bool
    path: Path = field(repr=False)


@dataclass(frozen=True)
class ValidatedRunnerContext:
    """Typed context emitted by a trusted Glaeda-owned adapter."""

    installation_id: str
    workspace_id: str
    repository: str
    cache: ValidatedRunnerCacheContext
    provenance: RunnerContextProvenance
    root: Path = field(repr=False)


def _issue(code: str, message: str) -> dict[str, str]:
    return public_issue(code, message)


def _valid_identifier(value: object) -> bool:
    return isinstance(value, str) and IDENTIFIER_RE.fullmatch(value) is not None


def _valid_private_path(path: Path) -> bool:
    return path.is_absolute() and all(part not in (".", "..") for part in path.parts)


def _is_beneath(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return path != root
    except ValueError:
        return False


def _safe_private_mode(mode: int) -> bool:
    return isinstance(mode, int) and 0 <= mode <= 0o7777 and mode & 0o022 == 0


def _valid_descriptor_identity(identity: DescriptorIdentity) -> bool:
    return identity.device >= 0 and identity.inode > 0


def capability_observations(
    tools: list[dict[str, object]], mapping: dict[str, str] | None = None
) -> list[dict[str, object]]:
    """Map every observed tool once to the canonical CapabilityId vocabulary."""

    mapping = CAPABILITY_ID_BY_TOOL if mapping is None else mapping
    observations: list[dict[str, object]] = []
    seen: set[str] = set()
    for tool in tools:
        name = str(tool["name"])
        capability = mapping.get(name)
        if capability is None or not _valid_identifier(capability) or capability in seen:
            raise ValueError("invalid or duplicate capability mapping")
        seen.add(capability)
        observations.append(
            {"capability": capability, "available": bool(tool["available"])}
        )
    return observations


def _mib_to_bytes(value: object) -> int | None:
    if not isinstance(value, int) or value < 0:
        return None
    maximum = ((1 << 64) - 1) // (1024 * 1024)
    return None if value > maximum else value * 1024 * 1024


def _provenance_issues(
    context: ValidatedRunnerContext,
) -> list[dict[str, str]]:
    provenance = context.provenance
    issues: list[dict[str, str]] = []

    if (
        provenance.source_kind != TRUSTED_SOURCE_KIND
        or provenance.repository_controlled_source
    ):
        issues.append(
            _issue(
                "runner_context_untrusted_source",
                "runner identity must originate in protected Glaeda state",
            )
        )
    if not provenance.state_root_descriptor_relative or not provenance.installation_descriptor_opened:
        issues.append(
            _issue(
                "runner_context_descriptor_boundary_unproven",
                "runner state must be opened descriptor-relative from the canonical state root",
            )
        )
    if not provenance.identity_bound_from_durable_state:
        issues.append(
            _issue(
                "runner_context_identity_unbound",
                "runner identity fields must be bound from protected durable state",
            )
        )
    if not provenance.workspace_resolved_from_durable_state or not provenance.cache_resolved_from_durable_state:
        issues.append(
            _issue(
                "runner_context_state_resolution_unproven",
                "workspace and cache identities must be resolved from protected durable state",
            )
        )
    if not provenance.filesystem_observed_independently:
        issues.append(
            _issue(
                "runner_context_filesystem_unobserved",
                "workspace and cache filesystem facts require independent observation",
            )
        )
    if not provenance.state_parents_owner_matches:
        issues.append(
            _issue(
                "runner_context_state_parent_owner_mismatch",
                "protected state parent ownership does not match the trusted producer",
            )
        )
    if not provenance.state_parents_mode_private:
        issues.append(
            _issue(
                "runner_context_state_parent_writable",
                "protected state parents permit group or other writes",
            )
        )
    if not provenance.state_parents_symlink_free:
        issues.append(
            _issue(
                "runner_context_state_parent_symlink",
                "protected state resolution crossed a symbolic-link parent",
            )
        )
    if not provenance.installation_regular_file:
        issues.append(
            _issue(
                "runner_context_installation_type_invalid",
                "installation state descriptor must identify one regular file",
            )
        )
    if provenance.installation_owner_uid != provenance.expected_owner_uid:
        issues.append(
            _issue(
                "runner_context_installation_owner_mismatch",
                "installation state owner does not match the trusted producer",
            )
        )
    if not _safe_private_mode(provenance.installation_mode):
        issues.append(
            _issue(
                "runner_context_installation_mode_unsafe",
                "installation state permits group or other writes",
            )
        )
    if provenance.installation_link_count != 1:
        issues.append(
            _issue(
                "runner_context_installation_hard_link",
                "installation state must have exactly one filesystem link",
            )
        )
    identities = (
        provenance.descriptor_identity_before,
        provenance.descriptor_identity_after,
        provenance.path_identity_after,
    )
    if any(not _valid_descriptor_identity(identity) for identity in identities):
        issues.append(
            _issue(
                "runner_context_descriptor_identity_invalid",
                "installation descriptor identity observation is invalid",
            )
        )
    elif provenance.descriptor_identity_before != provenance.descriptor_identity_after:
        issues.append(
            _issue(
                "runner_context_installation_replaced",
                "installation state changed while the trusted adapter observed it",
            )
        )
    elif provenance.descriptor_identity_after != provenance.path_identity_after:
        issues.append(
            _issue(
                "runner_context_path_race",
                "installation path no longer identifies the opened descriptor",
            )
        )
    if not provenance.workspace_is_directory or not provenance.cache_is_directory:
        issues.append(
            _issue(
                "runner_context_filesystem_type_mismatch",
                "workspace and cache observations must identify directories",
            )
        )
    if (
        provenance.workspace_owner_uid != provenance.expected_owner_uid
        or provenance.cache_owner_uid != provenance.expected_owner_uid
    ):
        issues.append(
            _issue(
                "runner_context_filesystem_owner_mismatch",
                "workspace and cache ownership must match the trusted producer",
            )
        )
    if not provenance.workspace_alias_free or not provenance.cache_alias_free:
        issues.append(
            _issue(
                "runner_context_filesystem_alias",
                "workspace and cache observations must be free of symbolic aliases",
            )
        )
    if not provenance.cache_contained_in_workspace:
        issues.append(
            _issue(
                "runner_context_cache_containment_unproven",
                "cache containment beneath the exact workspace is unproven",
            )
        )
    if DIGEST_RE.fullmatch(provenance.evidence_digest) is None:
        issues.append(
            _issue(
                "runner_context_evidence_digest_invalid",
                "trusted runner evidence digest is invalid",
            )
        )
    return issues


def _observation_digest(observation: dict[str, object]) -> str:
    encoded = json.dumps(observation, sort_keys=True, separators=(",", ":")).encode()
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def map_validated_runner_context(
    receipt: dict[str, object], context: ValidatedRunnerContext
) -> dict[str, object]:
    """Map trusted typed context without acquiring or selecting its identity source.

    The return value deliberately has no readiness field. A Glaeda-owned
    adapter may consume ``observation`` only when ``blocking_reasons`` is empty,
    then construct the merged #153 Rust observation types inside the trusted
    process.
    """

    required = receipt.get("required_tools", [])
    optional = receipt.get("optional_tools", [])
    resources = receipt.get("resources")
    repository_root = receipt.get("repository_root")
    source = receipt.get("source")
    caches = receipt.get("declared_cache_paths", [])
    backends = receipt.get("verification_backends", [])
    formatters = receipt.get("formatter_capabilities", [])
    if not all(
        (
            isinstance(required, list),
            isinstance(optional, list),
            isinstance(resources, dict),
            isinstance(repository_root, dict),
            isinstance(source, dict),
            isinstance(caches, list),
            isinstance(backends, list),
            isinstance(formatters, list),
        )
    ):
        return {
            "schema_version": 1,
            "observation": None,
            "blocking_reasons": [
                _issue(
                    "repository_receipt_schema_invalid",
                    "repository workspace receipt cannot be mapped safely",
                )
            ],
        }

    try:
        capabilities = capability_observations([*required, *optional])
    except (KeyError, TypeError, ValueError):
        return {
            "schema_version": 1,
            "observation": None,
            "blocking_reasons": [
                _issue(
                    "repository_capability_mapping_invalid",
                    "repository capability observations are missing or duplicate",
                )
            ],
        }

    available_ids = {str(item["capability"]) for item in capabilities}
    for backend in backends:
        if not isinstance(backend, dict):
            return {
                "schema_version": 1,
                "observation": None,
                "blocking_reasons": [
                    _issue(
                        "repository_capability_mapping_invalid",
                        "verification backend lacks one canonical capability mapping",
                    )
                ],
            }
        capability_id = BACKEND_CAPABILITY_IDS.get(str(backend.get("name")))
        if capability_id is None or capability_id not in available_ids:
            return {
                "schema_version": 1,
                "observation": None,
                "blocking_reasons": [
                    _issue(
                        "repository_capability_mapping_invalid",
                        "verification backend lacks one canonical capability mapping",
                    )
                ],
            }
    for formatter in formatters:
        if not isinstance(formatter, dict):
            return {
                "schema_version": 1,
                "observation": None,
                "blocking_reasons": [
                    _issue(
                        "repository_capability_mapping_invalid",
                        "formatter lacks one canonical capability mapping",
                    )
                ],
            }
        capability_id = FORMATTER_CAPABILITY_IDS.get(str(formatter.get("formatter")))
        if capability_id is None or capability_id not in available_ids:
            return {
                "schema_version": 1,
                "observation": None,
                "blocking_reasons": [
                    _issue(
                        "repository_capability_mapping_invalid",
                        "formatter lacks one canonical capability mapping",
                    )
                ],
            }

    issues = _provenance_issues(context)
    if (
        not _valid_identifier(context.installation_id)
        or not _valid_identifier(context.workspace_id)
        or REPOSITORY_RE.fullmatch(context.repository) is None
        or not _valid_identifier(context.cache.cache_id)
        or not _valid_identifier(context.cache.owner_workspace_id)
        or DIGEST_RE.fullmatch(context.cache.namespace_digest) is None
        or not _valid_private_path(context.root)
        or not _valid_private_path(context.cache.path)
    ):
        issues.append(
            _issue(
                "runner_context_identity_invalid",
                "validated runner context contains an invalid bounded identity",
            )
        )
    if context.cache.owner_workspace_id != context.workspace_id:
        issues.append(
            _issue(
                "runner_context_cache_owner_mismatch",
                "cache owner does not match the exact workspace identity",
            )
        )
    if not _is_beneath(context.cache.path, context.root):
        issues.append(
            _issue(
                "runner_context_cache_escape",
                "cache path must remain beneath the exact workspace root",
            )
        )
    if context.repository != repository_root.get("repository"):
        issues.append(
            _issue(
                "runner_context_repository_mismatch",
                "trusted runner context names a different repository",
            )
        )
    clean = (
        source.get("clean_before") is True
        and source.get("clean_after") is True
        and source.get("cleanliness_unchanged") is True
    )
    if not clean:
        issues.append(
            _issue(
                "runner_context_cleanliness_unavailable",
                "verification-profile mapping requires an exact clean workspace observation",
            )
        )
    memory_bytes = _mib_to_bytes(resources.get("available_memory_mib"))
    swap_bytes = _mib_to_bytes(resources.get("available_swap_mib"))
    if memory_bytes is None or swap_bytes is None:
        issues.append(
            _issue(
                "runner_context_resources_unavailable",
                "verification-profile mapping requires exact memory and swap byte observations",
            )
        )
    target = next(
        (
            item
            for item in caches
            if isinstance(item, dict) and item.get("name") == "cargo-target"
        ),
        None,
    )
    repository_cache_present = bool(
        target
        and target.get("path_class") == "repository-local"
        and target.get("exists") is True
        and target.get("directory") is True
        and target.get("ownership_observed") is True
        and target.get("ownership") == "current-user"
    )
    if not repository_cache_present or not context.cache.present:
        issues.append(
            _issue(
                "runner_context_cache_absent",
                "profile cache must be present with independently established ownership",
            )
        )

    if issues:
        return {
            "schema_version": 1,
            "observation": None,
            "blocking_reasons": issues,
        }

    observation: dict[str, object] = {
        "schema_version": 1,
        "workspace": {
            "installation_id": context.installation_id,
            "workspace_id": context.workspace_id,
            "repository": context.repository,
            "cleanliness": "clean",
        },
        "capabilities": capabilities,
        "resources": {
            "available_memory_bytes": memory_bytes,
            "available_swap_bytes": swap_bytes,
        },
        "cache": {
            "cache_id": context.cache.cache_id,
            "owner_workspace_id": context.cache.owner_workspace_id,
            "namespace_digest": context.cache.namespace_digest,
            "present": True,
        },
        "trusted_evidence_digest": context.provenance.evidence_digest,
        "private_paths_retained_by_adapter": True,
    }
    observation["observation_digest"] = _observation_digest(observation)
    return {
        "schema_version": 1,
        "observation": observation,
        "blocking_reasons": [],
    }
