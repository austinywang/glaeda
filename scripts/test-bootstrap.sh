#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
temporary_root=$(mktemp -d)
trap 'rm -rf "$temporary_root"' EXIT

fixture="$temporary_root/glaeda"
mkdir -p "$fixture/scripts" "$temporary_root/bin" "$temporary_root/home"
cp "$repo_root/scripts/bootstrap" "$fixture/scripts/bootstrap"
cp -R "$repo_root/scripts/workspace_bootstrap" "$fixture/scripts/workspace_bootstrap"
chmod +x "$fixture/scripts/bootstrap"

cat > "$fixture/Cargo.toml" <<'TOML'
[package]
name = "glaeda"
version = "0.1.0"
edition = "2024"
TOML
printf '# locked fixture\n' > "$fixture/Cargo.lock"
printf 'target/\n.cache/\n' > "$fixture/.gitignore"

write_tool() {
  local name=$1
  local output=$2
  cat > "$temporary_root/bin/$name" <<EOF_TOOL
#!/usr/bin/env bash
printf '%s\\n' '$output'
EOF_TOOL
  chmod +x "$temporary_root/bin/$name"
}
write_unavailable_tool() {
  local name=$1
  cat > "$temporary_root/bin/$name" <<'EOF_TOOL'
#!/usr/bin/env bash
exit 1
EOF_TOOL
  chmod +x "$temporary_root/bin/$name"
}

write_tool cargo 'cargo 1.88.0 (fixture)'
write_tool cargo-clippy 'clippy 0.1.88 (fixture)'
write_tool cargo-nextest 'cargo-nextest 0.9.99 (fixture)'
write_tool rustc 'rustc 1.88.0 (fixture)'
write_tool rustfmt 'rustfmt 1.8.0-stable (fixture)'
# Keep optional availability deterministic even on hosted images that provide them.
write_unavailable_tool just
write_unavailable_tool podman

ambient_git_config="$temporary_root/ambient-gitconfig"
git config --file "$ambient_git_config" commit.gpgSign true
git config --file "$ambient_git_config" init.defaultBranch hostile
export GIT_CONFIG_GLOBAL="$ambient_git_config"

fixture_git() {
  env -i \
    PATH=/usr/bin:/bin \
    HOME="$temporary_root/home" \
    GIT_CONFIG_GLOBAL=/dev/null \
    GIT_CONFIG_NOSYSTEM=1 \
    LANG=C \
    LC_ALL=C \
    git -C "$fixture" "$@"
}

fixture_git init -q -b main
fixture_git config user.name 'Glaeda Bootstrap Test'
fixture_git config user.email 'bootstrap-test@example.invalid'
fixture_git remote add origin https://github.com/teamleaderleo/glaeda.git
fixture_git add Cargo.toml Cargo.lock .gitignore scripts/bootstrap scripts/workspace_bootstrap
fixture_git commit -qm fixture

export PATH="$temporary_root/bin:/usr/bin:/bin"
export HOME="$temporary_root/home"
unset CARGO_HOME RUSTUP_HOME CARGO_TARGET_DIR

sha256_file() {
  python3 - "$1" <<'PY'
import hashlib
import pathlib
import sys

print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
}

lock_before=$(sha256_file "$fixture/Cargo.lock")

