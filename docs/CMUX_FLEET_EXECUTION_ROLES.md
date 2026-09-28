# CMUX fleet execution roles and workload capabilities

Tracking: #1056, #1057, #1058, #546, #743, #760, #840.

This contract keeps four facts separate:

1. enrollment identifies a CMUX-owned node;
2. exact role canary evidence grants eligibility;
3. #546 evidence may classify an eligible role as preferred, neutral, or background;
4. fresh local admission plus a #1057 physical lease owns scarce local resources.

Machine price, machine age, and arrival order carry zero role eligibility. Placement consumes declared workload requirements and accepted/measured node capabilities.

## Closed role vocabulary

The existing fleet names remain canonical:

| Role | Work it can accept after exact canary |
| --- | --- |
| `cmux_macos_native_build` | native Xcode compile, native release, native Glaeda/dev build |
| `cmux_macos_test` | app-host tests and reviewed Mac verification |
| `cmux_linux_ci` | Linux CI, web/browser CI, background verification |
| `cmux_linux_agent` | Linux agent work and reviewed build-helper work |
| `artifact_cache` | artifact service/publication |
| `background_replay` | replay |
| `benchmark` | benchmark |
| `diagnostic` | diagnostic |

Roles stay small. More precise work belongs in the operation and capability vocabulary in `scripts/cmux_execution_roles.py`.

The capability vocabulary is also closed. It includes machine prerequisites and semantic workload abilities:

```text
apple_silicon
cgroup_v2
systemd_execution
task_isolation
linux_ci_toolchain
native_glaeda_build
native_xcode_build
native_release_build
app_host_test
linux_ci
web_ci
linux_agent
build_helper
background_verification
artifact_service
background_replay
benchmark
diagnostic
```

Nodes, role-canary projections, and workload requests reject unknown capability names. This prevents a caller from inventing a capability label and treating it as comparable placement evidence.

Current reviewed operations include:

```text
cmux_macos_compile_admission
cmux_native_dev_build_admission
cmux_macos_release_admission
cmux_macos_app_host_test
cmux_linux_ci_admission
cmux_web_ci_admission
cmux_linux_agent_admission
cmux_build_helper_admission
background_verification
artifact_cache_service
artifact_cache_publish
background_replay
benchmark
diagnostic
```

An operation fixes its role, required semantic capabilities, and local scarce-slot claims. A remote caller cannot invent another operation and have it interpreted as equivalent work.

## Role eligibility

`glaeda-cmux-node-capabilities/v1` describes bounded current node facts:

```text
node ID
lifecycle state
platform / architecture / OS version class
enrollment generation
CPU / memory / disk capability classes
capability generation
current toolchain profiles
declared capabilities
current #1056 role acceptance identities (profile + receipt digest)
fresh pressure classes
```

The CPU, memory, and disk values are reviewed symbolic classes. They are node evidence, not caller-selected raw resource values. The compact role-acceptance identities are projected with `fleet_role_acceptances()` from receipts that first pass #1056's closed `validate_acceptance_receipt()` and match the exact node/enrollment generation; callers do not supply acceptance digests or role profiles.

A role becomes eligible only when:

- the node is in `eligible` or `active`;
- platform and architecture match;
- reviewed CPU/memory/disk classes meet the role minimum;
- the node advertises every role prerequisite capability;
- the node carries a current #1056-reviewed role acceptance identity: exact CMUX profile ID/generation plus exact finalized acceptance-receipt SHA-256;
- a role canary exists for the current enrollment and capability generation;
- the canary names that exact current #1056 profile and acceptance-receipt digest;
- the canary result is `accepted`;
- the canary accepted every role prerequisite capability;
- an exact current toolchain profile is bound when the role requires one.

Enrollment or a canary alone therefore grants zero role eligibility. Roles reserved by #1056 remain non-routable here even if this vocabulary already names their future capability/slot classes.

## Toolchain generations

Toolchain identity is an exact reviewed profile token, for example:

```text
apple-xcode-27-sdk-27
linux-rust-ci-2026-09
```

The token represents the exact observed generation accepted by the canary. Mac compile/test work names the required profile explicitly.

