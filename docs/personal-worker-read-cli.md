# Personal worker read CLI

Glaeda exposes three read-only commands for one already-published personal-worker snapshot:

```text
glaeda worker status --store-root /absolute/state/root
glaeda queue list --store-root /absolute/state/root --revision 7 --generation 11 --offset 0 --limit 100
glaeda job show --store-root /absolute/state/root --revision 7 --generation 11 JOB_ID
```

Use `--output json` for the schema-versioned machine-readable views. Human output is deterministic and intended for operator inspection.

## Exact snapshot reads

`worker status` reports the durable store revision and queue generation that were loaded. Pass those exact values to `queue list` and `job show`. A changed revision or generation fails closed rather than combining data from different snapshots.

Queue pages contain only live queued and active work. Retained terminal tombstones do not alter live queue totals, ordering, or pagination. `job show` can still return a retained terminal job while its exact tombstone remains in the bounded durable ledger. Evicted or otherwise unprovable terminal history returns `not_found`.

## Store boundary

`--store-root` must be an explicit absolute normalized path. The command does not infer state from the current directory, home directory, manifest, environment, or credentials.

The read commands:

- open only an existing `personal-worker` store beneath the supplied root;
- load the canonical `current.json` document;
- do not create the store directory or lock file;
- do not acquire the writer lock;
- do not run staged-state recovery;
- do not publish, replace, migrate, initialize, or repair state;
- leave an existing `.next.json` document untouched.

This means reads can proceed while a cooperative writer holds the mutation lock, but they observe only the currently published snapshot.

## Public errors and privacy

Errors are fixed and bounded. JSON errors include a schema version, kind, and public message. They do not include the supplied store path, raw document bytes, operating-system error prose, commands, environment, credentials, process output, or cache contents.

The commands add no queue submission or cancellation authority, no broker loop, no GitHub or Lima calls, no clock reads, and no background process.
