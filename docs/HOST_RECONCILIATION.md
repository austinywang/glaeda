# Host reconciliation

Glaeda separates desired state, observed state, proposed actions, and eventual execution. The separation is intentional: a setup script that immediately mutates the machine cannot reliably explain drift, partial completion, or rollback.

## State model

`DesiredHostState` is derived from a validated project manifest. It currently identifies required host commands, the dedicated runner account, the project container image, and the GitHub repository.

`CurrentHostState` records each bounded observation as:

- `present` — the probe found direct evidence;
- `absent` — the probe found direct evidence that the state is missing;
- `unknown` — the current privilege or integration boundary cannot establish the fact safely.

Unknown is not treated as absent. The planner emits `needs_inspection` rather than proposing a mutation based on a guess.

The initial Linux filesystem probe reads only:

- `PATH` to locate executable `git`, `podman`, and `systemctl` commands;
- `/etc/passwd` for the dedicated runner account;
- `/etc/subuid` and `/etc/subgid` for rootless-container mappings;
- `/var/lib/systemd/linger/` for user lingering.

It does not inspect a runner user's rootless Podman storage or query GitHub registration state. Those fields remain unknown until Glaeda has an explicit run-as-user and GitHub-authentication design.

## Command execution

The process layer exists before mutation so its invariants can be reviewed independently:

- the program path must be absolute;
- no implicit shell is invoked;
- the child environment starts empty;
- only explicit arguments and environment entries are passed;
- secret arguments and environment values serialize and debug-print as `[REDACTED]`;
- captured stdout and stderr are scrubbed for exact secret values before becoming an execution record;
- the record contains displayed argv, environment keys, exit status, success, stdout, and stderr.

Exact-value redaction is a defense, not a general secret-leak proof. A child process can transform, encode, split, hash, or otherwise derive a secret. Future mutation commands must minimize secret-bearing subprocesses and must never interpret child output as safe merely because basic redaction ran.

## Why apply is still absent

No CLI path invokes the process executor for host mutation yet. Before adding `apply`, the project must define:

1. which actions are reversible;
2. which actions require an operator confirmation boundary;
3. how completed and remaining actions are recorded after partial failure;
4. how root operations and runner-user operations are separated;
5. how registration tokens are acquired, passed, and destroyed;
6. how an existing non-Glaeda installation is adopted without overwriting it;
7. how package-manager and distribution differences are represented.

Until those semantics exist, `doctor`, `plan`, and `host plan` are bounded read-only commands.
