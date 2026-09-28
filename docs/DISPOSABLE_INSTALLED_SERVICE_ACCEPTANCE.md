# Installed disposable-service physical acceptance

This runbook is the operator-facing execution sequence for issue #492. It prepares and records one benign, explicitly approved physical acceptance of the installed disposable-worker service on an operator-owned Apple-silicon Mac.

It does not grant authority by itself. Keep the three operator gates below separate:

1. using live GitHub App credentials and the enrolled test repository;
2. installing or removing the LaunchAgent with `service apply`;
3. queueing the live GitHub Actions acceptance job.

The acceptance repository must remain a deliberately selected benign test repository until the hostile-CI network boundary is accepted.

The `/opt/smolrunner`, `SMOLRUNNER_*`, and installed-helper identities below describe the existing physical service contract. They remain exact transition identities until their owning migration lanes move and verify those live surfaces.

## Evidence boundary

Retain only bounded evidence needed to identify the exact accepted run:

- Glaeda source commit under test;
- installed Glaeda executable SHA-256;
- installed Scale Set bridge SHA-256;
- canonical enrollment SHA-256;
- exact LaunchAgent plan identity;
- typed `service apply` report;
- read-only `service status` evidence that the exact LaunchAgent identity is `loaded_exact` with `installation_remediation=none`;
- GitHub workflow/run and bounded durable attempt identities needed to follow the one job;
- terminal proof that the runner is absent, the VM is absent, capacity is released, no worker-specific durable debt remains, and the exact LaunchAgent identity remains loaded.

Current status schema v2 deliberately reports `runtime_health=unknown` for an exact loaded LaunchAgent. That is the truthful accepted value until a separately reviewed observer can prove controller progress. Do not translate `loaded_exact` into a claim that the supervised process is healthy or polling.

Do not retain or paste:

- GitHub App private-key material or tokens;
- JIT configuration or registration tokens;
- enrollment contents or private enrollment path;
- Keychain material;
- secret-bearing environment values;
- raw `launchctl` output, process argv, environment dumps, or arbitrary command output.

## 0. Select exact identities

Run as the non-root operator account that will own the per-user LaunchAgent.

Set local shell variables without printing them into a transcript:

```sh
PROGRAM=/absolute/path/to/installed/smolrunner
ENROLLMENT=/absolute/path/to/canonical/enrollment.json
OPERATOR_HOME="$HOME"
```

The supported Scale Set bridge path is fixed by the enrollment/service contract:

```text
/opt/smolrunner/bin/scaleset-bridge
```

Record the exact repository commit being accepted separately from private machine paths.

## 1. Read-only installed-identity preflight

This step performs no LaunchAgent mutation, no GitHub request, no Keychain lookup, no Lima command, and no job admission.

Run the checked-in ignored acceptance test:

```sh
SMOLRUNNER_INSTALLED_SERVICE_ACCEPTANCE=preflight-identities \
SMOLRUNNER_INSTALLED_SERVICE_PROGRAM="$PROGRAM" \
SMOLRUNNER_INSTALLED_SERVICE_ENROLLMENT="$ENROLLMENT" \
SMOLRUNNER_INSTALLED_SERVICE_OPERATOR_HOME="$OPERATOR_HOME" \
cargo test --locked \
  --test disposable_installed_service_preflight \
  installed_service_identities_produce_one_exact_approval_plan \
  -- --ignored --exact --nocapture
```

The preflight must prove all of the following before proceeding:

- the selected Glaeda executable is a stable root-owned, non-writable executable leaf;
- `/opt/smolrunner/bin/scaleset-bridge` is a stable root-owned, non-writable executable leaf;
- the canonical enrollment is stable, operator-owned, and mode `0600`;
- the installed bridge digest equals the digest bound by the canonical enrollment;
- the linked library and the selected installed Glaeda executable derive the same exact installed-service plan identity;
- the emitted receipt exposes no private paths.

Keep the receipt. It contains the public program, bridge, and enrollment digests plus the exact plan identity needed for the next gate.

A passing preflight is evidence only. It does not authorize installation or live GitHub use.

## 2. Rebuild the exact service plan

Using the digests from the accepted preflight receipt, rebuild the installed plan with the exact target executable:

```sh
"$PROGRAM" --output json service plan \
  --desired installed \
  --operator-home "$OPERATOR_HOME" \
  --program "$PROGRAM" \
  --program-digest "$PROGRAM_DIGEST" \
  --enrollment "$ENROLLMENT" \
  --enrollment-digest "$ENROLLMENT_DIGEST"
```

Record only the bounded JSON report. Confirm that its `plan_identity` is exactly the same identity approved from the preflight.

If any identity changed, stop and rerun the read-only preflight. Do not carry approval across a changed plan identity.

## 3. Operator gate: LaunchAgent installation

Obtain explicit operator approval for the exact installed `plan_identity`.

Only after that approval, apply the plan:

```sh
"$PROGRAM" --output json service apply \
  --desired installed \
  --operator-home "$OPERATOR_HOME" \
  --program "$PROGRAM" \
  --program-digest "$PROGRAM_DIGEST" \
  --enrollment "$ENROLLMENT" \
  --enrollment-digest "$ENROLLMENT_DIGEST" \
  --approve-plan "$PLAN_IDENTITY"
```

The apply engine revalidates the current protected inputs and exact LaunchAgent state immediately before mutation. Its typed report is the accepted public installation evidence; do not substitute raw `launchctl` output for that report.

