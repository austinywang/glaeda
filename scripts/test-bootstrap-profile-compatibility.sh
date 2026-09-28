#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
temporary_root=$(mktemp -d)
trap 'rm -rf "$temporary_root"' EXIT

# Repository code has no identity-bearing compatibility command or context-file parser.
test ! -e "$repo_root/scripts/bootstrap-profile-compatibility"
if grep -Eq 'json\.loads|os\.open|Path\([^)]*\)\.read_(text|bytes)|(^|[^[:alnum:]_])open\(' \
  "$repo_root/scripts/workspace_bootstrap/profile_bridge.py"; then
  printf 'profile bridge reacquired caller-selected identity input\n' >&2
  exit 1
fi

PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$repo_root/scripts" python3 - "$temporary_root" <<'PY'
from __future__ import annotations

import hashlib
import json
import os
import pathlib
import stat
import sys
from dataclasses import replace

from workspace_bootstrap.profile_bridge import (
    TRUSTED_SOURCE_KIND,
    DescriptorIdentity,
    RunnerContextProvenance,
    ValidatedRunnerCacheContext,
    ValidatedRunnerContext,
    capability_observations,
    map_validated_runner_context,
)

root = pathlib.Path(sys.argv[1])
workspace = root / "repository-workspace"
cache = workspace / "target"
state_root = root / "protected-state"
installation = state_root / "installation.json"
workspace.mkdir()
cache.mkdir()
state_root.mkdir(mode=0o700)
installation.write_text("trusted state fixture", encoding="utf-8")
installation.chmod(0o600)

uid = os.geteuid()
installation_stat = installation.stat()
identity = DescriptorIdentity(installation_stat.st_dev, installation_stat.st_ino)

def digest(label: str) -> str:
    return "sha256:" + hashlib.sha256(label.encode()).hexdigest()


def provenance(**changes: object) -> RunnerContextProvenance:
    base = RunnerContextProvenance(
        source_kind=TRUSTED_SOURCE_KIND,
        evidence_digest=digest("trusted-context"),
        state_root_descriptor_relative=True,
        installation_descriptor_opened=True,
        installation_regular_file=stat.S_ISREG(installation_stat.st_mode),
        installation_owner_uid=installation_stat.st_uid,
        expected_owner_uid=uid,
        installation_mode=stat.S_IMODE(installation_stat.st_mode),
        installation_link_count=installation_stat.st_nlink,
        state_parents_owner_matches=True,
        state_parents_mode_private=True,
        state_parents_symlink_free=True,
        workspace_resolved_from_durable_state=True,
        cache_resolved_from_durable_state=True,
        identity_bound_from_durable_state=True,
        filesystem_observed_independently=True,
        workspace_is_directory=workspace.is_dir(),
        cache_is_directory=cache.is_dir(),
        workspace_owner_uid=workspace.stat().st_uid,
        cache_owner_uid=cache.stat().st_uid,
        workspace_alias_free=True,
        cache_alias_free=True,
        cache_contained_in_workspace=True,
        descriptor_identity_before=identity,
        descriptor_identity_after=identity,
        path_identity_after=identity,
        repository_controlled_source=False,
    )
    return replace(base, **changes)


def context(**changes: object) -> ValidatedRunnerContext:
    base = ValidatedRunnerContext(
        installation_id="runner-a",
        workspace_id="workspace-a",
        repository="teamleaderleo/smolrunner",
        root=workspace,
        cache=ValidatedRunnerCacheContext(
            cache_id="cargo-target",
            owner_workspace_id="workspace-a",
            namespace_digest=digest("cache-namespace"),
            present=True,
            path=cache,
        ),
        provenance=provenance(),
    )
    return replace(base, **changes)


def receipt() -> dict[str, object]:
    tools = [
        ("python3", True),
        ("git", True),
        ("cargo", True),
        ("rustc", True),
        ("rustfmt", True),
        ("clippy", True),
        ("cargo-nextest", True),
        ("just", True),
        ("podman", True),
    ]
    return {
        "repository_root": {"repository": "teamleaderleo/smolrunner"},
        "source": {
            "clean_before": True,
            "clean_after": True,
            "cleanliness_unchanged": True,
        },
        "required_tools": [
            {"name": name, "available": available}
            for name, available in tools[:6]
        ],
        "optional_tools": [
            {"name": name, "available": available}
            for name, available in tools[6:]
        ],
        "verification_backends": [
            {"name": "cargo-test"},
            {"name": "cargo-nextest"},
        ],
        "formatter_capabilities": [{"formatter": "rustfmt"}],
        "declared_cache_paths": [
            {
                "name": "cargo-target",
                "path_class": "repository-local",
                "exists": True,
                "directory": True,
                "ownership_observed": True,
                "ownership": "current-user",
            }
        ],
        "resources": {
            "available_memory_mib": 4096,
            "available_swap_mib": 512,
        },
    }


def codes(result: dict[str, object]) -> set[str]:
    return {
        str(item["code"])
        for item in result["blocking_reasons"]
        if isinstance(item, dict)
    }


