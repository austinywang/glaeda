# Durable execution receipt storage

Glaeda stores validated legacy SmolRunner execution receipt v1 documents beneath one installation at:

```text
installations/INSTALLATION_ID/receipts/EXECUTION_ID.json
```

The receipt schema and identity remain the exact v1 SmolRunner contract during the rename. A successor identity requires its own versioned migration.

Receipts use their own managed directory. They are not placed beside journal files with a suffix because journal IDs permit dots, which would allow a valid journal identity to collide with a receipt filename.

## Publication contract

`publish_execution_receipt()` encodes the typed receipt through `encode_execution_receipt()` and publishes one complete private file through `StateStore::create_atomic`.

- a missing destination is created atomically;
- exact replay reads and validates the existing file and returns `duplicate`;
- the same execution ID with different valid receipt semantics returns `conflict`;
- malformed, noncanonical, identity-mismatched, or invalid existing bytes return `corrupt_state`;
- existing state is never replaced.

A duplicate result is not inferred from filename existence. The existing bytes are decoded, rebound to the requested execution ID, canonically re-encoded, and compared as typed receipt semantics before replay is accepted. Duplicate publication reports zero bytes written.

The Linux store uses the existing installation-local writer lock, a private synchronized temporary file, no-replace rename, and parent-directory synchronization.

## Read-back contract

`read_execution_receipt()` derives the path only from validated installation and execution identities. Present bytes must:

1. be valid UTF-8;
2. decode through the strict execution receipt v1 schema;
3. contain the requested execution ID;
4. reproduce the exact stored bytes through deterministic canonical encoding.

A hand-edited equivalent JSON document is not accepted as durable canonical state. Missing state is returned separately from corrupt state.

## Directory lifecycle

New installation publication creates and synchronizes `receipts/` together with `resources/` and `journals/`.

`prepare_installation()` creates the directory for compatible existing installations and refuses symlinks, broad modes, or foreign ownership. Staging and temporary-file recovery inspection includes the new directory rather than treating it as an unexpected entry.

Read-only orphan inspection treats a missing `receipts/` directory in a pre-receipt installation as empty, allowing recovery review before migration. A present receipt directory must still satisfy the normal owner, mode, directory, and no-symlink checks.

## Boundary of this slice

This storage layer does not:

- modify host-preparation execution;
- generate the execution ID;
- capture start or terminal clocks;
- compute the reviewed-source digest;
- publish a receipt from the live CLI;
- add a receipt read command;
- send receipts to Stensibly, Proofwake, or another service.

A later execution-boundary slice must create and retain the receipt context before the first journal checkpoint, publish after a terminal report, and fail closed when externally consumable JSON is requested but receipt publication does not complete.
