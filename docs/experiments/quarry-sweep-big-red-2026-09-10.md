# Quarry research sweep: Big Red triangulated batch result

Status: experiment adapter landed as `scripts/quarry_sweep_impl.py` with unit
gate `scripts/test-quarry-sweep.py` (25 tests). The measurements are
performance observations, not source-validity, publication, merge, or
result-reuse authority.

## Landed slice

One Glaeda-managed batch runs a fixed, deterministic Quarry research sweep —
seven in-repo sample backtest configs, sequential, offline, through Quarry's
exact `run_backtest` code path — inside the shared task-private
systemd/bubblewrap boundary (`scripts/owned_linux_task.py`, network `none`,
closed environment, absolute argv only) and returns one bounded canonical
receipt (`glaeda-quarry-sweep-receipt`, stdout only).

The slice admits, executes, and returns bounded evidence:

```text
exact resident Quarry commit/tree + clean checkout + cpython-3.14 identity
-> experiment admission gate (pressure, memory/CPU reserve, VPN lane, RDP observed)
-> task-private materialization from resident Git at the exact commit/tree
-> systemd-run grant: CPUQuota=200%, MemoryHigh=3G, MemoryMax=4G,
   TasksMax=128, RuntimeMaxSec=600, network none
-> fixed driver: 7 x evaluate_backtest_config via quarry.cli.run_backtest
-> pre-opened task-private evidence file (64 KiB ceiling, strict schema)
-> bounded stdout capture + digest, unit settlement, task cleanup
-> one canonical receipt: source, toolchain, workload, grant, admission
   before/after, per-config digests, outer terminal/cleanup observation
```

No queue, daemon, scheduler, remote CLI, or caller-selected command, mount,
or resource property. The verifier remains behaviorally untouched; the
adapter reuses its physical mechanics without cloning them.

## Admission: VPN lanes and interactive RDP are never starved

The batch carries no network traffic (bubblewrap `none`, in-repo sample
data only) and is capped at 2 of 16 CPUs / 4 GiB of 30 GiB, leaving 14 CPUs
and ~26 GiB plus the full pressure headroom for Tailscale, RDP, and owner
work. The hard gate refuses before any materialization when:

- the Tailscale lane is not `IFF_UP` (`refuse: vpn_lane_impaired`);
- PSI `avg10` exceeds cpu 30 / memory 10 / io 30 (`wait: pressure_high`);
- available memory is below the 8 GiB job+owner reserve, or fewer than
  6 CPUs are visible (`wait: ..._reserve_unmet`);
- resident source, origin, cleanliness, or the Python 3.14 identity mismatches.

RDP (`:3389` LISTEN) is observed and recorded before/after every attempt
rather than required, so a legitimately stopped desktop never blocks batch
while interactivity stays protected by the quota + pressure gates.

## Complete comparable loop

Exact workload: Quarry `328bc223` / tree `36eb9ba7`, driver
`sha256:076490b681579c2c2043b89c9d00acee5732d98aa2b75042464fd412a7f01520` (also in the receipt), `/usr/bin/python3`
3.14.4, seven configs fixed in `SWEEP_CONFIGS`. Interleaved serial
control, 1 warmup + 5 measured samples per arm, same box, same head.

| Arm | Median outer wall | Median inner exec | Worker-visible/result bytes | Outer calls | Result |
| --- | ---: | ---: | ---: | ---: | --- |
| Cold direct (`cold`) | 146 ms | 93.273 ms | 6,945 stdout | 1 | 7/7 succeeded |
| Glaeda managed (`run`) | 624 ms | 187.274 ms | 6,944 stdout | 1 | 7/7 succeeded, settled, cleaned |

The managed path is slower on this sub-second workload: about 480 ms of
outer overhead (task materialization, sandbox spawn, settlement, cleanup)
plus ~94 ms inner (bubblewrap/systemd launch). No speedup is claimed and
none would be credible here. The product win is admission, isolation, and
bounded identity-bearing evidence for an external workload that previously
had none: one typed receipt replaces procedural rediscovery of what ran,
under what grant, against which exact source.

## Semantic equality (the actual claim)

All seven per-config report digests were byte-identical across all ten
measured runs (5 cold + 5 managed), e.g. `ma_cross_sample.json →
sha256:84aa459d…` in every receipt. Whole-stdout digests differ only in
the evidence line's embedded per-config elapsed timings, which are
explicitly outside the identity. VPN stayed up and the RDP listener stayed
present before and after every managed run; pressure stayed at
cpu 0.01 / io 0.0 / memory 0.0.

Negative controls: a bogus commit refuses with exit 75 and a typed
`glaeda-quarry-sweep-error` before any materialization; no `/tmp`
task state and no systemd unit survive any run; both the Glaeda and Quarry
checkouts stayed clean throughout.

## What this does not do

- No strict Quarry v2 receipt decode: `parallel-full --receipt-stdout`
  (Quarry #1120) is not in this Quarry checkout, so the receipt binds
  outer terminal/cleanup evidence plus per-config report digests, and
  proves equality against the cold control rather than decoding a
  repository-canonical receipt.
- No durable replay store: each attempt executes fresh under a
  workload-identity unit name that refuses on collision.
- No production admission-root installation or machine mutation.

## Proposed second workload

Quarry `parallel-full` per Glaeda #1011 is the natural next triangulation:
real batch weight (full corpus, internal 1–15 worker shards) where the
quota/pressure gates matter physically. Two gaps block it: (1) Quarry
`--receipt-stdout` for the strict stdout receipt channel; (2) a reviewed
Python dev-toolchain closure (pytest) for the network-none sandbox —
`parallel-full` currently requires the resident `.venv`, which cannot be
rebound into a task-private view without breaking its absolute-path
identity. Both are bounded, named, and independent of this slice.