A toolchain/OS/Glaeda-relevant upgrade advances the node capability generation and replaces the current toolchain profile. A new #1056 role acceptance also changes the bound receipt digest/profile identity. Old role canaries then become stale automatically and routing reports the corresponding canary/acceptance reason until fresh acceptance exists.

This allows one profile generation to be accepted while another generation remains ineligible.

Apple toolchain canary runs in the #1056/#1058 acceptance lifecycle that produces fresh role-canary evidence. It is deliberately outside ordinary placement: a canary must be able to establish eligibility without already possessing the eligibility it is trying to prove.

## Workload requirements

A workload names semantic requirements rather than a machine:

```json
{
  "schema": "glaeda-cmux-workload-requirement/v1",
  "operation": "cmux_macos_compile_admission",
  "role": "cmux_macos_native_build",
  "platform": "macos",
  "architecture": "arm64",
  "toolchainProfile": "apple-xcode-27-sdk-27",
  "minimumCpuClass": "medium",
  "minimumMemoryClass": "medium",
  "requiredCapabilities": ["native_xcode_build"],
  "resourceProfile": "medium"
}
```

The request has no node selector, raw CPU count, RAM bytes, cgroup/systemd settings, host paths, or physical slot IDs.

The reviewed resource profiles are:

```text
small
medium
large
exclusive
```

A role canary states which resource profiles were accepted for the exact current #1056 role profile/receipt. A separate capacity receipt must then prove the resource profile on the exact current node generation.

## Measured resource profiles

`glaeda-cmux-role-capacity/v1` backs local concurrency with #760-style evidence.

Each accepted capacity receipt carries:

```text
role
resource profile
scarce slot class
maximum concurrent work
contention evidence generation
validated completions
p50 / p90
CPU pressure
memory pressure
swap start/peak/end bytes
thermal behavior
unfinished work
```

Only current accepted receipts contribute capacity. Missing, stale, rejected, or wrong-profile evidence produces `resource_profile_unmeasured`. The #760-style cohort counts must reconcile, all offered work must settle, every settled task must be a repository-validated completion, and an accepted `maxConcurrent=N` requires at least N simultaneously observed tasks and N validated completions. When an operation claims multiple scarce slots, every required slot must be present in one complete `contentionEvidenceGeneration`; the reducer never assembles a synthetic capacity claim from unrelated measurement generations. Accepted capacity also requires bounded CPU/memory pressure; unknown or critical CPU/memory evidence grants zero slots. Rejected measurements may retain unfinished-work counts for diagnosis but grant no capacity.

This can express measurements such as:

```text
mac_native_build_lane = 1
mac_native_heavy_slot = 2
mac_app_host_test_slot = 2
linux_medium_slot = 4
linux_heavy_slot = 1
browser_ui_test_slot = 1
artifact_publisher_slot = 1
artifact_service_slot = 1
background_replay_slot = 2
```

The exact values come from reviewed contention windows. They are never inferred from hardware marketing labels.

For example, a Mac can measure `mac_native_heavy_slot = 2`, `mac_native_build_lane = 1`, and `mac_app_host_test_slot = 2`. That admits one compile plus one app-host shard, or two app-host shards, while a second concurrent compile still refuses on the one-lane compile limit. A Linux node can independently prove `linux_medium_slot = 4` and refuse the fifth medium job.

## Scarce-resource ownership

Operation contracts derive local slot claims. Callers do not supply them.

Examples:

- Mac compile/dev-build -> `mac_native_build_lane` + `mac_native_heavy_slot`;
- Mac release -> `mac_native_build_lane` + `mac_native_heavy_slot` + `mac_release_universal_slot` + `artifact_publisher_slot`;
- app-host test -> `mac_app_host_test_slot` + `mac_native_heavy_slot`;
- Linux medium work -> `linux_medium_slot`;
- Linux large/exclusive work -> `linux_medium_slot` + `linux_heavy_slot`;
- web CI -> `browser_ui_test_slot` + `linux_medium_slot`, adding `linux_heavy_slot` for large/exclusive profiles;
- artifact service -> `artifact_service_slot`;
- artifact publication -> `artifact_publisher_slot`;
- replay/benchmark/diagnostic work -> their closed `background_replay_slot`, `benchmark_slot`, or `diagnostic_slot`.

