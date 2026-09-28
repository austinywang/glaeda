# Accelerator burst execution

Status: design plan for a provider-neutral first slice. Physical rented-GPU experiments are owned by Leo Workspace issue #462; reusable runtime semantics stay in Glaeda.

## User experience target

Keep the operator on the local machine and make expensive accelerator capacity feel like an attached execution capability.

```text
glaeda run/open/enter
-> declare or inherit accelerator requirements
-> reuse an already-hot eligible lease when one exists
-> otherwise select an eligible observed target
-> attach valid persistent project/cache state
-> execute or open the interactive session
-> retain the expensive target only for a bounded idle grace period
-> detach/terminate accelerator capacity while cheaper state survives
```

The eventual product interface may use `run`, `open`, or `enter`; this first slice does not add mutation or provider commands.

## First coded slice

Implement a pure, read-only accelerator request/target/plan vocabulary.

It should express at least:

- accelerator vendor/API class such as NVIDIA/CUDA;
- minimum accelerator memory;
- batch versus interactive intent;
- an explicit maximum acceptable RTT for interactive work;
- expected useful-work duration when a workload adapter has a defensible estimate;
- candidate accelerator memory, region class, observed RTT, startup-to-useful estimate, hourly price, and warm/cold lease state;
- explicit eligibility/refusal reasons;
- deterministic ranking that strongly prefers an already-hot eligible lease, then predicts earliest useful completion among cold candidates, with cost as an inspectable input/tie-break rather than hidden authority.

The plan grants zero provisioning, billing, credential, network, storage, remote-desktop, execution, lease, or teardown authority.

## Why this belongs above provider APIs

The desired semantics survive provider churn. A job should ask for a capability such as `NVIDIA + >=24 GiB VRAM + interactive`, while Runpod/Vast/another provider remains an adapter choice.

Glaeda already separates semantic workload identity from physical execution requests. Accelerator sizing belongs to physical request/placement when changing GPU class does not change the accepted result. Workload families may still use `ComputeWorkloadIdentity.required_capabilities` when an exact runtime/API capability changes semantics.

## State placement

The expensive accelerator should be disposable independently of useful persistent state.

```text
local machine
  source edits / Git identity / input

cheap persistent remote tier
  large assets / model weights / package and compiler caches / prepared environment

expensive accelerator lease
  attached only while active GPU work exists
```

Do not make a rented GPU VM the sole owner of expensive-to-rebuild project state.

## Native-feeling process semantics

Later execution adapters should preserve ordinary local expectations where the transport permits it:

- bounded streaming stdout/stderr;
- truthful exit status;
- cancellation propagation;
- reconnect to an existing lease after brief client interruption;
- exact source/project generation binding;
- explicit output materialization policy;
- idle grace followed by scale-to-zero.

Interactive applications should use a separate presentation adapter (`open`) over measured mature remote-display transports. Glaeda should not become a remote-desktop implementation.

## First physical proof

Leo Workspace #462 owns the human experiment. Before a provider adapter is promoted, prove manually:

1. one nearby rented NVIDIA/CUDA target;
2. one real batch workload;
3. one real 1440p-ish/60 interactive graphical workload;
4. startup-to-useful and shutdown friction;
5. persistent project/cache state surviving accelerator teardown where supported;
6. exact billed GPU/storage/network cost;
7. local-versus-remote interaction quality.

A negative interactive result may classify the provider/transport as batch-only without invalidating accelerator burst execution.

## Promotion sequence

1. pure request/observation/plan contract;
2. manual Leo Workspace #462 receipt;
3. one narrow provider adapter behind the same contract;
4. explicit `accelerator up/down` experiment only if manual friction warrants automation;
5. command-level `run` semantics with hot-lease reuse and idle grace;
6. `open` interactive presentation adapter;
7. eventually fold eligible accelerator targets into the broader heat/cost/completion-time router.

## Guardrails

- no provider credentials or raw provider payloads in public receipts;
- no hidden spend from a read/plan command;
- no arbitrary shell introduced as part of accelerator routing;
- no opaque optimizer: every eligibility/ranking input stays inspectable;
- no assumption that GPU-hours are interchangeable; equal-work receipts must retain GPU class, wall time, VRAM, result identity, and actual cost;
- no automatic purchase recommendation from synthetic pricing;
- no persistent expensive accelerator merely to preserve state that belongs on a cheaper tier.
