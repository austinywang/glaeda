# CMUX developer lease contract

Status: draft direction for teamleaderleo/glaeda#1298  
Related: #1174, #1057, #1056, #750, manaflow-ai/cmuxterm-hq#778

## Purpose

Make idle CMUX-owned Mac minis available as temporary interactive developer/agent capacity without creating another scheduler or weakening CI ownership rules.

The motivating journey is:

```text
developer runs out of local RAM/CPU
-> ask for one eligible owned machine
-> Glaeda selects an idle candidate
-> existing reservation mechanism removes it from new CI admission
-> CMUX opens persistent remote work there
-> developer releases it or the lease expires
-> machine returns to normal CI eligibility
```

The first proof deliberately uses an idle machine. It does not preempt a running GitHub Actions job.

## Existing authority

Glaeda already has the relevant hold format and fleet behavior.

`glaeda-mini-fleet reserve` writes:

```json
{
  "schema": "glaeda-reservation/v1",
  "owner": "...",
  "purpose": "...",
  "since": 0,
  "until": 0
}
```

The marker is:

- written atomically under a per-host reservation lock;
- bounded to an expiry no more than the reviewed maximum ahead;
- extendable by the same owner;
- protected from takeover by another owner unless explicitly forced;
- parsed by the same code used by runner hooks and fleet tooling;
- enough to make a host unavailable to new CI admission;
- enough to make fleet mutation paths treat the host as held.

This RFC does not introduce a second reservation document.

## Product operation

Add a candidate-selection operation above `reserve HOST`.

Illustrative CLI:

```bash
glaeda-mini-fleet borrow \
  --class std \
  --hours 4 \
  --purpose "interactive development" \
  --yes
```

The operation means:

> Select one currently eligible idle host from the requested class and create the ordinary `glaeda-reservation/v1` hold on that exact host.

A dry run remains the default if `--yes` follows the surrounding fleet command convention.

## Inputs

First version should stay narrow:

- requested accepted class, for example `std`;
- duration or explicit expiry;
- purpose;
- optional owner override;
- optional capability/role filter only if it already exists in fleet enrollment vocabulary.

Do not accept:

- arbitrary host mutation;
- raw CPU/RAM values as execution authority;
- runner registration tokens;
- CI workflow semantics;
- shell commands;
- a caller-selected host while claiming the result was automatic placement.

An explicit `reserve HOST` remains available for intentional host selection.

## Candidate eligibility

A borrow candidate must satisfy all existing fleet truth that can be checked before mutation.

At minimum:

1. enrolled;
2. accepted for the requested class;
3. current Glaeda generation accepted;
4. lifecycle/role allows interactive developer borrowing;
5. not `never_touch`;
6. no active or invalid reservation;
7. host lock not held or waiting in a way that makes the machine currently busy;
8. no currently executing CI work that would be displaced;
9. required connection path is available;
10. enough current host headroom for ordinary interactive use according to the reviewed fleet observation.

Unknown state refuses or removes the host from candidacy.

## Special-role hosts

Not every idle machine is ordinary capacity.

The first rollout should exclude hosts whose role would make interactive borrowing unsafe or surprising, including where applicable:

- signing writer;
- unique controller;
- unique cache writer;
- dedicated browser/dev-build machine;
- trusted-only seed role where personal work would violate its trust assumptions.

Eligibility should come from the fleet manifest/role model instead of a hard-coded hostname list where possible.

## Selection

Keep v1 deterministic and explainable.

A reasonable ordering:

1. exact requested class;
2. role explicitly allows developer leases;
3. idle now;
4. greater available memory/headroom;
5. useful known project/build heat when the existing evidence already exposes it;
6. stable host/node tie-break.

A learned or historical completion-time router is unnecessary for the first proof.

The result should include bounded reasons so an operator can understand why one host was selected and why a candidate was excluded.

## Concurrency

Two simultaneous borrowers must not receive the same machine.

Selection and reservation need one of:

- a controller-side serialized selection transaction over live fleet state; or
- optimistic candidate selection followed by the existing atomic reservation write, with bounded retry on a collision.

The existing on-host reservation writer is the final authority.

A stale picker snapshot cannot authorize reuse after another caller acquires the reservation.

## Output

Return a bounded machine-consumable result suitable for CMUX.

