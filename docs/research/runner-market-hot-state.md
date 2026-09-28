# CI runner market and hot-state techniques: strategy notes for Glaeda

Researched 2026-09-23. Prices are list prices found that day; verify before quoting externally.

## TL;DR

- Hosted macOS CI has converged on one product: an ephemeral Apple Silicon VM at about $0.08/min, plus some kind of cache. Nobody sells hot memory state or per-PR warm incremental DerivedData on macOS.
- The trust pattern for hot disks is already standard on Linux: fork a snapshot per job, let everyone read, only default-branch jobs commit (Blacksmith sticky disks, Namespace cache volumes). Glaeda should copy it, not reinvent it.
- The Xcode compilation-cache service (gRPC CAS plugin) is now a crowded feature (Tuist, Bitrise, Namespace). RFC #1134 is table stakes, not a moat. Its value for Glaeda is as a component under hot state.
- Virtualization.framework can save and restore a running macOS guest (macOS 14+, Tart already exposes it), but save files are hardware-encrypted and bound to the host Mac and user. Hot memory templates must be built per machine, never shipped across the fleet.
- The Apple license caps you at 2 macOS VMs per Mac and restricts service-bureau use; leasing to third parties needs 24 hour minimum exclusive leases. That shapes any "offer a Blacksmith" plan more than technology does.

## 1. Competitors

| Provider | macOS hardware | Isolation | Hot-state mechanism on macOS | Cache offering | macOS price | Notable |
| --- | --- | --- | --- | --- | --- | --- |
| Blacksmith | M4, 6 vCPU/24 GB or 12 vCPU/48 GB | Not documented | Sticky disks documented for Linux only | Actions cache, Docker layer cache (Linux) | $0.08/min | Sticky disk = Ceph ext4 block device, clone on start, commit on finish, default-branch-only commits |
| Depot | M2, 8 CPU/24 GB (beta) | Not documented | None documented | Depot Cache (remote cache for Bazel/Gradle/Turbo etc.), fast Actions cache | $0.08/min, per-second | macOS only on Startup plan and above |
| Namespace | M4 Pro and M5 Max, up to 16 vCPU/56 GB | Not documented | Cache volumes on macOS (forked per job, NVMe, fleet locality grows with use) | Xcode 26 compilation cache on cache volumes, SwiftPM, CocoaPods | $0.04 to $0.20/min prepaid by shape | Only provider with macOS persistent volumes plus explicit Xcode compilation cache docs |
| WarpBuild | M4 Pro, 6 vCPU/14 GB, 12 vCPU/28 GB | Ephemeral VM | None (snapshots are Linux only) | actions/cache on macOS | $0.08 and $0.16/min | States plainly that snapshot runners are Linux only |
| Cirrus Runners (Tart) | M4 Pro | Tart VMs | None | Standard Actions cache | $150/month per concurrent runner, unlimited minutes | Cirrus Labs joined OpenAI (Apr 2026); not onboarding new customers; Cirrus CI shut down Jun 1 2026 |
| MacStadium Orka | Customer-dedicated Macs | VMs (Orka) | Image caching: locally cached images launch in under 1 s, thin images boot in 1 to 2 min | None built in | Per-node monthly | Sells the orchestration, not the CI |
| GitHub hosted | M1 3/4-core, M2 Pro 5-core (xlarge) | VM | None | actions/cache (tarballs) | $0.062, $0.077, $0.102/min | 2026 price cut; the baseline everyone quotes against |
| Buildkite hosted | M4, 6x28 and 12x56 | Virtualization.framework VM per job | Cache volumes (NVMe, included), git mirrors | Via cache volumes | $0.02/vCPU-min ($0.12 to $0.24/min) | Cache storage included at no charge |
| Bitrise | M2 Pro, M4 Pro (54 GB) | VM | None | Build Cache for Xcode (compilation cache, also usable from GitHub Actions), Gradle, Bazel | Credit/build packages | Claims 10+ min archives down to 2 to 5 min with CAS |
| Codemagic | M4 | VM | None | Basic | $0.114/min PAYG, M4 plan from $5,400/yr | Mobile-first |
| Tuist | Moving to owned bare-metal Macs (Sep 2026) | TBD | Colocated cache and compute planned | Remote Xcode compilation cache (gRPC proxy), module binary cache, chunk reuse | Cache free during iteration | Now vertically integrating into Mac hardware; closest strategic competitor |
| RWX (Mint) | No macOS found | Linux containers | Content-addressed task cache | Automatic content-based task caching | n/a | DAG with per-task cache keys; interesting model, not a macOS player |
| Anka (Veertu) | Customer Macs | VMs | Suspended VM templates, "instant start" in a few seconds, delta image registry | None | Licence | Only vendor productising macOS memory-state templates |