If apply reports foreign, unsafe, ambiguous, or changed state, stop. Re-observe before proposing retry or cleanup.

After a successful apply, inspect the exact installed state through the read-only status surface:

```sh
"$PROGRAM" --output json service status \
  --operator-home "$OPERATOR_HOME" \
  --program "$PROGRAM" \
  --program-digest "$PROGRAM_DIGEST" \
  --enrollment "$ENROLLMENT" \
  --enrollment-digest "$ENROLLMENT_DIGEST"
```

Before proceeding, require all of these from the same bounded report:

- `state` is `loaded_exact`;
- `service_loaded` is `true`;
- `plan_identity` is the exact approved installed plan identity;
- `installation_remediation` is `none`;
- `runtime_health` is `unknown`.

`runtime_health=unknown` is expected. The current observer proves exact installed/load identity and intentionally does not infer controller liveness, polling progress, or crash-loop health from `launchctl print` fields.

## 4. Operator gate: live credentials and enrolled repository

Before allowing the installed service to contact GitHub, explicitly approve use of the selected live GitHub App credential and enrolled benign test repository.

Keep credential material in the Mac control plane. Do not copy private-key bytes, tokens, JIT configuration, or secret environment values into the runbook transcript or acceptance artifacts.

Freshly confirm `service status` remains `loaded_exact` with `installation_remediation=none`. Separately require worker-side durable evidence that no prior disposable attempt, runner, VM, or capacity debt is present before queueing work. Do not use `runtime_health=unknown` as either success or failure evidence for worker progress, and do not start a second manual worker process as a substitute for the installed service.

## 5. Operator gate: queue exactly one benign job

Explicitly approve one job from the selected enrolled repository, then queue it through ordinary GitHub Actions.

Do not queue a second acceptance job while this run is unsettled.

Record bounded identities as they become available:

1. GitHub workflow/run identity;
2. durable Scale Set delivery / attempt identity;
3. capacity reservation;
4. exact disposable VM identity after clone binding;
5. exact GitHub runner ID after JIT binding;
6. exact job-start identity;
7. one terminal job result.

The job should exercise enough ordinary build/test work to prove the real runner path while remaining benign. Hostile-network, credential-theft, persistence, and peer-reachability fixtures belong to M4 after its enforcement boundary is accepted.

## 6. Require zero-state convergence

Do not call the run successful at GitHub job completion alone. Require fresh evidence that the lifecycle converged through teardown:

- terminal GitHub evidence was observed;
- the exact runner registration is absent after removal;
- the exact disposable VM is absent after teardown;
- the reservation/capacity is released and reported capacity is zero;
- no acceptance-specific VM, runner registration, durable reservation, or temporary acceptance artifact remains;
- fresh `service status` still reports `loaded_exact`, `service_loaded=true`, the exact approved plan identity, `installation_remediation=none`, and `runtime_health=unknown`.

The zero-capacity/no-debt proof comes from the worker lifecycle evidence. The service-status proof establishes only that the exact accepted LaunchAgent remains installed and loaded; it does not establish runtime progress or health.

If teardown or release remains uncertain, retain the exact durable recovery evidence and stop. Do not replay JIT generation, acquisition acknowledgement, clone creation, or another conflicting mutation across an ambiguous Started/debt state.

## 7. Failure handling

For every failed checkpoint:

1. keep the exact durable evidence that identifies the unsettled operation;
2. freshly re-observe GitHub, Lima, and service state before deciding what to do next;
3. preserve same-name/foreign-state protections;
4. avoid deletion from a basename, PID, stale report, or elapsed time alone;
5. split any required production repair into its own small issue/PR tied to the observed checkpoint.

Controller-SIGKILL quiescence belongs to #486/#491 unless this run reproduces the same exact failure. Hostile-CI network enforcement belongs to M4/#498.

Any repair that changes privilege, credential handling, durable recovery, destructive cleanup authority, or race-sensitive lifecycle behavior requires implementation-independent exact-head acceptance.

## 8. Optional service removal after acceptance

If the operator chooses to remove the LaunchAgent after the acceptance, plan removal separately:

```sh
"$PROGRAM" --output json service plan \
  --desired removed \
  --operator-home "$OPERATOR_HOME" \
  --program "$PROGRAM" \
  --program-digest "$PROGRAM_DIGEST" \
  --enrollment "$ENROLLMENT" \
  --enrollment-digest "$ENROLLMENT_DIGEST"
```

Obtain explicit approval for that removal plan identity, then invoke `service apply --desired removed` with the same exact inputs and the newly approved removal identity.

Installation approval never authorizes later removal, and removal approval never authorizes a changed installation.

## Completion receipt

A #492 acceptance comment should summarize only:

- exact Glaeda commit accepted;
- exact program / bridge / enrollment digests;
- exact installed plan identity and apply disposition;
- enrolled benign repository and GitHub run identity;
- bounded durable attempt / VM / runner identities;
- terminal result;
- runner absence;
- VM absence;
- capacity zero and absence of worker-specific durable debt;
- final LaunchAgent state `loaded_exact` with `installation_remediation=none` and `runtime_health=unknown` if the service is retained;
- secret/private-path sentinel result;
- whether the operator retained or removed the installed service afterward.

The acceptance is complete only when the one real GitHub Actions job ran once through the installed LaunchAgent-supervised disposable-worker path and the worker-specific state converged back to zero. A retained service must still match the exact accepted loaded identity; current status does not claim runtime health.