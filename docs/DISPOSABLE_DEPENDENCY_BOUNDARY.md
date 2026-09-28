# Disposable runner dependency and adapter boundary

Status: decision candidate for [#368](https://github.com/teamleaderleo/smolrunner/issues/368), source-reviewed 2026-08-11.

Glaeda should stay small by owning the lifecycle facts that make an operator-owned Mac safe to use as hostile CI capacity and delegating mature protocol, VM, OS, networking, caching, and supervision primitives to projects that already do those jobs well.

This note records the M2–M4 dependency boundary after comparing nearby runner managers and Apple-silicon VM backends. It also records why the current direct `actions/scaleset` + Lima/VZ path remains the default.

## Decision

1. **Glaeda remains the durable lifecycle authority.** It owns host-wide admission, attempt identity, job/runner/worker binding, crash recovery, cleanup authority, capacity release, trust policy, and bounded status/recovery debt.
2. **Keep GitHub's pinned official `actions/scaleset` client behind `tools/scaleset-bridge`.** GitHub owns scale-set sessions, long polling, acknowledgement/acquisition semantics, JIT generation, and runner service APIs. Glaeda persists and reconciles a polled message before explicitly acknowledging it.
3. **Keep Lima/VZ as the first disposable VM backend.** Backend-specific commands and filesystem details stay private to the adapter. The durable core reasons in Glaeda attempt/worker identities.
4. **Use a tiny exec-only guest JIT handoff when the runner's supported startup interface would expose the secret in argv.** The helper receives one bounded one-time value over a controller-only channel and starts the exact pinned runner. It has no general command or shell surface.
5. **Keep hostile-CI network policy intent backend-independent.** Required policy is inbound deny plus egress denial to the Mac host, private/LAN, link-local, metadata, controller, and peer-worker destinations while preserving explicit DNS and ordinary public CI traffic across IPv4 and IPv6.
6. **Treat Tart/Softnet as the first serious alternative backend/network benchmark, not an automatic switch.** Their capabilities are relevant; their FSL licensing and current Lima investment make adoption an explicit product decision.
7. **Prefer mature cache and supervision components.** GitHub Actions cache/artifacts, `sccache`, BuildKit cache backends, and `launchd` should carry their existing responsibilities before Glaeda grows another cache protocol, CAS, package manager, VM monitor, or supervisor.

The result is deliberately asymmetric: Glaeda owns fewer mechanisms than neighboring runner managers while keeping the exact lifecycle facts that determine safe cleanup and resource release.

## Source, version, and license matrix

These are the source-review points used for this decision. Any component promoted into a required distribution dependency needs its own exact release/digest/license pin at that time.

| Component | Source reviewed | License observed | Useful responsibility | Decision |
|---|---|---|---|---|
| GitHub `actions/scaleset` | commit `cb0405b2d874500e75ae34eff8d582ab75956b45` | GitHub upstream | Scale-set sessions, polling, acknowledgement/acquisition, JIT and runner APIs | **Direct dependency through the existing pinned bridge** |
| GARM | v0.2.1; scale-set path stable from v0.2.0 | Apache-2.0 | Full runner manager, providers, durable controller state, scale-to-zero | Borrow provider/recovery ideas; keep Glaeda as outer controller |
| GHA Outrunner | v1.2.0 | MIT | Direct scale-set controller with small Docker/libvirt/Tart provisioners | Borrow the thin provisioner idea; keep Glaeda lifecycle ownership |
| Graftery | `main` inspected 2026-08-11 | Apache-2.0 | Mac-native scale-set controller, Tart clones, prepared images, warm pools | Strong design prior art; too young to become a core dependency today |
| Lima | v2.2.0 | Apache-2.0 | Apple VZ lifecycle, templates, cloning, guest provisioning/control | **First VM backend** |
| Tart | v2.34.0 | FSL-1.1-ALv2 | Apple VZ lifecycle, OCI image distribution, automation | Benchmark alternative; explicit licensing/product decision before adoption |
| Softnet | v0.22.1 | FSL-1.1-ALv2 | Tart-specific userspace packet filtering with dynamic policy | M4 reference/benchmark candidate; gaps remain for the complete policy |
| Lume in Cua | v0.4.0 | Cua repository MIT | Higher-level Apple VZ management for macOS/Linux guests | Benchmark after a measured Lima gap; recheck exact package/release license before adoption |
| vfkit | v0.6.4 | Apache-2.0 | Low-level Apple Virtualization.framework VM runner | Keep as low-level fallback/research option |

### Licensing consequence

Lima, GARM, Graftery, and vfkit are permissive Apache-2.0 projects; Outrunner and the Cua repository are MIT. Tart and Softnet currently use FSL-1.1-ALv2, with an Apache-2.0 future license for each covered release after the license's stated interval. Making Tart or Softnet a required product dependency therefore needs an explicit release/distribution review.

## GitHub scale-set controller boundary

### Direct pinned `actions/scaleset` — selected

The existing `tools/scaleset-bridge` is the preferred boundary. It pins the official Go client and separates polling from acknowledgement:

```text
poll through official client
-> return one bounded normalized message to Rust
-> persist/reconcile under the Glaeda durable lock
-> explicitly acknowledge/acquire the exact persisted message
```

That ordering preserves Glaeda's recovery model. GitHub owns wire/session behavior; Glaeda owns the durable meaning of each message for local capacity.

The narrow M3 adapter should expose only capabilities equivalent to:

```text
validate_scale_set(exact enrolled identity)
poll() -> bounded unacknowledged message + service statistics
ack(exact persisted message, exact acquired request ids)
generate_jit(exact durable runner identity)
observe_runner(exact numeric id/name/scale-set identity)
remove_runner(exact observed runner identity)
```

Keychain access, durable attempt state, capacity reservation, worker creation, actual-job binding, JIT guest delivery, teardown, and capacity release remain Glaeda responsibilities.

### GARM — borrow interfaces, keep outside the control path

GARM is a complete runner manager. Its stable v0.2 scale-set path already owns GitHub polling, JIT generation, providers, durable controller state, cleanup, and scale-to-zero. Its external provider boundary is valuable prior art.

Putting GARM above Glaeda would create two durable authorities: GARM's controller/database would own runner lifecycle while Glaeda's catalog owns admission, worker identity, cleanup proof, and capacity release. Ambiguous registration or teardown would then require a cross-controller recovery protocol at the exact seams Glaeda already models directly.

GARM agent mode also includes broader administration such as remote-shell access. That serves GARM's product and exceeds Glaeda's intended authority surface.

**Decision:** borrow provider timeout/recovery ideas and retain the direct official Scale Set client.

### GHA Outrunner — borrow the thin provisioner model

Outrunner provisions a fresh container or VM per job using the Scale Set API and has small Docker, libvirt, and Tart provisioners. It is strong evidence that a future Glaeda backend interface should stay tiny.

Outrunner still owns the overall controller/provisioner lifecycle. Replacing Glaeda's reconciler with it would discard the host-global reservation ledger and exact crash/replay joins already implemented.

**Decision:** borrow the small provisioner idea, especially for backend benchmarks.

### Graftery — product prior art

Graftery is the closest product comparison: Mac-native scale-set integration, a fresh Tart VM per job, JIT injection, content-keyed prepared images, warm pools, orphan cleanup, and scale-to-zero. Those ideas are useful for M6 image preparation and latency work.

The project remains young at this review point, so it is stronger as design evidence than as a dependency Glaeda should build around.

**Decision:** study prepared-image identity, warm-pool accounting, metrics, and orphan cleanup; retain Glaeda's controller and Lima backend.

## Disposable VM backend boundary

The durable core should never infer ownership from a backend name or generic VM state. It hands the backend an exact typed identity and receives exact observations back.

A useful backend-independent contract is conceptually:

```text
observe_template(template_identity)
prepare_template(exact prepared-template generation)

observe_worker(attempt_worker_identity)
create_and_start(exact authorized attempt, template_identity, resources, network_policy_identity)
destroy(exact owned worker observation)
list_orphan_candidates(controller_identity)
```

Important semantics live above the backend:

- creation requires a durable capacity reservation and durable creation authorization;
- observations distinguish absence, exact owned stopped/ready state, unknown, and conflict;
- a stopped partial clone is cleanup debt only inside the exact authorized creation history;
- same-name discovery grants zero ownership;
- deletion requires exact owned observation;
- capacity release follows proven VM/runner cleanup;
- retry and recovery decisions belong to the controller.

### Lima/VZ — selected first backend

Lima supplies mature Apple VZ and guest lifecycle primitives, and M2 now has pinned template inputs, clone recovery, durable mutation checkpoints, same-lock execution, and ambiguous-outcome handling around it.

Plain mode also gives Glaeda a useful starting point for disabling host mounts, forwarding, built-in containerd, and guest-agent conveniences while retaining reviewed provisioning.

**Decision:** finish M2 and physical acceptance on Lima before benchmarking a replacement.

### Tart — first alternative benchmark

Tart is built for automated Apple VZ workloads and supports OCI-distributed VM images. Nearby runner projects demonstrate strong CI ergonomics around it. Current release 2.34.0 also passes Softnet's policy-control file descriptor through Tart, tightening their integration.

Its FSL-1.1-ALv2 license and Glaeda's accepted Lima recovery work make an immediate switch expensive. Tart becomes compelling when M4 enforcement or M6 measurements show a Lima limitation that changes the product outcome.

**Decision:** retain as the first serious alternative benchmark with an explicit license gate before becoming required.

### Lume

Lume v0.4.0 provides a higher-level Apple VZ surface for macOS and Linux guests. It is closer to the abstraction Glaeda wants than vfkit, while Lima and Tart currently have stronger CI lifecycle prior art.

**Decision:** benchmark after a measured Lima gap. Pin and re-review the exact Lume release/license before any switch.

### vfkit

vfkit is a useful low-level Virtualization.framework tool and permissively licensed. Choosing it would push image acquisition, provisioning, clone/snapshot conventions, guest control, and more recovery semantics into Glaeda.

**Decision:** keep it as a low-level fallback/research tool; mature higher-level backends are a better first choice.

## Guest JIT control channel

GitHub's supported JIT startup documentation passes encoded JIT configuration as a `--jitconfig` command-line argument to the runner startup path. Glaeda's product boundary keeps one-time JIT material out of argv, public logs, durable public state, and reusable guest storage.

That makes a tiny controller-owned handoff appropriate unless the exact pinned runner exposes an equally narrow secret-input mechanism during implementation.

The helper should do exactly this:

```text
receive one bounded JIT configuration over a controller-only private channel
-> validate framing/size and exact attempt/runner generation
-> start the exact pinned Runner.Listener as the locked workload account
-> remove run-private secret material after handoff
```

Required properties:

- fixed helper and runner executable identities;
- zero arbitrary shell, command, path, environment override, file transfer, or general remote execution;
- one attempt/runner generation per invocation;
- bounded input and output;
- controller-only transport, preferably the existing private Lima control path or a narrowly reviewed vsock path;
- secret absent from process-list-visible argv, inherited long-lived environment, durable Glaeda documents, and reusable guest state;
- helper version/identity bound into the prepared-template identity.

A general guest agent adds authority the product does not need.

## Hostile-CI network enforcement

The policy contract belongs in Glaeda even if enforcement changes backend later.

### Required policy intent

For a hostile disposable worker:

- deny unsolicited inbound traffic;
- deny the Mac host/controller;
- deny RFC1918/private and local LAN destinations;
- deny IPv4/IPv6 link-local destinations;
- deny cloud/container metadata destinations;
- deny peer workers;
- use explicit DNS without inheriting the Mac's local resolver state;
- allow ordinary public GitHub, package, source, browser, and build traffic according to policy;
- cover IPv4 and IPv6;
- fail closed when enforcement authority is unavailable;
- allow bounded project exceptions only through reviewed policy.

### Softnet source review

Softnet 0.22.1 is interesting because it provides per-Tart-VM userspace packet filtering, bounded stateful flow authorization, and an atomic local Unix-socket JSON-RPC policy API. Its documented policy supports default-deny IPv4 egress with specific allows.

Current gaps for Glaeda M4:

- Tart coupling;
- FSL-1.1-ALv2 product/distribution gate;
- elevated host authority during initialization before privilege drop;
- dynamic policy is documented as IPv4-focused;
- default behavior allows the vmnet gateway and incoming traffic, while Glaeda requires explicit inbound denial plus controller/LAN isolation;
- closing the policy control socket leaves the last accepted policy active, so controller-loss semantics need separate proof;
- M4 still needs a complete IPv6 policy and bypass test story.

**Decision:** keep Softnet as a source-reviewed reference and benchmark candidate. Continue looking for a Lima-compatible path that satisfies the same policy. If Lima cannot meet the contract cleanly, benchmark Tart+Softnet as one combined backend option and make the licensing/distribution decision explicitly.

## Cache and acceleration boundary

M6 should begin with mature mechanisms:

1. GitHub Actions cache/artifacts for portable cross-job state;
2. `sccache` for compiler output where producer/consumer trust can be separated;
3. BuildKit's GitHub Actions cache backend for container layers;
4. prepared-template generations for reviewed static toolchains and dependencies.

Any later host-local writable cache needs an explicit namespace, quota, producer authority, consumer authority, generation identity, and discard path. Cache existence carries zero source or verification authority by itself.

## Attractive alternatives deliberately declined

### GARM as outer controller

It reduces custom GitHub code but creates split durable lifecycle ownership at Glaeda's most important crash boundaries. Keep the direct official client.

### Official convenience listener unchanged

Its handler/ack ordering does not provide Glaeda's persistence-before-ack boundary. The checked-in bridge already provides explicit acknowledgement after durable reconciliation.

### Immediate Tart switch

Current M2 Lima work closes difficult clone/recovery/ownership seams. Switch from measured evidence or an M4 capability gap, with the FSL distribution decision first.

### Direct vfkit integration

That trades one dependency for a much larger custom image/provisioning/guest-lifecycle surface. Glaeda should stay above mature VM lifecycle tools.

### General guest agent

M3 needs one secret-bearing runner startup handoff and bounded observations. A general command channel adds broader execution authority without product value.

### Custom generic firewall

Glaeda should define network intent and bind enforcement evidence. Packet filtering belongs to a mature backend when one satisfies the policy.

## Benchmark and acceptance probes before dependency switches

### VM backend probes

Run the same pinned ARM64 guest/template intent through Lima, Tart, and any serious Lume candidate:

- prepared-template build time;
- clone/create latency;
- boot-to-controller-ready latency;
- destroy latency;
- disk amplification per worker;
- idle host footprint;
- resource-limit fidelity;
- exact orphan discovery after controller kill/reboot;
- controller kill at every create/start/destroy checkpoint;
- same-name foreign VM protection;
- cleanup proof before capacity release.

vfkit needs separate accounting for the image/provisioning lifecycle it would force Glaeda to own.

### JIT secrecy probes

For one generated JIT configuration, prove the secret is absent from:

- host and guest process listings/argv;
- inherited environment visible to unrelated guest processes;
- Glaeda human/JSON output;
- durable attempt/template documents and journals;
- controller/bridge diagnostics;
- reusable guest files after the attempt;
- host temporary files and crash-recovery debris.

Kill the controller/helper/runner at each handoff point and require a bounded recovery result without silently reusing one-time JIT state.

### Network probes

From a hostile guest fixture verify:

- public GitHub/package/DNS access succeeds according to policy;
- Mac host/controller access fails;
- RFC1918/private LAN access fails;
- IPv4 and IPv6 link-local access fails;
- metadata endpoints fail;
- peer-worker access fails;
- unsolicited inbound access fails;
- IPv6 cannot bypass IPv4 policy;
- enforcement restart/failure produces the accepted fail-closed behavior;
- nested rootless containers cannot bypass worker policy.

### Licensing/distribution gate

Before Tart or Softnet becomes required, record the exact covered release, license text, future-license date semantics, binary redistribution plan, and product-use compatibility. They may remain optional benchmark dependencies while that decision is open.

## Upgrade policy

Every required external component should be pinned to an exact reviewed release/commit/digest and upgraded as an explicit generation change:

- `actions/scaleset` bridge source pin;
- Lima binary version;
- Ubuntu guest image digest;
- official Actions runner archive digest;
- prepared-template identity;
- future guest helper identity;
- future network-enforcement backend identity.

An upstream upgrade changes the reviewed dependency identity and triggers the appropriate integration/physical acceptance lane. Moving tags, moving guest images, ambient package-manager resolution, and opportunistic backend fallback never silently widen production authority.

## Sources inspected

Primary upstream sources inspected for this decision:

- GitHub `actions/scaleset`: <https://github.com/actions/scaleset/commit/cb0405b2d874500e75ae34eff8d582ab75956b45>
- GitHub JIT runner documentation: <https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/run-jit-config>
- GARM: <https://github.com/cloudbase/garm> and <https://github.com/cloudbase/garm/releases/tag/v0.2.1>
- GHA Outrunner: <https://github.com/NetwindHQ/gha-outrunner>
- Graftery: <https://github.com/diranged/graftery>
- Lima: <https://github.com/lima-vm/lima/releases/tag/v2.2.0> and <https://lima-vm.io/docs/config/plain/>
- Tart: <https://github.com/openai/tart/releases/tag/2.34.0> and <https://github.com/openai/tart/blob/main/LICENSE>
- Softnet: <https://github.com/openai/softnet/releases/tag/0.22.1> and <https://github.com/openai/softnet/blob/main/LICENSE>
- Lume/Cua: <https://github.com/trycua/cua/releases/tag/lume-v0.4.0>
- vfkit: <https://github.com/crc-org/vfkit/releases/tag/v0.6.4>

## Completion signal for #368

This decision satisfies the research gate when accepted:

- direct `actions/scaleset` is selected over GARM/Outrunner/Graftery as outer controller while preserving useful interface lessons from each;
- Lima/VZ remains the first backend behind a narrow worker adapter;
- JIT handoff is narrowed to an exec-only secret-bearing helper when required by the runner's supported interface;
- M4 network policy intent is fixed independently of enforcement, with Softnet documented as a candidate plus explicit gaps;
- source/version/license implications are recorded;
- benchmark gates are defined before any backend switch.
