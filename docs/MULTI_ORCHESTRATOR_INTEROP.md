# Multi-orchestrator execution interoperability

Status: accepted direction; implementation owner: #1057.

Related: #1050, #546, #967, #1010, #1012, #970, #770, #1056, #1033.

## Purpose

One CMUX-owned machine may serve GitHub Actions, CMUX controller/build tooling,
Glaeda placement, direct agents, operator commands, and other reviewed fleet
tools at the same time.

Each caller keeps its own workflow semantics. Glaeda supplies the compute-side
truth needed to share scarce physical resources safely:

- fresh local admission;
- exact physical ownership;
- hot-state validity;
- bounded execution and settlement;
- cache/project-state lifecycle;
- bounded receipts.

The interoperability contract has three independent identities.

## Caller identity

Caller identity comes from the adapter that authenticates the request.

A caller identity carries a bounded class and namespace plus an external request
reference, for example:

```text
caller_class = github_actions | cmux_ci | cmux_build | direct_agent | operator | ...
caller_namespace = authenticated adapter/principal scope
external_request_ref = caller-owned correlation id
```

A GitHub Actions job and a CMUX build command may ask for the same semantic
operation while retaining separate caller identity.

External references are namespaced by caller identity. Reusing one external
string in an unrelated caller namespace creates an independent request.

Caller identity carries correlation and authentication. It carries zero
physical ownership by itself.

## Semantic workload request

The semantic request describes useful work without exposing physical machine
internals.

The provider-neutral request family can carry:

```text
project/repository identity
exact source identity
operation/profile identity
requested capability class
reviewed priority/latency class
deadline
reuse hint
```

Caller class and external request reference travel alongside the semantic
request for correlation. They stay outside the physical lease identity unless a
specific workload contract requires otherwise.

The semantic boundary excludes:

- host paths;
- raw cgroup or service-manager properties;
- caller-selected physical machine IDs as execution authority;
- cache directories;
- internal task or attempt IDs;
- arbitrary environment;
- shell command strings;
- unrestricted argv.

Reviewed adapters compile semantic work into local profiles. #1050 owns this
request seam.

## Physical execution lease

The physical lease is the local ownership claim for scarce resources.

Glaeda issues the lease from fresh local state, or validates an externally owned
CMUX reservation and binds a local lease generation to that evidence.

A lease binds enough physical truth to fence concurrent users:

```text
node identity/class
resource slot claims
workspace/task generation
cache/hot-state generation where applicable
network class
execution lifetime
lease generation
ownership provenance
```

Useful resource claims include:

```text
mac_native_build_lane
linux_heavy_slot
project_native_lock:<project>
artifact_publisher_slot
resident_workspace:<project/generation>
```

Every participating adapter joins the same local collision boundary before
launch. Process names, runner liveness, PIDs, and an apparently idle machine
carry no ownership authority.

Lease release follows bounded process settlement, cleanup, and fresh ownership
observation.

## External scheduler compatibility

An external scheduler can participate in two ways.

### Candidate node only

```text
scheduler selects node A
-> node A receives caller identity + semantic request
-> node A freshly validates hold/drain/pressure/interference/resources
-> Glaeda accepts and creates the local lease, or returns wait/refusal
```

Candidate-node selection is advisory. A stale remote placement decision never
bypasses current node admission.

### External reservation already exists

A reviewed adapter can present bounded reservation evidence when another CMUX
system already owns the machine or a resource slot.

Glaeda validates:

- reservation owner/caller namespace;
- exact node/resource scope;
- reservation generation;
- validity/expiry;
- current local compatibility;
- conflict with existing local leases.

On success, Glaeda binds a subordinate local lease to that reservation. This
avoids a second competing reservation system on the same machine.

Ambiguous, stale, expired, or conflicting reservation evidence yields
wait/refusal and preserves the existing owner.

## Adapter composition

### GitHub Actions

```text
Scale Set / JIT assignment
-> GitHub caller identity
-> semantic workload
-> local lease/admission
-> bounded execution
-> receipt
```

#1010 owns GitHub-specific JIT lifecycle semantics.

### CMUX native build

```text
cmux build request
-> CMUX caller identity
-> native Apple semantic profile
-> project/build-lane lease
-> cache/hot-state validation
-> bounded execution
-> receipt
```

### Direct agent/operator request

```text
authenticated direct request
-> caller identity
-> semantic workload
-> same local lease family
-> bounded execution
-> receipt
```

#1012 owns direct transport composition.

## Placement and heat evidence

#546 can score or select an execution target from bounded observations. #970
capability/heat snapshots can improve that choice.

Both remain advisory. A placement decision becomes executable only after fresh
local admission and lease acquisition/binding.

This preserves Glaeda's local correctness checks when CMUX later adopts another
fleet scheduler.

## Replay and deduplication

Replay follows caller namespace plus semantic request identity:

- same caller namespace + same external ref + same semantic request: resume or
  replay the existing durable execution state;
- same caller namespace + same external ref + semantic drift: refuse conflict;
- unrelated caller namespaces + same external string: independent requests;
- semantic equality across unrelated callers does not silently merge physical
  ownership;
- uncertain prior execution cannot become a fresh second launch.

Intentional result sharing across callers uses a separate workload-owned
reusable-result contract.

## Receipt contract

Every physical execution receipt carries bounded correlation and physical truth:

```text
caller class
external request ref
semantic workload identity
physical node class / opaque id
lease generation
start/end
result
cleanup/settlement
```

Caller-private workflow state stays outside Glaeda. A receipt describes what
Glaeda observed and settled; the caller adapter maps that evidence into its own
workflow state.

## Required tests

Exercise:

- GitHub Actions and direct CMUX requests arriving simultaneously;
- an agent request while a native build owns the project lock;
- a selected node becoming pressured before admission;
- caller cancellation;
- caller disappearance;
- replay of the same caller-scoped external ID;
- semantic drift under the same caller-scoped external ID;
- unrelated callers using the same external ID;
- node drain between selection and start;
- local completion after caller timeout;
- valid external reservation import;
- stale/expired/conflicting external reservation evidence;
- incomplete settlement blocking conflicting reuse.

The tests must prove collision behavior from typed ownership evidence rather
than runner/process-idle heuristics.

## First physical proof

Use one CMUX-owned machine and at least two caller classes.

Preferred proof:

```text
GitHub Actions compile request
+
direct CMUX native build request
```

A Linux equivalent is acceptable when that hardware is available first.

Acceptance requires distinct caller identities, typed semantic requests, one
shared local lease owner, correct serialization/refusal for conflicting claims,
safe coexistence for compatible claims, bounded receipts, and no duplicate
launch after replay or ambiguous prior execution.
