# External execution receipts

Glaeda preserves the exact **SmolRunner execution receipt v1** identity for the existing content-minimised JSON contract for one durable execution. It lets a coordinator, evidence index, or operator record what Glaeda attempted and whether the work completed, failed, or stopped for fresh observation without receiving host mutation authority or private execution detail.

The v1 producer, schema, and receipt identity remain legacy SmolRunner evidence during the rename; successor receipt identities require a separately reviewed versioned migration.

The implementation lives in `src/execution_receipt.rs`.

## Scope of the first slice

The first supported operation family is `host_preparation`. This module defines and validates the external document only. It does not yet:

- map a live `HostPreparationExecutionReport` into a receipt;
- persist receipts beside durable journals;
- change `host prepare` JSON output;
- add a receipt read command;
- send data to Stensibly, Proofwake, or another service;
- add network transport, retry, scheduling, or generic event publication.

Those steps follow after the document contract is accepted.

The intended delivery order is: map one live host-preparation report through stable public failure codes, publish the receipt atomically beside its durable journal, add exact execution-ID read-back, then expose the same typed document through JSON CLI output. Stensibly and Proofwake integrations begin only after those local boundaries are reviewed.

## Exact identity

Every receipt binds:

- one validated durable journal ID as `execution_id`;
- the SmolRunner producer version;
- one canonical `owner/name` repository identity;
- one externally supplied SHA-256 digest of the reviewed source identity;
- one bounded host-preparation phase ID;
- canonical millisecond-precision UTC start and terminal times;
- the host-preparation operation schema version.

A later live adapter must create the execution ID before the first durable checkpoint and compute the source digest from the exact reviewed public source identity. The receipt contract does not derive either identity from names, paths, journal prose, or mutable host state.

## Terminal outcomes

The receipt disposition is one of:

- `completed` — every action completed and no continuation remains;
- `action_failed` — at least one action or rollback failed;
- `fresh_observation_required` — every action in the phase completed, but the reviewed plan requires one or more explicit observation barriers before further work.

Each action retains only:

- bounded action ID;
- execution lane;
- rollback class;
- typed terminal outcome;
- stable public failure code for `failed` and `rollback_failed` outcomes.

The receipt derives all action counts from the action list. A decoder rejects altered summaries, duplicate action IDs, unsupported versions, nonterminal semantics, mismatched failure codes, and inconsistent continuation state.

## Continuation

Fresh-observation barriers and deferred action identities are bounded lowercase tokens. A receipt cannot claim `fresh_observation_required` without naming at least one barrier, and a receipt without that state cannot carry barrier identities.

A command exit is therefore not sufficient to claim completion when the reviewed plan requires new observation. Stensibly may use this state to retain responsibility or block a dependent item. Proofwake may index it as evidence. Neither consumer receives authority to continue the mutation.

## Privacy boundary

The v1 coverage declaration is fixed as partial, redacted, and not truncated. It explicitly omits:

- command values;
- process output;
- filesystem paths;
- complete host observations;
- precondition evidence;
- journal messages;
- credentials.

Free-form strings are not accepted as action IDs, phase IDs, continuation IDs, or failure codes. Public identity fields use existing validated `JournalId`, `RepositoryRef`, and `Sha256Digest` types.

Untrusted JSON and schema failures use one bounded generic diagnostic. Unknown attacker-controlled field names or values are not copied into the public error text.

## Limits

- maximum encoded receipt: 65,536 bytes;
- actions: 1 through 256;
- barriers: at most 64;
- deferred action identities: at most 64;
- bounded tokens: at most 128 lowercase ASCII characters;
- timestamps: exact `YYYY-MM-DDTHH:MM:SS.mmmZ` form, with calendar validation and no leap seconds.

## Canonical encoding and read-back

`encode_execution_receipt` emits stable pretty JSON with one trailing newline. `decode_execution_receipt` uses an exact deny-unknown-fields wire schema, reconstructs the typed receipt, and rejects forged derived summaries or coverage declarations.

The encoded document is deterministic for the same ordered action evidence. A later persistence slice will define atomic publication, no-replace or compare-and-swap behavior, exact execution-ID conflict handling, and canonical receipt digests.
