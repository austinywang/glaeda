# Owned-Linux admission

The local admission gate is the shared physical launch boundary for reviewed owner-local workloads.
`verify-focused run --admission-root <installed-private-root>` was its first production consumer;
`verify-required/v1` now uses the same boundary with its own checked-in demand. An installed adapter
supplies this root; a connected request must never select, override, or omit it. The dispatch v2
focused capability forwards this fixed local option. Ordinary local verification without the option
retains its existing behavior.

The gate admits one reviewed in-process `AdmissionDemand` at a time. A demand contains only the
capacity facts the current host observer can enforce truthfully: candidate memory bytes and a
minimum logical-CPU count. It is constructed by checked-in local adapter code and is never decoded
from remote request bytes. `VERIFY_FOCUSED_DEMAND` exactly preserves the existing
`verify-focused/v1` requirement: 8 GiB MemoryMax and an eight-logical-CPU host floor. The
verification adapter maps `verify-required/v1` to 12 GiB candidate memory and the same reviewed
eight-logical-CPU floor, matching its existing `MemoryMax=12G` execution profile without changing
either profile generation. Future reviewed action/profile adapters may supply another demand only
after their semantic profile identity binds that mapping.

The gate reserves one job in the installed root before source preparation and retains that slot
through physical settlement, task cleanup, and terminal receipt publication. The slot is compute
capacity state, not another work queue or execution/result identity.

Immediately around the existing `Popen`, the gate locks local operator policy, observes the host
through the pinned `glaeda-host-observe` binary, and calls the pinned `glaeda-local-admission`
reducer. Available memory must cover the reviewed candidate demand plus the configured owner
reserve (minimum 4 GiB), and the host must meet the demand's logical-CPU floor. CPU/memory/I/O PSI
avg10 at or above 50%/1%/20% respectively asks the reducer to wait. Missing, malformed, stale,
oversized or unbound observations refuse. The combined observation/reduction must finish within
three seconds, including the final durable launch intent. Binaries are SHA256-bound and executed
through held file descriptors with a closed environment and bounded output.

Only the compatible `admit_now` decision is accepted, including all its false authority fields.
Caller/source/profile authorization remains in the existing adapters and verifier. This policy
is a coexist profile with no quiet-window claim. The gate does not observe an external quiet-lease
store, schedule Windows work, reserve CPU affinity, or guarantee against new unrelated host load.
Existing quiet-window owners must integrate before claiming that gate. No preemption, VM controls,
network widening, or arbitrary commands are added.

## Reviewed demand boundary

`AdmissionDemand` is intentionally an in-process type instead of a request/CLI document. Both
fields must be positive bounded integers; booleans, zeroes, foreign objects, and oversized integers
refuse before host observation or workload launch. `observe(root, demand)`,
`Reservation(..., demand)`, `Reservation.resume(..., demand)` and `recover(..., demand)` consume
the same reviewed value. Fresh `Reservation` construction requires an explicit demand; omitting
it fails before opening the store or observing the host. The advisory `observe()` default remains
compatible with the focused profile.

The demand does not grant execution, resource ownership, preemption, routing, queue, persistence,
or result authority. A remote caller cannot lower its apparent memory/CPU need to bypass admission.
The local adapter that already owns semantic profile authorization owns the mapping from that
reviewed identity to one demand. The command/profile fingerprint and the reviewed demand together
are the semantic binding used for durable reservation and recovery: the `reservation.json` record
(`schema_version` 2) carries the demand, so a resume or recovery that names a different demand
refuses instead of rechecking the launch boundary against the wrong capacity. The record is
evidence to match, never authority: the caller still supplies the reviewed demand. A pre-v2 record
omits part of its own binding, so every reservation, launch, release and recovery path refuses it
as an explicit operator recovery. The advisory `observe()` snapshot still reports it as `reserved`
rather than refusing, because it adopts nothing and takes no action.

For verification, the adapter mapping is deliberately closed: `verify-focused/v1` selects the
existing 8 GiB demand, `verify-required/v1` selects 12 GiB, and any other profile refuses until a
reviewed mapping is checked in. Memory strings or raw CPU numbers are never parsed from a request.
The profile's existing command fingerprint and generation continue to bind its executable recipe,
resource class, systemd properties, source identity, and durable recovery state.

Fresh host availability remains decisive at the final launch boundary. For example, owner-local
coding-agent work that consumes memory after an advisory readiness check reduces the next fresh
`available_bytes` observation; if candidate memory plus owner reserve no longer fits, launch waits.
No process scan, prompt/session inspection, or semantic inference about the competing work is
required.

These generalized slices preserve the installed policy schema and the coexist interference class.
They do not add raw resource fields to Git/GitHub requests, an arbitrary-resource CLI, editable
workspaces, multi-node placement, or GPU/provider acquisition.