Candidate fields:

```json
{
  "schema": "glaeda-developer-lease/v1",
  "host": "cmux12s",
  "node_id": "cmux-mac-...",
  "class": "std",
  "owner": "...",
  "purpose": "interactive development",
  "since": 0,
  "until": 0,
  "reservation_schema": "glaeda-reservation/v1",
  "connection": {
    "kind": "ssh",
    "host": "cmux12s"
  }
}
```

The lease result is a description/correlation object. The on-host reservation marker remains the hold authority.

Do not return credentials, runner tokens, signing material, or an unrestricted host command channel.

## Lease and physical execution ownership

A fleet reservation and a Glaeda physical execution lease are distinct.

### Interactive SSH workspace

For the first CMUX dogfood path:

```text
developer lease reservation
-> host removed from ordinary CI admission
-> CMUX opens an authenticated remote user session
```

The reservation is an operator/fleet hold. The user's shell is not pretending to be a typed Glaeda workload receipt.

### Reviewed Glaeda workload

When work is submitted through a reviewed Glaeda execution adapter:

```text
developer reservation
-> reviewed semantic workload
-> #1057 import/bind or local admission
-> physical execution lease
-> receipt/settlement
```

The reservation cannot bypass final local admission.

## Expiry

Expiry is a safety property and should stay boring.

When `until` passes:

- runner/fleet observation stops treating the marker as an active hold;
- cleanup may remove the stale marker through the existing reviewed path;
- no new interactive execution authority is inferred;
- an old CMUX workspace may remain visible as history/reconnect metadata, but it cannot assume the machine is still reserved.

The initial product should warn before expiry rather than silently extending without the owner asking.

Same-owner explicit extension can reuse current semantics.

## Release

Existing `glaeda-mini-fleet release HOST --yes` remains authoritative.

Potential convenience:

```bash
glaeda-mini-fleet release --mine --host cmux12s --yes
```

or a lease-result identifier, only if it maps unambiguously back to the exact reservation owner/host.

Release does not kill arbitrary user processes by default.

The CMUX layer owns whether a remote workspace should settle, detach, stop an agent, or become history before releasing the fleet hold.

## CI behavior

No special CI workflow is needed.

While the reservation is active:

- runner hooks refuse new jobs on the host;
- fleet pool observation reports it reserved;
- routing chooses remaining owned capacity;
- hosted overflow remains available.

Do not add preemption.

The first dogfood test should deliberately run ordinary CI while one std mini is borrowed and show that work routes elsewhere.

## CMUX integration seam

CMUX should consume the borrow result as a machine/lease input, not as Glaeda internals.

Needed fields are limited to:

- stable machine/node reference;
- connectable host;
- class/capability summary;
- lease owner/purpose/expiry.

CMUX should not need:

- reservation file paths;
- runner hook internals;
- Glaeda state-store paths;
- host-lock paths;
- CI routing implementation.

## Tests

### Pure/fixture

- one eligible host selects successfully;
- several candidates are deterministic;
- busy host excluded;
- reserved host excluded;
- invalid marker excludes/fails closed;
- expired marker does not count as active;
- special-role host excluded;
- `never_touch` excluded;
- requested class unavailable returns no candidate;
- same-owner replay/extension is explicit;
- output is bounded/canonical.

### Concurrent acquisition

Two borrowers race for one eligible host.

Acceptance:

- one succeeds;
- one retries or receives no-candidate;
- never two successful active leases for the same host.

### Physical dogfood

1. borrow one std mini;
2. confirm its normal runner listener/admission path refuses new CI;
3. run an interactive coding-agent workload through CMUX;
4. keep CI active on the rest of the fleet;
5. release;
6. confirm the host returns to pool eligibility without repair.

## Non-goals

- live migration of a provider session;
- automatically borrowing a busy machine after its current job;
- CI preemption;
- arbitrary resource reservations;
- replacing #1174 routing;
- replacing #1057 physical leases;
- making Glaeda the CMUX UI/session database;
- long-lived personal ownership of a specific mini.

## Completion

The feature is complete when an operator who needs more RAM/CPU can request one owned std machine without naming it, receive an idle eligible mini, use it interactively while CI routes elsewhere, and return it cleanly with no manual fleet surgery.
