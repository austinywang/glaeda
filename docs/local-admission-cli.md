# Local interference policy interface

`glaeda-local-admission` lets checked-in owned-execution adapters call Glaeda's existing
`local_interference_admission` reducer without implementing another policy engine. It reads one
JSON document from stdin (maximum 4096 bytes) and emits one bounded JSON decision. It accepts no
arguments and performs no host observation, reservation, queue, or execution action.

```json
{
  "schema_version": 1,
  "request": {"interference_class": "coexist"},
  "observation": {
    "observed_at_unix_millis": 1000,
    "node_control": "available",
    "pressure": "low",
    "candidate_quiet_compatibility": "unknown",
    "quiet_lease": null,
    "active": {"conflicting_non_yieldable": 0, "conflicting_yieldable": 0}
  }
}
```

The observation must be constructed from fresh local evidence by the controller, never accepted
as a remote caller's assertion. The controller must check that `input_sha256` matches its exact
stdin bytes and interpret the full decision. Exit zero means a policy decision was produced,
including `wait` or `refuse`; it does not mean work may launch. Invalid protocol input exits two
with a closed error code and never echoes the input. Unknown fields and invalid enum values refuse.

Both the envelope and decision retain `authorizes_execution=false` and `grants_authority=false`.
Source/caller authorization, capacity, any required quiet lease or drain, durable reservation,
and fresh observation at the physical launch boundary remain the consuming gate's responsibilities.
There is no executable policy shortcut from `admit_now` to process launch.

The existing reducer and its tests own interference semantics (#1000). Physical consumption by
the shared owned-Linux gate remains the next #1012 integration; this interface alone does not
activate an unattended worker or change operator policy.