## Operator control and installation boundary

Installation is a separate protected action; this change does not install a policy or service.
The installation must provide a private 0700 exact directory containing canonical UTF-8 JSON
`policy.json` (sorted keys, compact separators, newline; regular 0600 single-link file):

- `schema_version`: integer 1.
- `generation`: fresh random 64 lowercase hexadecimal characters identifying this installation.
- `revision`: positive integer, advanced by operator control.
- `node_control`: `held`, `draining`, or `available`; provision held until acceptance finishes.
- `memory_reserve_bytes`: integer at least 4294967296, chosen for the owner's reserved workload.
- `host_executable` and `policy_executable`: objects containing exact absolute `path` and
  `sha256` (`sha256:` plus 64 lowercase hexadecimal characters). Use the reviewed local
  `glaeda-host-observe` and `glaeda-local-admission` binaries, respectively.

Use the serialized control path, never edit policy behind an active launch transaction:

```bash
python3 scripts/owned-admission-control --root <installed-private-root> held
python3 scripts/owned-admission-control --root <installed-private-root> draining
python3 scripts/owned-admission-control --root <installed-private-root> available
```

Control refuses busy while the short check/spawn transaction is active. Retry after observing the
refusal; an unsuccessful control command has not established a hold. The slot lock spans the
whole job, but the policy lock does not, so a hold can prevent the next job while one is running.
Neither held nor draining stops an already-started job. Missing or changed installation policy
refuses new admission. Installation replacement must drain and settle the previous generation.

## Refusal and recovery

A hold/drain/pressure/headroom change during materialization is checked again before process
creation. A pre-launch refusal removes only that attempt's task and intent before releasing the
slot. If cleanup fails, the slot remains. Once process creation may have occurred, exceptions and
crashes retain the durable reservation. Dead PIDs, absent locks, age, and service restarts never
authorize redispatch. A failure between preparing the reservation and acquiring a terminal receipt
remains an explicit recovery case; no automatic stale-reservation deletion is provided.

Normal exact replay reads the existing receipt without entering admission or changing identity.
For recovery after terminal receipt publication but before slot release, call the same exact
verifier request with both `--reconcile-only` and `--admission-root`. Recovery validates the
existing receipt, matching reservation and installation generation, the digest binding the full
source/profile identity and exact command-state directory path/device/inode, then freshly observes
exact unit and task absence, and validates any remaining intent before releasing capacity. It does
not run source or recreate a result. Unsettled or mismatched state stays reserved. The profile is
already part of that binding, so focused and required recovery cannot alias one another even though
they share the same installed admission root.

## Evidence and next integration

`python3 scripts/test-owned-linux-admission.py` covers reviewed demand validation, different memory
and CPU requirements against fresh host headroom, final launch-boundary demand recheck, a resumed
reservation rechecked against its admitted demand rather than the default, resume and recovery
refusal of another demand, durable contention, crash refusal, exact recovery, serialized control,
hold/drain/pressure changes at the real child-launch boundary, pre-launch cleanup, real disposable
child settlement, immutable replay, filesystem substitution, protocol binding, and bounded helper
output. `python3 scripts/test-verify-focused.py` additionally pins both verification profile
generations and command bytes, checks the closed profile-to-demand mapping, and checks that the
required demand reaches both the shared reservation and terminal recovery. These tests use local
children and fixture host facts; they do not prove systemd/bubblewrap required verification or a regular ChatGPT journey.

The next consumer may map another reviewed semantic action/profile to an `AdmissionDemand`, then
prove that exact profile through the same physical admission/receipt path. A generic project-worker
or source-development profile should remain a separate adapter with its own semantic authority and
recovery contract. Service/capability installation still requires its own concrete reviewed action.

## Pending-before-launch observation

`python3 scripts/owned-admission-observe --root <installed-private-root>` keeps the focused verifier
demand for compatibility and returns a bounded `glaeda-owned-admission-observation` v1 JSON
snapshot. It creates no lock, reservation, journal or directory. All authority fields are false.
An in-process reviewed consumer may call `observe(root, demand)` for another already-authorized
profile. A consumer may leave a request pending when `outcome` is `wait`: `node_held`,
`node_draining`, `pressure_high`, `capacity_unavailable`, or `reserved`. A surviving reservation
stays reserved; this observer never infers completion from PIDs, age or lock availability. Invalid
or unavailable observations return `refused` with `observation_unavailable`, without exposing paths,
host facts or exception text.

`ready` / `compatible` is only a disposable scheduling hint. The consumer must still validate
caller/source/profile/capability and the executor must perform its fresh reservation and final
physical launch check. A race after this observation can still refuse at that boundary. The
observer does not schedule retries or publish a terminal result. Operator hold maps to pending for
the consumer; it remains a refusal in the physical reducer and launch path.
