# Glaeda Runner Scale Set bridge

This is a deliberately small process boundary around GitHub's official
[`actions/scaleset`](https://github.com/actions/scaleset) Go client. The module pins exact commit
`cb0405b2d874500e75ae34eff8d582ab75956b45` through the Go pseudo-version in `go.mod`.

The bridge exists so Glaeda does not reimplement GitHub's Runner Scale Set authentication,
message-session refresh, long polling, JIT configuration, or runner API semantics in Rust. It does
not use the client's convenience listener because that listener acknowledges a message before it
invokes lifecycle handlers. Glaeda needs this ordering instead:

```text
poll through official client
-> return one bounded normalized message to Rust
-> persist/reconcile it under the durable Glaeda lock
-> explicit ack from Rust
-> delete the message and acquire its available job request IDs
```

## Process contract

The binary rejects arguments, clears its inherited environment, and exchanges one bounded JSON
object per line on stdin/stdout. A bounded strict response gate rejects unknown service lifecycle
types before the pinned official client can normalize them. Protocol version 2 supports:

- `start`: accept the GitHub App private key through stdin, verify one already-enrolled exact scale
  set by ID/name/group/labels/disabled-update policy, and create a message session;
- `poll` and `ack`: expose the latest validated statistics and bounded job lifecycle messages at an
  explicit capacity from zero through the enrolled maximum, without automatic acknowledgement,
  and accept only positive unique acquired IDs from the exact persisted available-job set;
- `resume`: restore one positive durable acknowledged-message cursor exactly once in a fresh
  process before polling, so zero-capacity lifecycle observation cannot admit another job;
- `generate_jit`: return one exact runner ID/name plus its one-time encoded JIT configuration;
- `observe_runner` and `remove_runner`: observe by exact name and remove only after re-observing the
  exact numeric ID, name, and configured scale-set identity.

Errors contain fixed codes only. The App private key never appears in argv, the environment,
stdout, or an error. The encoded JIT configuration appears only in the successful `jit` response
and remains secret-bearing data for the future Rust/guest handoff.

GitHub.com can directly assign organization Scale Set work with `runnerRequestId=0`, without a
preceding Available event. For that shape only, the bridge derives a stable positive private join
key from the exact job and assignment evidence. Distinct assignment times remain distinct, and the
Rust consumer reserves capacity directly from the Assigned event without calling `AcquireJobs` or
claiming a runner identity before JIT registration. A runnerless `Completed(canceled)` for that
exact assignment releases and retires the untouched reservation without acquiring VM or runner
deletion authority. The Quarry pilot observed GitHub.com repeat the exact nanosecond assignment
timestamp from message `100000001` (`Assigned`) in message `100000002`
(`Completed(canceled)`), for workflow run `31951901986`; this is the bounded live-service evidence
for the private join across the pre-runner reassignment lifecycle.

## Current nonclaims

The private Rust adapter and durable delivery controller now call this package, load the
pre-enrolled App key from the Mac Keychain, persist/reconcile messages before acknowledgement, and
recover ambiguous acquisition without replaying acknowledgement. This package still does not
create or adopt a scale set, launch a VM, transfer JIT data to a guest, supervise a runner, or by
itself settle an empty ambiguous acquisition. Cursor restore and zero-capacity polling are only the
process prerequisite for the next durable lifecycle-evidence transaction, not a usable autoscaler.
