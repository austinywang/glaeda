# Repository workspace bootstrap contract

## Purpose

Glaeda repositories expose one repository-owned command that evaluates whether a checkout is ready for a named verification profile. The first implementation is read-only: it reports workspace capabilities and refuses installation, source mutation, Git mutation, credential use, and publication.

This boundary remains separate from Glaeda host preparation and issue #148's runner-owned verification-profile execution.

## Canonical command

Run from the repository root:

```bash
./scripts/bootstrap
./scripts/bootstrap --output json
```

The canonical path is `./scripts/bootstrap`. Each adopting repository owns that file and its implementation.

## Required working directory

The current working directory must equal the resolved Git worktree root. Invocation from a parent directory, subdirectory, or unrelated worktree ends in `blocked`. The repository root must contain a `[package]` entry named `smolrunner` in `Cargo.toml` and a committed `Cargo.lock`.

The workspace capability receipt is a Glaeda schema v2 identity. Its canonical public repository identity is `teamleaderleo/glaeda`; an observed SmolRunner or other GitHub remote remains a distinct alternate repository. The package marker remains transition-compatible with the retained name. Verification-profile names use the explicit Glaeda successor identities from issue #754. Retained SmolRunner schema v1 receipt/profile fixtures remain historical evidence.

## Accepted inputs

Workspace receipt schema version 2 accepts only:

- `--output human|json`, defaulting to `human`;
- `--operation verify|commit|publish`, defaulting to `verify`.

Unknown flags and free-form arguments are rejected. The command accepts no shell fragment, command string, package name, path, environment assignment, or ref.

`verify` evaluates verification readiness. `commit` also evaluates Git author readiness. `publish` evaluates local publication prerequisites and then blocks because remote authorization probing and publication remain outside this slice.

## Environment policy

The entrypoint reads only the minimum ambient values needed for fixed probes and cache classification:

- `PATH` for executable discovery;
- `HOME`, `CARGO_HOME`, `RUSTUP_HOME`, and `TMPDIR` for normal tool discovery;
- `CARGO_TARGET_DIR` for Cargo target-cache classification.

Configured values are resolved privately. Relative `CARGO_TARGET_DIR` and `CARGO_HOME` values are based on the required repository-root working directory. An unset `CARGO_TARGET_DIR` resolves to `<repository-root>/target`. An unset `CARGO_HOME` resolves from `HOME` to the user's Cargo directory.

Raw environment values and resolved private paths never enter human output, JSON, errors, or the capability fingerprint. Child probes use fixed executable names, absolute resolved programs, fixed argument vectors, bounded execution time, closed stdin, captured output, `GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0`, and no shell. Version output is reduced to one bounded token; other command output is discarded.

## Cache policy

Each declared cache is reported with bounded fields:

- `name`;
- `source`: `default` or `environment`;
- `base`: `repository-root`, `home-directory`, or `absolute`;
- symbolic `public_path`;
- `path_class`: `repository-local`, `external-private`, `missing`, or `unsafe`;
- `intended_path_class` when a missing path can still be classified;
- directory-existence and ownership-observation booleans;
- `ownership`: `current-user`, `different-user`, or `unestablished`;
- parent-escape and symlink-alias booleans;
- ownership expectation.

The probe checks path components without following symlinks. Any `..` component is rejected. A symlink at the cache path or any observed ancestor is rejected. A cache path equal to the repository root, a non-directory, a wrong-owner directory, an unresolved path, or an existing directory whose ownership cannot be established blocks readiness.

A missing directory is a declared deviation. Its ownership remains `unestablished`; the receipt reports only whether its intended location is repository-local or external-private. The command creates no directory.

## Idempotence requirements

The command is observational and deterministic for a fixed checkout and machine observation. A run:

1. observes checkout cleanliness before probing;
2. performs fixed read-only probes;
3. observes checkout cleanliness again;
4. requires a clean checkout both times;
5. requires the cleanliness result to remain unchanged.

It writes no source file, generated file, lockfile, index entry, Git ref, cache entry, directory, or configuration. Repeated runs remain safe. Receipts change only when an observed input changes, including source identity, tool availability/version, cache existence/ownership, memory, swap, CPU count, Git identity, operation, or checkout cleanliness.

## Credential boundary

The command does not read, create, update, print, or validate credentials. It never invokes a credential helper, contacts a registry or GitHub, fetches dependencies, or tests a push.

Git author readiness is evaluated only for `commit` and `publish`, and only booleans are public. Author name and email values remain private. For `publish`, remote authorization remains `unprobed`, publication readiness remains false, and the result is `blocked`.

## Observations

The receipt contains:

- repository-root identity without a filesystem path;
- exact source commit and tree identities;
- checkout cleanliness before and after execution;
- required and optional tool observations and bounded versions;
- repository-native verification backend availability and exact scope classes;
- formatter capabilities by domain;
- evidence-based cache classification and ownership expectations;
- available memory and swap through `/proc/meminfo` where available;
- logical CPU count and recommended concurrency;
- conditional Git identity and publication readiness;
- exact next verification-profile names;
- typed deviations and blocking reasons.

Optional tools remain optional. Their absence produces deviations and never silently widens the Cargo verification backend.

## Output contract

Human and JSON output are rendered from the same receipt. The command exits:

- `0` for `ready`;
- `0` for `ready_with_declared_deviations`;
- `1` for `blocked`;
- `2` when argument parsing cannot enter the command contract.

The top-level `state` is exactly one of:

- `ready`;
- `ready_with_declared_deviations`;
- `blocked`.

## Capability receipt schema

Schema version 2 is a bounded object. The cache portion has this form:

```json
{
  "schema_version": 2,
  "receipt_type": "glaeda-workspace-capability-receipt",
  "state": "ready_with_declared_deviations",
  "operation": "verify",
  "repository_root": {
    "kind": "git-worktree",
    "repository": "teamleaderleo/glaeda",
    "expected_repository": "teamleaderleo/glaeda",
    "required_marker": "Cargo.toml",
    "required_lockfile": "Cargo.lock",
    "working_directory": "repository-root",
    "cwd_is_repository_root": true,
    "private_path_exposed": false
  },
  "source": {
    "commit": "40 lowercase hexadecimal characters",
    "tree": "40 lowercase hexadecimal characters",
    "clean_before": true,
    "clean_after": true,
    "cleanliness_unchanged": true
  },
  "required_tools": [],
  "optional_tools": [],
  "observed_tool_versions": {},
  "verification_backends": [],
  "formatter_capabilities": [],
  "declared_cache_paths": [
    {
      "name": "cargo-target",
      "source": "default",
      "base": "repository-root",
      "public_path": "<repository-root>/target",
      "path_exposed": false,
      "path_class": "missing",
      "intended_path_class": "repository-local",
      "exists": false,
      "directory": null,
      "ownership_observed": false,
      "ownership": "unestablished",
      "parent_escape_detected": false,
      "symlink_alias_detected": false,
      "expectation": "exclusive-writer-per-build"
    }
  ],
  "resources": {
    "available_memory_mib": 4096,
    "available_swap_mib": 0,
    "logical_cpu_count": 4,
    "recommended_concurrency": 2
  },
  "git_identity": {
    "evaluated": false,
    "ready": null,
    "name_configured": null,
    "email_configured": null
  },
  "publication_readiness": {
    "evaluated": false,
    "ready": null,
    "remote_configured": null,
    "authorization": "not-requested"
  },
  "next_verification_profiles": [
    "glaeda.required",
    "glaeda.doctor",
    "glaeda.plan"
  ],
  "deviations": [],
  "blocking_reasons": [],
  "capability_fingerprint": "sha256:64 lowercase hexadecimal characters"
}
```

The field names `required_tools`, `optional_tools`, `verification_backends`, `formatter_capabilities`, `deviations`, `blocking_reasons`, and `next_verification_profiles` are shared vocabulary with issue #148. Issue #117 owns workspace observation; issue #148 owns profile selection and execution.

## Verification profile names

Workspace receipt schema version 2 emits the current Glaeda successor profile identities:

- `glaeda.required` — exactly the eight checks emitted by `scripts/verify required --plan-json`;
- `glaeda.doctor` — the machine-readable doctor check;
- `glaeda.plan` — the reference plan and host-plan smoke checks.

`glaeda.required` names the repository-required AGENTS suite. It does not claim parity with the larger GitHub Actions `Verify` closure, which also runs additional bridge, helper, shell, cross-target, and reference assertions.

This slice identifies names only. It does not select, expand, or run profiles.

## Failure classes

Blocking classes include unresolved repository identity, wrong working directory, marker/lockfile failure, missing source identity, dirty or unobservable checkout state, required-tool failure, unsafe cache path, parent escape, symlink alias, non-directory cache, wrong cache owner, unestablished ownership for an existing cache, missing Git identity for a requested commit/publication operation, unproven publication authorization, changed checkout cleanliness, and fixed internal probe failure.

Deviation classes include alternate or unidentified public origin, optional-tool failure, missing cache directories with unestablished ownership, unavailable memory/swap observation, and available memory below the two-gibibyte build guideline.

All public messages are fixed strings. Raw command output, paths, environment values, numeric user IDs, and exception text are discarded.

## Cleanup expectations

No cleanup action is required because the command creates no workspace state. The postcondition is a clean checkout whose cleanliness result matches the precondition. Redirect receipts outside the checkout; a receipt redirected into the checkout becomes an untracked file and correctly blocks the postcondition.

## Repeated-run behaviour

For an unchanged clean checkout and equivalent machine observations, state, source identity, tool observations, cache classifications, profile names, and `capability_fingerprint` remain equivalent. `Cargo.lock` remains byte-identical and Git status remains clean.

The fixture covers the canonical Glaeda remote, historical SmolRunner and foreign remote classification, unset defaults, relative repository-local paths, absolute external paths, parent escapes, symlinks, wrong ownership, missing directories, private-path suppression, cold and repeated execution, commit readiness, publication refusal, dirty/subdirectory refusal, and lockfile preservation.

## Explicit exclusions

The command cannot perform `sudo`, elevation, host or global tool installation, dependency download, shell-profile edits, Git configuration changes, credential creation/lookup/mutation, formatting, build, test, reset, clean, commit, arbitrary shell execution, remote publication, or production deployment.

A later slice may add a separately reviewed repository-local dependency-preparation mode after its exact files, caches, lockfile policy, failure recovery, and cleanup contract are explicit.