# Unset defaults remain missing, owned by nobody, and safe to repeat.
(
  cd "$fixture"
  ./scripts/bootstrap --output json > "$temporary_root/first.json"
  ./scripts/bootstrap --output json > "$temporary_root/second.json"
  ./scripts/bootstrap --operation commit --output json > "$temporary_root/commit.json"
)
python3 - "$temporary_root" "$fixture" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
fixture = sys.argv[2]
first = json.loads((root / "first.json").read_text())
second = json.loads((root / "second.json").read_text())
commit = json.loads((root / "commit.json").read_text())
assert first["state"] == "ready_with_declared_deviations"
assert first["repository_root"]["repository"] == "teamleaderleo/glaeda"
assert first["repository_root"]["expected_repository"] == "teamleaderleo/glaeda"
assert "repository_remote_differs" not in {item["code"] for item in first["deviations"]}
assert first["source"]["clean_before"] is True
assert first["source"]["clean_after"] is True
assert first["source"]["cleanliness_unchanged"] is True
assert first["source"]["commit"] == second["source"]["commit"]
assert first["source"]["tree"] == second["source"]["tree"]
assert first["capability_fingerprint"] == second["capability_fingerprint"]
assert all(tool["available"] for tool in first["required_tools"])
assert first["verification_backends"][0]["available"] is True
assert first["verification_backends"][1]["available"] is True
assert first["git_identity"]["evaluated"] is False
assert commit["git_identity"]["ready"] is True
assert {item["code"] for item in first["deviations"]} == {
    "optional_tool_just_unavailable",
    "optional_tool_podman_unavailable",
    "cache_cargo_target_missing",
    "cache_cargo_home_missing",
}
caches = {item["name"]: item for item in first["declared_cache_paths"]}
assert caches["cargo-target"]["path_class"] == "missing"
assert caches["cargo-target"]["intended_path_class"] == "repository-local"
assert caches["cargo-target"]["base"] == "repository-root"
assert caches["cargo-home"]["path_class"] == "missing"
assert caches["cargo-home"]["intended_path_class"] == "external-private"
assert caches["cargo-home"]["base"] == "home-directory"
assert all(cache["ownership"] == "unestablished" for cache in caches.values())
public = json.dumps(first)
assert fixture not in public
assert str(root / "home") not in public
assert str(root / "bin") not in public
assert all(cache["path_exposed"] is False for cache in caches.values())
PY

# Existing defaults and available optional tools are ready.
mkdir -p "$fixture/target" "$HOME/.cargo"
write_tool just 'just 1.40.0 (fixture)'
write_tool podman 'podman version 5.4.0 (fixture)'
(
  cd "$fixture"
  ./scripts/bootstrap --output json > "$temporary_root/ready.json"
)
python3 - "$temporary_root/ready.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["state"] == "ready"
assert receipt["deviations"] == []
assert all(tool["available"] for tool in receipt["optional_tools"])
caches = {item["name"]: item for item in receipt["declared_cache_paths"]}
assert caches["cargo-target"]["path_class"] == "repository-local"
assert caches["cargo-home"]["path_class"] == "external-private"
assert all(item["ownership"] == "current-user" for item in caches.values())
PY

# Current Glaeda identity stays distinct from historical and foreign remotes.
fixture_git remote set-url origin https://github.com/teamleaderleo/smolrunner.git
(
  cd "$fixture"
  ./scripts/bootstrap --output json > "$temporary_root/legacy-repository.json"
)
python3 - "$temporary_root/legacy-repository.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["state"] == "ready_with_declared_deviations"
assert receipt["repository_root"]["repository"] == "teamleaderleo/smolrunner"
assert receipt["repository_root"]["expected_repository"] == "teamleaderleo/glaeda"
assert {item["code"] for item in receipt["deviations"]} == {"repository_remote_differs"}
PY

fixture_git remote set-url origin https://github.com/alternate-owner/glaeda.git
(
  cd "$fixture"
  ./scripts/bootstrap --output json > "$temporary_root/alternate-repository.json"
)
python3 - "$temporary_root/alternate-repository.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["state"] == "ready_with_declared_deviations"
assert receipt["repository_root"]["repository"] == "alternate-owner/glaeda"
assert receipt["repository_root"]["expected_repository"] == "teamleaderleo/glaeda"
assert {item["code"] for item in receipt["deviations"]} == {"repository_remote_differs"}
PY

fixture_git remote set-url origin https://github.com/teamleaderleo/glaeda.git
(
  cd "$fixture"
  ./scripts/bootstrap --output json > "$temporary_root/restored-repository.json"
)
python3 - "$temporary_root/restored-repository.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["state"] == "ready"
assert receipt["repository_root"]["repository"] == "teamleaderleo/glaeda"
assert receipt["repository_root"]["expected_repository"] == "teamleaderleo/glaeda"
assert receipt["deviations"] == []
PY