Sources: [Blacksmith pricing](https://www.blacksmith.sh/pricing), [Blacksmith instance types](https://docs.blacksmith.sh/blacksmith-runners/overview), [Blacksmith sticky disks](https://docs.blacksmith.sh/blacksmith-caching/dependencies-sticky-disks); [Depot macOS launch](https://depot.dev/blog/mac-github-actions-runners), [Depot runners](https://depot.dev/docs/github-actions/overview); [Namespace macOS](https://namespace.so/docs/architecture/compute/macos), [Namespace cache volumes](https://namespace.so/docs/architecture/storage/cache-volumes), [Namespace caching](https://namespace.so/docs/solutions/github-actions/caching), [Namespace pricing](https://namespace.so/pricing); [WarpBuild macOS](https://www.warpbuild.com/runners/macos); [Cirrus Runners pricing](https://cirrus-runners.app/pricing/), [Cirrus Labs joins OpenAI](https://cirruslabs.org/); [Orka](https://macstadium.com/orka), [Orka on AWS](https://macstadium.com/orka-on-aws); [GitHub runner pricing](https://docs.github.com/en/enterprise-cloud@latest/billing/reference/actions-runner-pricing); [Buildkite macOS](https://buildkite.com/docs/agent/buildkite-hosted/macos), [Buildkite pricing](https://buildkite.com/pricing/); [Bitrise Build Cache](https://bitrise.io/platform/build-cache), [Bitrise compilation cache FAQ](https://docs.bitrise.io/en/bitrise-build-cache/build-cache-for-xcode/xcode-compilation-cache-faq), [Bitrise roadmap](https://roadmap.bitrise.io/c/196-xcode-26-compilation-cache-support-30-faster-xcode-builds); [Codemagic pricing](https://docs.codemagic.io/billing/pricing/); [Tuist Xcode cache](https://tuist.dev/blog/2025/10/22/xcode-cache), [The new Tuist](https://tuist.dev/blog/2026/09/12/the-new-tuist); [RWX caching](https://www.rwx.com/docs/caching); [Anka](https://docs.veertu.com/anka/what-is-anka/), [Anka registry](https://veertu.com/anka-registry-version-distribute-macos-vms/).

Observations:

- **The typical range is $0.06 to $0.08/min** for a 6 to 8 core Mac VM. Unit-priced offers below that exist (Namespace from about $0.04, Buildkite per vCPU-minute), but the differentiators are queue depth, cache, and hardware generation.
- **Hot state is a Linux feature everywhere except Namespace and Buildkite.** WarpBuild says so outright. Blacksmith documents sticky disks only with Ubuntu examples.
- **The cache winners on macOS are colocated.** Tuist's own benchmarks show remote cache gains shrink versus local (Wikipedia 24% local vs 18% remote; Pocket Casts 36% vs 20%). Separately, #1134 found that the client sends large blobs as file paths on its own disk, so a store on another host cannot receive them; that, not distance, is why the node daemon is required.
- **Queue depth is the real product for cmux.** The 1 to 3 hour wait is capacity, not speed. Every provider above sells burst capacity from a shared pool; the minis are dedicated capacity.

## 2. macOS virtualization constraints

**Two VMs per Mac, and no time-sharing.** The macOS Tahoe licence grants "up to two (2) additional copies or instances" in VMs per Mac you own or control, for development, testing, macOS Server, or personal use, and excludes use "in connection with service bureau, time-sharing" and similar services except as Section 3 permits ([macOS Tahoe SLA](https://www.apple.com/legal/sla/docs/macOSTahoe.pdf)). Section 3 allows leasing for developer services only if each lease is at least 24 consecutive hours and the lessee has sole and exclusive use of the Mac. The framework enforces the VM count: a third macOS guest fails ([Eclectic Light](https://eclecticlight.co/2022/08/04/virtualisation-on-apple-silicon-macs-8-how-apple-limits-vms/)). For cmux running its own CI on its own minis this is fine. For selling runners to other orgs it is the central business constraint (not legal advice; get counsel before pricing a service).

**Virtualization.framework save/restore.** Since macOS 14, `saveMachineStateTo` and `restoreMachineStateFrom` pause and serialise a running macOS or Linux guest. Restore requires a VM built from the same configuration. Save files are hardware encrypted and "no other Mac or user account" can restore them; disk images are not included and must be managed separately ([WWDC23 10007](https://developer.apple.com/videos/play/wwdc2023/10007/), [API docs](https://developer.apple.com/documentation/virtualization/vzvirtualmachine/savemachinestateto(url:completionhandler:))). Apple publishes no restore latency. Which config differences are tolerated is undocumented; one open project is still empirically mapping it ([kernova#1318](https://github.com/nicholas-lonsinger/kernova/issues/1318)).

**Tart.** Tart implements this today: `tart run --suspendable` disables audio and entropy devices and restricts input devices, and `tart suspend` signals the runner to call `saveMachineStateTo`; the next `tart run` restores. It only supports macOS guests for suspend (Linux suspend is open issues #1177, #1297) ([openai/tart Run.swift](https://github.com/openai/tart/blob/main/Sources/tart/Commands/Run.swift)). Licence history: AGPL at launch, changed to a paid source-available Fair Source licence in 2023 ([Changing Tart License](https://tart.run/blog/2023/02/11/changing-tart-license/)), enforced in 2025 ([press release](https://tart.run/blog/2025/10/27/press-release-cirrus-labs-successfully-enforces-its-fair-source-license/)). After the OpenAI deal Cirrus promised a more permissive licence and no fees; the repo now lives at openai/tart under FSL-1.1-ALv2 (converts to Apache 2.0 after two years), copyright OpenAI. Typical clone boot is around 15 s, sometimes 45 s+ ([openai/tart#903](https://github.com/openai/tart/issues/903)).

**Anka** already sells suspended templates ("instant start" in a few seconds) and a registry that moves only image deltas ([Anka docs](https://docs.veertu.com/anka/what-is-anka/)). So the idea is proven commercially; what nobody does is combine it with a project-warm build state and a trust model.

**APFS clones.** `clonefile` gives constant-time copy-on-write copies of multi-GB disk images; any block write, even writing identical bytes, diverges that block ([Wade Tregaskis](https://wadetregaskis.com/copy-on-write-on-apfs/)). Tart clones use this. This is the macOS equivalent of the Ceph or ZFS clone under a sticky disk, but only on one host.

## 3. Hot-state snapshot tech elsewhere

- **Firecracker**: lazy memory restore through userfaultfd gets the restore step to tens of ms (one vendor reports ~49 ms restore, p50 179 ms end to end) ([PandaStack](https://www.pandastack.ai/blog/firecracker-memory-snapshots/)). Firecracker's own docs warn that restoring one snapshot more than once duplicates "unique identifiers, random numbers, and cryptographic tokens", and recommend VMGenID reseeding, which only covers the kernel entropy pool ([snapshot-support.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)).
- **CodeSandbox** forks a running VM in about 1.5 s regardless of memory size by sharing memory copy-on-write via userfaultfd; running dev servers survive the fork ([CodeSandbox](https://codesandbox.io/blog/how-we-clone-a-running-vm-in-2-seconds), [uffd cloning](https://codesandbox.io/blog/cloning-microvms-using-userfaultfd)).
- **Modal** snapshots a function just before it takes input; the win is skipping library init and JIT, not loading data ([Modal memory snapshots](https://modal.com/blog/mem-snapshots)). **Fly** suspend/resume requires 2 GB or less of memory and discards snapshots on deploy ([Fly docs](https://fly.io/docs/reference/suspend-resume/)).
- **CRIU** does process-level checkpoint on Linux only ([CRIU](https://criu.org/Main_Page)). No macOS equivalent; the VM is the checkpoint unit on macOS.
- **Sticky disks and cache volumes**: every job gets a private clone of the last committed snapshot; only successful (Namespace) or default-branch (Blacksmith, opt-in Namespace tag for forks) jobs commit; last write wins; Namespace may serve a slightly stale version to keep startup fast.
- **Bazel remote persistent workers**: keeping compilers warm across remote actions measured about 2x on large builds ([proposal](https://github.com/bazelbuild/proposals/blob/main/designs/2021-03-06-remote-persistent-workers.md)). Same lesson as Modal: the process warm-up is a large share of cost for JIT or daemon-heavy toolchains.

The pattern: memory snapshots pay off when initialisation dominates (JVMs, interpreters, indexers, booted simulators), and disk snapshots pay off when incremental state dominates. Xcode CI has both: SourceKit/indexing, simulator boot, and `xcodebuild` package resolution on one side, DerivedData on the other.

## 4. Where the gap is

1. **Hot incremental DerivedData with isolation, on macOS.** Namespace and Buildkite give persistent volumes, but a volume only restores files. Nobody advertises incremental DerivedData that is at the exact current `main` commit per PR. The cmux measurement (36 s warm slot vs 953 s cold) is a 26x gap no hosted provider claims.
2. **Compilation cache cannot fix whole-module invalidation.** cmux#13108 and #13514 show one edit inside the app target changes the cache identity of essentially the whole `cmux` module. A remote CAS makes valid hits cheap; it cannot create hits for invalidated actions. Swift's own incremental build inside warm DerivedData can. So the compilation cache (#1134) and warm slots solve different halves: cross-machine reuse of unchanged modules, versus per-file incrementality inside the changed module.
3. **Memory-hot macOS runners.** Only Anka productises suspended templates, and without project state or a trust model. Hosted providers all boot or restore disk-only VMs.
4. **Trust-tiered macOS.** Everyone has either "shared cache everyone writes" or "default branch writes". Nobody exposes explicit tiers (hostile, trusted, ultra-trusted) with different hot-state entitlements.
5. **Dedicated capacity without queue.** For an org that already owns Macs, the market offers orchestration (Orka, Anka, Tart/Orchard) but not a runner that is fast and trust-aware on your own hardware.

What is not a gap: a remote Xcode compilation cache service (Tuist, Bitrise, Namespace all ship one), cheap per-minute Mac VMs, or Docker layer caching.

## 5. Implications for Glaeda (ranked)

**1. Measure the ultra-hot path on one mini before anything else.** Concretely, on an M4 Pro 48 GB:
- VZ save and restore wall time and save-file size for a macOS 26/27 guest with 16 and 24 GB RAM, right after `xcodebuild` has built cmux (no Xcode GUI) (Tart `--suspendable` is enough to test).
- Whether one save file can be restored repeatedly against successive APFS clones of the same disk (Apple docs neither promise nor forbid this; Anka's instant start suggests it works).
- PR build time in the restored VM vs the 36 s host warm slot vs 953 s cold, to price the VM tax.
- How often a PR's checkout from the template's `main` still builds incrementally (large rebases may fall back near cold).
Decision rule: if restore plus checkout plus incremental build is under ~90 s p50, build the design below; if restore is slow or brittle, fall back to disk-only hot clones (step 3 without memory).

**2. Copy the sticky-disk trust rule exactly.** Every job forks the last trusted generation; only trusted builders (default-branch push, schedule, maintainers) commit a new generation; untrusted clones are always discarded. This is industry standard (Blacksmith, Namespace) and maps directly onto Glaeda's "reviewed reusable generations". Key consequence: hostile fork PRs can safely *read* hot state. Isolation and speed conflict only on writes. Do not invent a new model here.

**3. Proposed "ultra-hot but safe" design (per mini, 2 VM slots).**
- *Generation build (trusted):* on each `main` merge, a trusted slot restores the previous template, fetches `main`, builds (fills DerivedData, resolves SwiftPM, optionally boots a simulator), writes to the fleet compilation cache (#1134), then pauses and `saveMachineStateTo`. The disk image plus state file becomes generation N, published only on this host. Record a receipt (commit, Xcode build, image digest, config hash).
- *Job (any tier):* `clonefile` the generation's disk and state file, restore, inject per-job secrets after restore over vsock or SSH (GitHub JIT runner registration, never baked in), check out the PR, build, test, destroy. Untrusted jobs get the node-local cache in read-only mode.
- *Uniqueness hygiene:* templates contain no credentials, runner tokens, or SSH host keys that matter; reseed or restart anything identity-bearing post-restore; never run the template's source VM after snapshotting. Firecracker's warning about duplicated entropy applies; Tart's suspendable mode already removes the virtio entropy device, so measure what macOS does for randomness after restore.
- *Locality:* because save files are bound to one Mac and user, each mini builds its own generation (cost: one incremental build per merge per mini, cheap when warm). The fleet shares only the compilation cache and base OCI image, not memory state. Treat this as a hard constraint, not an optimisation to remove later.
- *Ultra-trusted tier:* a resident VM or bare-metal slot that never resets (the current warm slot), for maintainers and main. The fastest path, and the only tier allowed to keep state across jobs.

**4. Ship #1134 as a component, not the pitch.** It is necessary (fresh or new machines near warm; 30 s vs 118 s) and makes generation builds cheap, but Tuist, Bitrise and Namespace sell the same protocol. Keep the node daemon mandatory and the store on the LAN; Tuist's own numbers show remote-only keeps less of the gain (24% local vs 18% remote on one project, 36% vs 20% on another).

**5. Differentiate on "hot at main, per PR, on hardware you own", not per-minute price.** The pitch that no one else can make today: a PR build starts from a memory-hot VM that is already at the latest `main` with warm DerivedData, isolated, discarded afterwards, with an auditable trust receipt for every generation. For cmux the immediate win is simply removing the 1 to 3 hour Blacksmith queue by moving macOS jobs to the minis.

**6. Avoid selling multi-tenant macOS minutes until the licence question is answered.** The 2-VM cap and the 24-hour exclusive lease clause fit "operator-controlled compute" (customers run Glaeda on their own Macs, or lease whole Macs by the day) far better than a Blacksmith-style shared per-minute pool. Position the product as software for owned or leased-dedicated fleets first.

**7. Avoid tarball caches and whole-directory restores.** cmux's current nightly single-writer 5 GiB CAS tarball is the pattern every fast provider moved away from (block-level clones, colocated CAS). Also avoid depending on Tart's licence staying permissive: it is FSL today; keep the VM layer behind a Glaeda interface so Virtualization.framework can be driven directly if needed.

**8. Instrument queue time and hit rates from day one.** Track, per job: queue wait, restore time, checkout-to-first-compile, compilation cache hit/miss, incremental vs full compile count, and generation age (commits behind `main`). The Blacksmith comparison that matters to cmux is end-to-end wall time including queue, and generation age is the number that predicts whether hot state helps.
