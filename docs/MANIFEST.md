# Manifest reference

`glaeda.yml` describes the host and execution boundary Glaeda should reconcile for one project. It is deliberately not a pipeline language: dependency installation, test commands, build logic, and GitHub workflow triggers remain in the project repository.

## Versioning

The top-level `version` field is required. The current and only accepted value is `1`.

Unknown fields and unknown versions fail closed. Future releases may add a migration command, but they must not silently reinterpret an older or newer document.

## Fields

### `repository`

The GitHub repository in exact `OWNER/REPOSITORY` form.

### `runner`

- `scope`: `repository` or `organization`.
- `user`: dedicated Linux service account, at most 32 characters, using lowercase letters, digits, `_`, and `-`.
- `labels`: one or more project-specific GitHub Actions runner labels. Duplicate labels are rejected.

Organization scope targets the owner component of `repository`. Repository selection and runner-group restrictions will be added with GitHub integration; the v1 plan only records the intended scope.

### `container`

- `image`: local image reference Glaeda should build or inspect.
- `file`: project-relative Containerfile path. Absolute paths and parent traversal are rejected.

The repository owns the Containerfile. Glaeda should record the resulting immutable image digest rather than trusting a mutable tag alone.

### `verify`

- `command`: project-relative verification entry point.
- `suites`: stable names mapped to one command argument each.

Glaeda does not interpret npm, Maven, Python, Convex, Blender, Renderprove, or other project-specific concepts. It invokes the repository-owned entry point inside the disposable execution boundary.

A Renderprove enrolment uses the same generic command boundary. `examples/renderprove.yml` points to the checked-in `examples/renderprove/run-renderprove-review.sh` wrapper. The wrapper accepts one stable `render` suite, requires an explicit trusted Renderprove checkout, selects one project-relative evidence directory, and delegates browser policy and receipt generation to Renderprove.

The typed contract in `renderprove_verification` keeps source revision, project image, worker image, Renderprove manifest, sanitized receipt, approved screenshots, private worker identity, private diagnostics, failure traces, and later approved visual diffs as distinct identities. A successful process alone cannot satisfy verification; one matching passing sanitized receipt and successful cleanup are required. App-readiness and browser failures remain explicit bounded outcomes.

The pure adapter in `renderprove_execution` binds that contract to one fixed runner-user `CommandSpec`: reviewed `runuser`, an empty inner environment, fixed identity variables, private path values, the wrapper from the disjoint reviewed Renderprove checkout, the single `render` argument, and the exact disposable-workspace working directory. It accepts no free-form executable, wrapper, suite, environment, or workspace selection. Ambient process state never supplies an implied working directory. Deployed-origin review, subprocess execution, filesystem observation, cancellation, cleanup, and artifact export remain outside this adapter.

A typed execution observation retains the exact private `CommandSpec` used for execution and the explicit normalized absolute working directory. Receipt binding compares the full private specification before checking the redacted argv and environment-key views, so changing the Renderprove checkout, evidence directory, wrapper path, home, runtime directory, or another private value fails closed even when the public command shape is unchanged. The private specification, working directory, raw stdout, and raw stderr are excluded from receipt serialization and redacted from `Debug` output.

### `limits`

- `memory`: positive integer followed by `KiB`, `MiB`, or `GiB`.
- `cpus`: finite value greater than zero and at most 128.
- `pids`: positive process limit.

These values are desired policy. The current `plan` command does not apply them.

### `trust`

Version one intentionally accepts only:

```yaml
trust:
  forks: deny
  trigger: operator
```

Broader policies require an explicit threat-model change. Public-fork execution and automatic persistent-runner PR execution are not implicit configuration options.

## Example

```yaml
version: 1
repository: example/project

runner:
  scope: repository
  user: project-runner
  labels:
    - project-ci

container:
  image: localhost/project-ci:1
  file: build/ci/Containerfile

verify:
  command: scripts/run-vps-verification.sh
  suites:
    focused: focused
    full: full

limits:
  memory: 2GiB
  cpus: 1.5
  pids: 768

trust:
  forks: deny
  trigger: operator
```

Validate it without changing the host:

```bash
glaeda plan --file glaeda.yml
glaeda --output json plan --file glaeda.yml
```
