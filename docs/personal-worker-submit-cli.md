# Exact personal-worker queue submission CLI

`glaeda queue submit` publishes one exact queued request through the durable personal-worker transaction layer.

```text
glaeda queue submit \
  --store-root /absolute/state/root \
  --revision REVISION \
  --generation GENERATION \
  --observed-at EPOCH_MILLIS \
  --request-id REQUEST_ID \
  --verification-profile PROFILE \
  --runner-profile RUNNER_PROFILE \
  --repository OWNER/NAME \
  --commit FULL_GIT_OBJECT_ID \
  --tree FULL_GIT_TREE_ID \
  --priority background|normal|interactive \
  --cpu-millis CPU \
  --memory-bytes BYTES \
  --pids PIDS \
  --cache-id CACHE_ID \
  --cache-namespace-digest sha256:... \
  --cache-access read|write|exclusive \
  --submitted-at EPOCH_MILLIS \
  [--operator-deadline EPOCH_MILLIS]
```

All identity, resource, cache, and time evidence is explicit. The command does not inspect a checkout, environment, manifest, credential, or system clock.

## Durable boundary

The command opens only an existing Unix personal-worker store. The state root, managed directory, `store.lock`, and current durable document must already exist. Missing state is refused without creating any object.

The command passes one typed `Submit` mutation to `apply_personal_worker_store_mutation`. That transaction remains the only recovery, cooperative writer-lock, exact revision/generation check, semantic validation, and publication authority.

A first exact submission advances the store revision and queue generation once. Replaying the same request semantics against the resulting snapshot returns `duplicate` without changing durable bytes. Reusing the request ID with different semantics returns `conflict`. Retained terminal identity also returns `conflict`.

The live queue accepts at most 256 typed requests. A submission against an already-full valid queue is refused as `invalid_mutation` without publishing new durable bytes. Bounded request fields make this semantic queue cap the CLI-reachable capacity boundary before one additional submission could exceed the 1 MiB store document limit. Canonical document encoding remains the final byte-bound guard for every store mutation.

The command constructs only a repository-build cache namespace tied to the exact submitted repository. Cancellation defaults to active and fallback eligibility defaults to ineligible. No admission, reservation, cache lease, lifecycle transition, profile intent, or last-activity evidence can be supplied.

## Observation and submission time

`--observed-at` is the caller's durable queue observation for this mutation. It must not move behind the existing snapshot. `--submitted-at` may be older than that observation but cannot be newer. An optional operator deadline must be later than the submission time.

No timestamp is generated or inferred by the command.

## Output and privacy

Human output contains only disposition, mutation class, and old/new revision and queue generation. JSON serializes the bounded transaction receipt or a fixed command error envelope.

Neither output mode includes:

- store or runtime paths;
- raw documents or operating-system error prose;
- commands or environment;
- credentials;
- process output;
- cache contents;
- private admission diagnostics.

## Explicit exclusions

This command adds no cancellation, scheduling, admission, reservation, lifecycle, GitHub, Lima, credential, subprocess, daemon, background, arbitrary JSON, generic document mutation, or source-discovery authority.