assert TRUSTED_SOURCE_KIND == "glaeda-protected-state-v2"
valid = map_validated_runner_context(receipt(), context())
assert valid["blocking_reasons"] == []
observation = valid["observation"]
assert isinstance(observation, dict)
assert "ready" not in valid and "ready" not in observation
assert observation["workspace"] == {
    "installation_id": "runner-a",
    "workspace_id": "workspace-a",
    "repository": "teamleaderleo/smolrunner",
    "cleanliness": "clean",
}
assert observation["cache"] == {
    "cache_id": "cargo-target",
    "owner_workspace_id": "workspace-a",
    "namespace_digest": digest("cache-namespace"),
    "present": True,
}
assert observation["resources"] == {
    "available_memory_bytes": 4096 * 1024 * 1024,
    "available_swap_bytes": 512 * 1024 * 1024,
}
assert observation["trusted_evidence_digest"] == digest("trusted-context")
assert observation["observation_digest"].startswith("sha256:")
public = json.dumps(valid, sort_keys=True)
assert str(workspace) not in public
assert str(cache) not in public
assert str(installation) not in public
ids = [item["capability"] for item in observation["capabilities"]]
assert len(ids) == len(set(ids))

# Provenance refusal matrix. These are typed facts a future Glaeda-owned
# producer must derive while holding protected descriptors.
refusals: list[tuple[str, ValidatedRunnerContext, str]] = [
    (
        "legacy SmolRunner v1 provenance",
        context(provenance=provenance(source_kind="smolrunner-protected-state-v1")),
        "runner_context_untrusted_source",
    ),
    (
        "forged receipt",
        context(provenance=provenance(source_kind="caller-selected-json")),
        "runner_context_untrusted_source",
    ),
    (
        "arbitrary identity values",
        context(
            installation_id="chosen-runner",
            workspace_id="chosen-workspace",
            cache=replace(context().cache, owner_workspace_id="chosen-workspace"),
            provenance=provenance(identity_bound_from_durable_state=False),
        ),
        "runner_context_identity_unbound",
    ),
    (
        "writable parent directories",
        context(provenance=provenance(state_parents_mode_private=False)),
        "runner_context_state_parent_writable",
    ),
    (
        "wrong owner",
        context(provenance=provenance(installation_owner_uid=uid + 1)),
        "runner_context_installation_owner_mismatch",
    ),
    (
        "wrong mode",
        context(provenance=provenance(installation_mode=0o666)),
        "runner_context_installation_mode_unsafe",
    ),
    (
        "symlinked parents",
        context(provenance=provenance(state_parents_symlink_free=False)),
        "runner_context_state_parent_symlink",
    ),
    (
        "hard links",
        context(provenance=provenance(installation_link_count=2)),
        "runner_context_installation_hard_link",
    ),
    (
        "replaced files",
        context(
            provenance=provenance(
                descriptor_identity_after=DescriptorIdentity(identity.device, identity.inode + 1),
                path_identity_after=DescriptorIdentity(identity.device, identity.inode + 1),
            )
        ),
        "runner_context_installation_replaced",
    ),
    (
        "path races",
        context(
            provenance=provenance(
                path_identity_after=DescriptorIdentity(identity.device, identity.inode + 1)
            )
        ),
        "runner_context_path_race",
    ),
    (
        "otherwise valid repository-created document",
        context(provenance=provenance(repository_controlled_source=True)),
        "runner_context_untrusted_source",
    ),
]
for label, candidate, expected in refusals:
    result = map_validated_runner_context(receipt(), candidate)
    assert result["observation"] is None, label
    assert expected in codes(result), (label, codes(result))

# Additional independent filesystem and receipt refusal cases.
for candidate, expected in [
    (
        context(provenance=provenance(filesystem_observed_independently=False)),
        "runner_context_filesystem_unobserved",
    ),
    (
        context(provenance=provenance(cache_alias_free=False)),
        "runner_context_filesystem_alias",
    ),
    (
        context(provenance=provenance(cache_contained_in_workspace=False)),
        "runner_context_cache_containment_unproven",
    ),
]:
    result = map_validated_runner_context(receipt(), candidate)
    assert expected in codes(result)

unknown_resources = receipt()
unknown_resources["resources"] = {
    "available_memory_mib": None,
    "available_swap_mib": None,
}
result = map_validated_runner_context(unknown_resources, context())
assert result["observation"] is None
assert "runner_context_resources_unavailable" in codes(result)

missing_cache = receipt()
missing_cache["declared_cache_paths"][0]["exists"] = False
result = map_validated_runner_context(missing_cache, context())
assert result["observation"] is None
assert "runner_context_cache_absent" in codes(result)

try:
    capability_observations(
        [{"name": "git", "available": True}, {"name": "cargo", "available": True}],
        {"git": "duplicate", "cargo": "duplicate"},
    )
except ValueError:
    pass
else:
    raise AssertionError("duplicate capability mapping accepted")

# Actual fixture evidence for hard links, replacement, races, writable parents,
# and symlink parents demonstrates the producer observations used above.
hard_link = state_root / "installation-hard-link"
os.link(installation, hard_link)
assert installation.stat().st_nlink == 2
hard_link.unlink()
assert installation.stat().st_nlink == 1

writable_parent = root / "writable-parent"
writable_parent.mkdir(mode=0o777)
writable_parent.chmod(0o777)
assert stat.S_IMODE(writable_parent.stat().st_mode) & 0o022

real_parent = root / "real-parent"
real_parent.mkdir()
symlink_parent = root / "symlink-parent"
symlink_parent.symlink_to(real_parent, target_is_directory=True)
assert symlink_parent.is_symlink()

race_file = state_root / "race.json"
race_file.write_text("first", encoding="utf-8")
before = race_file.stat()
temporary = state_root / "replacement.json"
temporary.write_text("second", encoding="utf-8")
temporary.replace(race_file)
after = race_file.stat()
assert (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)

print("workspace bootstrap pure profile mapper tests passed")
PY