The slot-class vocabulary is closed. Capacity receipts and physical leases reject caller-invented slot names.

`glaeda-cmux-physical-lease-observation/v1` is the #1057 collision input:

```text
node ID
lease ID
lease generation
owner namespace
state
slot claims
exclusive flag
```

Lease states `active`, `releasing`, and `unsettled` all continue to consume their claims. This prevents a second orchestrator from treating incomplete settlement as free capacity.

Fresh local admission reduces the physical leases into current slot usage, rechecks current pressure, and refuses when a required claim is full. Critical or unknown fresh CPU/memory/swap/thermal evidence also refuses admission. Exclusive work refuses while any other physical lease is live; an existing exclusive lease blocks new work.

A successful admission result remains `authority = admission_only` and names `leaseBoundary = physical_execution_lease`. Launch still requires atomic #1057 lease acquisition/binding. The admission report itself does not own the resource.

## Candidate selection stays advisory

`select_eligible` answers whether the current node evidence satisfies one workload. It grants no execution or ownership.

`local_admission` repeats that check against fresh node evidence and then applies pressure and physical-lease collision checks.

Therefore an external scheduler can select node A and still receive a clean refusal when, before launch:

- the node drains;
- its exact toolchain profile disappears;
- a role canary becomes stale;
- pressure becomes critical;
- another orchestrator acquires the scarce slot;
- an earlier physical lease remains releasing/unsettled.

## Specialization stays separate

`glaeda-cmux-placement-preference/v1` accepts:

```text
preferred
neutral
background
```

with `authority = observation_only`. Each preference also names one reviewed operation and its matching role, so a Mac may be preferred for compile while release remains neutral, or preferred for app-host tests while compile stays merely eligible.

Preference can never create eligibility, canary acceptance, measured capacity, or physical ownership. #546 can eventually produce this evidence from complete-loop placement history.

The separation is:

```text
eligible     = exact role acceptance
preferred    = operation-specific adaptive placement evidence
background   = operation-specific adaptive placement evidence
owned slot   = #1057 physical lease
```

## Founder/operator view

The compact status projection exposes only fleet concepts:

```text
node cmux-mac-001
eligible:
  cmux_macos_native_build
temporarily unavailable:
  cmux_macos_test: fleet_acceptance_pending
  artifact_cache: node_capability_missing
capacity:
  mac_native_build_lane: 1
  mac_native_heavy_slot: 2
preferred:
  cmux_macos_compile_admission
background:
  background_replay
state: active
```

Operators do not need task IDs, cgroup values, host paths, cache directories, process inventories, or internal lease storage details.

## Synthetic acceptance

`scripts/test-cmux-execution-roles.py` covers the required cases:

- node meets exact role/workload requirements;
- wrong Xcode/toolchain profile;
- insufficient memory class;
- insufficient CPU class;
- failed role canary;
- stale canary profile or acceptance-receipt digest;
- fabricated current acceptance for a role that #1056 has not reviewed;
- draining node;
- role removed after upgrade/capability-generation change;
- multiple roles sharing one scarce slot;
- unrelated caller namespaces colliding on the same physical slot;
- releasing/unsettled physical lease still holding capacity;
- external scheduler request for an ineligible role;
- node pressure appearing between selection and local admission;
- resource profile without reviewed measurement;
- four-job Linux-style measured capacity;
- preference remaining advisory;
- background preference remaining separate;
- workload carrying no machine selector or raw CPU/RAM/cgroup control;
- current #1056-admitted Mac/Linux roles becoming eligible while reserved future roles remain `fleet_acceptance_pending`;
- compact operator status.

## Physical acceptance later

The first hardware proof should retain exact receipts for:

1. one Mac with at least two accepted roles;
2. one Linux node with at least two accepted roles;
3. incompatible workload placement refusing cleanly;
4. two caller classes contending through the same #1057 physical-lease boundary;
5. one measured concurrency policy per platform from #760-style windows;
6. a toolchain generation change removing affected eligibility until re-canary;
7. a pressure/drain change between remote selection and local admission producing a refusal.