# Relative configured paths resolve against the repository root.
mkdir -p "$fixture/.cache/cargo-target" "$fixture/.cache/cargo-home"
(
  cd "$fixture"
  CARGO_TARGET_DIR=.cache/cargo-target CARGO_HOME=.cache/cargo-home \
    ./scripts/bootstrap --output json > "$temporary_root/relative.json"
)
python3 - "$temporary_root/relative.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["state"] == "ready"
for cache in receipt["declared_cache_paths"]:
    assert cache["source"] == "environment"
    assert cache["base"] == "repository-root"
    assert cache["path_class"] == "repository-local"
    assert cache["ownership"] == "current-user"
    assert cache["public_path"] == "<repository-root>/<configured-cache>"
PY

# Absolute configured paths remain private and externally classified.
external_target="$temporary_root/private-target"
external_home="$temporary_root/private-cargo-home"
mkdir -p "$external_target" "$external_home"
(
  cd "$fixture"
  CARGO_TARGET_DIR="$external_target" CARGO_HOME="$external_home" \
    ./scripts/bootstrap --output json > "$temporary_root/absolute.json"
  CARGO_TARGET_DIR="$external_target" CARGO_HOME="$external_home" \
    ./scripts/bootstrap > "$temporary_root/absolute.txt"
)
python3 - "$temporary_root/absolute.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["state"] == "ready"
for cache in receipt["declared_cache_paths"]:
    assert cache["path_class"] == "external-private"
    assert cache["base"] == "absolute"
    assert cache["ownership_observed"] is True
    assert cache["ownership"] == "current-user"
    assert cache["path_exposed"] is False
PY
for private in "$external_target" "$external_home"; do
  ! grep -Fq "$private" "$temporary_root/absolute.json"
  ! grep -Fq "$private" "$temporary_root/absolute.txt"
done

# Parent escapes are blocked before resolution.
set +e
(
  cd "$fixture"
  CARGO_TARGET_DIR=../outside CARGO_HOME="$external_home" \
    ./scripts/bootstrap --output json > "$temporary_root/parent.json"
)
parent_status=$?
set -e
[[ $parent_status -eq 1 ]]
python3 - "$temporary_root/parent.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert "cache_cargo_target_parent_escape" in {item["code"] for item in receipt["blocking_reasons"]}
assert receipt["declared_cache_paths"][0]["parent_escape_detected"] is True
PY

# Symlink aliases are blocked.
ln -s "$external_target" "$fixture/.cache/linked-target"
set +e
(
  cd "$fixture"
  CARGO_TARGET_DIR=.cache/linked-target CARGO_HOME="$external_home" \
    ./scripts/bootstrap --output json > "$temporary_root/symlink.json"
)
symlink_status=$?
set -e
[[ $symlink_status -eq 1 ]]
python3 - "$temporary_root/symlink.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert "cache_cargo_target_symlink_alias" in {item["code"] for item in receipt["blocking_reasons"]}
assert receipt["declared_cache_paths"][0]["symlink_alias_detected"] is True
PY
rm "$fixture/.cache/linked-target"

# Missing configured directories are deviations with unestablished ownership.
missing_target="$temporary_root/private-missing-target"
missing_home="$temporary_root/private-missing-home"
(
  cd "$fixture"
  CARGO_TARGET_DIR="$missing_target" CARGO_HOME="$missing_home" \
    ./scripts/bootstrap --output json > "$temporary_root/missing.json"
)
python3 - "$temporary_root/missing.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["state"] == "ready_with_declared_deviations"
assert {item["code"] for item in receipt["deviations"]} == {
    "cache_cargo_target_missing", "cache_cargo_home_missing"
}
for cache in receipt["declared_cache_paths"]:
    assert cache["path_class"] == "missing"
    assert cache["intended_path_class"] == "external-private"
    assert cache["ownership_observed"] is False
    assert cache["ownership"] == "unestablished"
