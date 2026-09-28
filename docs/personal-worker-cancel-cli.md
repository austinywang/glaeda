# Personal worker queued cancellation CLI

Cancel one exact queued request with caller-supplied durable evidence:

```text
glaeda job cancel \
  --store-root /absolute/state/root \
  --revision 7 \
  --generation 11 \
  --cancelled-at 1780000000000 \
  JOB_ID
```

Use `--output json` for the schema-versioned transaction receipt or fixed error envelope.

## Exact evidence

The command does not read a clock. `--cancelled-at` is the exact cancellation observation in epoch milliseconds and must be greater than zero.

The caller must also supply the exact current durable store revision and queue generation. Stale values fail closed. An applied cancellation advances both values exactly once. Replaying the same request ID and cancellation time against the new revision and generation returns `duplicate` without changing durable bytes. A different cancellation time for the already-cancelled request returns `conflict`.

## Queued-only authority

This command supplies no draining admission evidence. It can therefore cancel queued work only.

An active request requires an exact transition to `draining`, including reservation and admission evidence held by the broker/control-plane path. The CLI refuses active cancellation rather than inventing that evidence. Terminal jobs are immutable and are not reconstructed from history.

## Existing-state mutation

`--store-root` must be an explicit absolute lexically normalized path. Relative paths, `.` components, repeated separators, and parent traversal are refused before filesystem access.

The command opens only an already-created personal-worker store. It does not initialize a missing root, managed directory, lock file, or first document. The merged transaction layer then:

- acquires the existing cooperative writer lock;
- performs the existing staged-state recovery contract;
- loads and checks the exact revision and generation;
- applies the typed cancellation mutation;
- publishes through revision-checked durable replacement.

Lock contention returns a bounded `busy` error.

## Privacy boundary

Public output and errors do not include the supplied store path, raw durable documents, operating-system error prose, commands, environment, credentials, process output, cache contents, or private admission diagnostics.

The command adds no queue submission, active drain construction, reservation, lifecycle transition, profile mutation, last-activity update, GitHub or Lima access, background process, or clock authority.