PY
! grep -Fq "$missing_target" "$temporary_root/missing.json"
! grep -Fq "$missing_home" "$temporary_root/missing.json"

# Wrong ownership is tested without privileged chown.
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$fixture/scripts" python3 - "$external_target" <<'PY'
import os, pathlib, sys
from workspace_bootstrap.probe import cache_observation_issues, observe_cache_path
path = pathlib.Path(sys.argv[1])
root = path.parent / "unrelated-repository-root"
observed = observe_cache_path(
    name="cargo-target", configured_value=str(path), default_value=None,
    configured_base=root, root=root, default_base_kind="repository-root",
    configured_public_path="<private-external-cache>/cargo-target",
    default_public_path="<repository-root>/target",
    expectation="exclusive-writer-per-build", expected_uid=os.geteuid() + 1,
)
assert observed["path_class"] == "unsafe"
assert observed["ownership_observed"] is True
assert observed["ownership"] == "different-user"
deviations, blockers = cache_observation_issues([observed])
assert deviations == []
assert [item["code"] for item in blockers] == ["cache_cargo_target_wrong_owner"]
assert str(path) not in str(observed)
PY

# Invalid arguments suppress caller-controlled content.
set +e
(
  cd "$fixture"
  ./scripts/bootstrap --unknown "$temporary_root/private-secret" > "$temporary_root/arguments.txt" 2>&1
)
argument_status=$?
set -e
[[ $argument_status -eq 2 ]]
grep -Fx 'bootstrap arguments are invalid' "$temporary_root/arguments.txt"
! grep -Fq "$temporary_root/private-secret" "$temporary_root/arguments.txt"

export CARGO_TARGET_DIR="$external_target"
export CARGO_HOME="$external_home"

# Publication remains blocked and credential-free.
set +e
(
  cd "$fixture"
  ./scripts/bootstrap --operation publish --output json > "$temporary_root/publish.json"
)
publish_status=$?
set -e
[[ $publish_status -eq 1 ]]
python3 - "$temporary_root/publish.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert receipt["publication_readiness"]["evaluated"] is True
assert receipt["publication_readiness"]["authorization"] == "unprobed"
assert "publication_authorization_unproven" in {item["code"] for item in receipt["blocking_reasons"]}
PY

# Dirty, missing-lockfile, and subdirectory invocation all block.
printf 'dirty\n' >> "$fixture/Cargo.toml"
set +e
(cd "$fixture" && ./scripts/bootstrap --output json > "$temporary_root/dirty.json")
dirty_status=$?
set -e
[[ $dirty_status -eq 1 ]]
python3 - "$temporary_root/dirty.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert "checkout_not_clean" in {item["code"] for item in receipt["blocking_reasons"]}
PY
git -C "$fixture" checkout -- Cargo.toml

mv "$fixture/Cargo.lock" "$temporary_root/Cargo.lock.saved"
set +e
(cd "$fixture" && ./scripts/bootstrap --output json > "$temporary_root/missing-lock.json")
missing_lock_status=$?
set -e
[[ $missing_lock_status -eq 1 ]]
python3 - "$temporary_root/missing-lock.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert "repository_lockfile_missing" in {item["code"] for item in receipt["blocking_reasons"]}
PY
mv "$temporary_root/Cargo.lock.saved" "$fixture/Cargo.lock"

mkdir "$fixture/nested"
set +e
(cd "$fixture/nested" && ../scripts/bootstrap --output json > "$temporary_root/subdirectory.json")
subdirectory_status=$?
set -e
[[ $subdirectory_status -eq 1 ]]
python3 - "$temporary_root/subdirectory.json" <<'PY'
import json, pathlib, sys
receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert "working_directory_not_repository_root" in {item["code"] for item in receipt["blocking_reasons"]}
PY
rmdir "$fixture/nested"

[[ -z $(git -C "$fixture" status --porcelain=v1 --untracked-files=all) ]]
[[ "$lock_before" == "$(sha256_file "$fixture/Cargo.lock")" ]]
printf 'workspace bootstrap tests passed\n'
