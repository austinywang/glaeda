# Blazingly hot execution

Glaeda should make useful compute begin with as little repeated setup as possible.

> **Disposable is a capability. Trust decides residency.**

This document is the current high-level map for hot execution. The general product boundary lives in [`COMPUTE_RUNTIME.md`](COMPUTE_RUNTIME.md). The detailed resident project/runtime continuation lives in [`M6_HOT_RUNTIME_HANDOFF.md`](M6_HOT_RUNTIME_HANDOFF.md). Current code and accepted issue/PR decisions win over either document when they move ahead.

## Product objective

The universal objective is **time from declared work to a useful accepted result**, together with throughput and resource cost under exact trust and correctness constraints.

Useful common metrics include:

- request-to-first-useful-compute;
- request-to-first-useful-output;
- request-to-final-accepted-result;
- workload completion wall time;
- fleet throughput under concurrency;
- CPU/RAM/storage/network/accelerator occupancy where relevant;
- hot-state hit/miss/reset benefit;
- recovery time after interruption.

Coding agents, GitHub Actions, edit/test/build loops, and repository verification are major current proving workloads. They add important workload-specific metrics such as edit-to-first-test, edit-to-build, verification completion, and multi-agent fan-out. Those workloads do not define the outer product boundary.

## Current execution model

Glaeda tries to do every reusable, trustworthy piece of preparation before the workload needs it.

```text
work becomes known
-> identify exact workload, trust, inputs, and capabilities
-> select eligible compute and the hottest valid reusable state
-> admit capacity
-> materialize only what must change
-> execute
-> return useful outputs / evidence
-> retain, reset, migrate, quarantine, or destroy state
-> recover from exact durable truth after interruption
```

For hostile or unknown work, the quickest safe path may still be a fresh worker. For trusted repeated work, the quickest path may be a prepared environment or warm pool. For ultra-trusted work, the winning optimization is often to keep valuable state resident.

## Native handoff principle

Glaeda should be a thin semantic compiler into the operating system wherever mature primitives
already provide the physical mechanism:

```text
exact workload intent + trust + reusable-state identity
-> choose the cheapest trustworthy native mechanism
-> hand runnable work to the operating system
-> observe the effective kernel/filesystem state
-> return a bounded durable receipt
```

The ordinary Linux scheduler, page cache, cgroups, process descriptors, filesystems, mount API,
reflinks and service manager are the baseline implementation toolkit. Glaeda adds the semantic
identity, authority boundary, composition, validation, recovery and evidence those mechanisms do
not provide by themselves. It should not remain in the workload hot path merely to duplicate an
operating-system decision.

The default comparison is therefore native, not bespoke. A custom placement, cache, filesystem or
lifecycle policy earns promotion only when a complete-loop experiment shows that native behavior
leaves a material gap and the candidate preserves stronger semantics. If the native control wins,
keep it.

Big Red's bounded CPU concurrency controls illustrate the rule. At four simultaneous warm
verification loops, ordinary unpinned Linux scheduling delivered 1.0882 loops/second, external
`taskset` 1.0496 and Glaeda's opt-in crash-clean CPU-set prototype 1.0325. The explicit set still
had value for reproducible, collision-free benchmark controls and reduced maximum worker RSS from
42,278 to 39,246 KiB, but it did not justify a general scheduler. Ordinary work remains unpinned;
reservations come and go only for experiments that require disjoint placement. Exact evidence and
candidate scope are tracked in [#953](https://github.com/teamleaderleo/glaeda/issues/953) and
[#955](https://github.com/teamleaderleo/glaeda/pull/955).

This same rule explains the current storage direction: native OverlayFS for mostly-read task views,
native CoW/reflink lineage for write-heavy private state where it wins, ordinary private state when
sharing has no measured value, and native page-cache residency instead of a Glaeda byte cache.

## Current experiment owners

Keep the implementation lanes separate enough that measurements remain interpretable:

- [#563](https://github.com/teamleaderleo/smolrunner/issues/563) — bounded comparable hot-execution performance receipts;
- [#560](https://github.com/teamleaderleo/smolrunner/issues/560) — resident Linux project storage and project-disk layout;
- [#562](https://github.com/teamleaderleo/smolrunner/issues/562) — cheap task/workspace materialization;
- [#570](https://github.com/teamleaderleo/smolrunner/issues/570) — trusted OverlayFS task views and source anchors;
- [#580](https://github.com/teamleaderleo/smolrunner/issues/580) — task-private Git metadata, inherited source index, OverlayFS source view, and final Git/source readiness proof;
- [#573](https://github.com/teamleaderleo/smolrunner/issues/573) — path-class-specific hot-state mechanism selection;
- [#565](https://github.com/teamleaderleo/smolrunner/issues/565) — crash-safe single-writer project-disk lifecycle;
- [#588](https://github.com/teamleaderleo/smolrunner/issues/588) — exact one-shot Mac-to-Linux guest control transactions;
- [#561](https://github.com/teamleaderleo/smolrunner/issues/561) — resident backend comparison;
- [#770](https://github.com/teamleaderleo/smolrunner/issues/770) — general compute-workload boundary and generic workload seam.

## The leading resident Linux path

For the current development/CI workload family, the leading trusted path is already more specific than “try a faster filesystem.”

```text
macOS durable control plane
        |
        v
resident Linux sandbox
        |
single-writer persistent project filesystem
        |
        +-----------------------------------+
        |                                   |
immutable / mostly-read state              write-heavy mutable state
        |                                   |
clean source anchor                        private CoW / reflink lineage
immutable Git object pool                  compiler/build output
private task Git metadata                  other mutation-heavy state
inherited reviewed source index            |
        |                                   |
        +---------- task execution --------+
                     |
              OverlayFS source view
                     |
                 useful work
```

This is not a custom general-purpose filesystem. Glaeda composes mature Linux filesystem primitives according to the mutation behavior of each state family.

### Mostly-read source state: OverlayFS

OverlayFS is the current leading task-view mechanism for clean resident source fan-out, not a hypothetical later comparator.

The accepted model is roughly:

```text
exact immutable source anchor
+ exact immutable Git object-pool generation
+ task-private Git directory
+ inherited reviewed clean source index
+ unique task upper/work directories
-> exact OverlayFS mount
-> hardened non-mutating Git/source proof
-> task ready
```

The lower source remains immutable for the lifetime of child task leases. Task edits and whiteouts live in the private upper layer. Git refs/config/index/new objects are task-private rather than shared writable metadata.

Owned hosted-ARM evidence recorded in #570/#580 found, for one representative source-view comparison:

```text
tasks    ordinary materialization    OverlayFS + inherited index    ready-state growth
1        ~76.2 ms                    ~15.9 ms                       ~8.05 MiB -> ~4 KiB
8        ~224.3 ms                   ~52.4 ms                       ~64.3 MiB -> ~32 KiB
32       ~695.6 ms                   ~175.2 ms                      ~257.3 MiB -> ~160 KiB
```

All compared views passed the non-mutating Git proof. The source-view model also worked on ext4, so XFS is not a prerequisite for OverlayFS source fan-out.

### Git metadata: private, objects shared immutably

The preferred task path does not rely on one shared writable common Git directory.

A resident immutable object-pool generation supplies base objects while each task gets private refs, config, index, HEAD, and new objects. The current V1 direction uses an explicit reviewed equivalent of:

```text
git clone
  --reference <exact-immutable-pool-generation>
  --no-local
  --no-checkout
  --separate-git-dir <exact-private-task-gitdir>
  --template <reviewed-empty-template>
  <exact-immutable-pool-generation>
  <exact-task-target>
```

The active pool remains frozen and task-unwritable. New/updated pool state is published as a successor generation instead of mutating the active generation in place.

### Write-heavy state: private CoW/reflink lineage

OverlayFS is not the universal answer.

Putting warmed compiler output behind a shared lower can create expensive copy-up. Current measurements showed that clearly: one synthetic eight-task Rust comparison was roughly **5.70 s** with the write-heavy target state behind OverlayFS versus roughly **2.77 s** with task-private reflink/CoW build-state lineage.

The resulting policy direction is path-class-specific:

```text
immutable / mostly-read source and suitable dependency state
-> immutable resident base + OverlayFS task view

write-heavy compiler/build state
-> task-private CoW/reflink lineage where supported

state with no worthwhile reusable parent
-> private empty state

shared mutable cache
-> only under an explicit producer/consumer authority model
```

Do not promote a filesystem primitive because its microbenchmark is attractive. Promote it for the state family where complete useful-result measurements show it wins.

### Ultra-trusted local hot-run prototype

For native SwiftPM, Xcode, and Apple app build helpers, use the
[native Apple build workflow](APPLE_NATIVE_BUILDS.md). It preserves one checkout
and gives native cache families stable, toolchain-keyed paths with project locking
and explicit interrupted-build recovery. Linux filesystem views below remain a
separate backend.

`scripts/hot-run` is a deliberately small Linux developer-loop prototype. It binds a task Git
worktree onto the pathname of a warmed resident worktree. Explicit path-class policies can expose
resident dependencies read-only, give a short-lived task a private OverlayFS upper over warmed
bytes, or retain a task-private directory at the same in-sandbox pathname across repeated edit
loops:

```bash
/path/to/resident/scripts/hot-run \
  --task /path/to/task-worktree \
  --cache target:private-copy \
  -- cargo test --locked --lib --bins
```

The stable-path bind would otherwise hide a linked worktree's common `.git` directory. For this
ultra-trusted prototype, `hot-run` re-exposes that exact common directory through a private
per-invocation mount location and places a read-only pointer over the rebound root's `.git` file.
This makes ordinary Git-aware build and verification commands work without rewriting the task
worktree. It is still linked-common metadata, not the product-grade task-private Git mechanism
owned by #580, and grants no isolation from other trusted users of the same repository.

`native` is an explicit observation-only declaration for a direct same-worktree command: it records
that the named cache is mutated at its ordinary task path, creates no state, and adds no isolation.
It refuses cross-worktree execution so it cannot silently turn a private-cache request into shared
resident mutation.

An unmeasured invocation with explicit resident and task arguments, at most one native-cache
declaration, and no other option first enters a small POSIX front door. It resolves both arguments
to the same physical directory, uses one Git probe to require that directory to be the worktree
root, resolves one absolute executable, changes to that root and waits for the command with the
caller's terminal and environment. Measurement, runtime binding, timeout, resource profile, source
seeding, state, Git environment overrides, cross-worktree execution and every non-native or
ambiguous cache request fall through to the unchanged Python implementation. This is a latency
optimization for work that was already direct; it grants no project identity, residency, cache,
observation, isolation, or result authority.

The Python fallback itself uses a tiny executable launcher and an importable implementation
module. Ordinary CPython bytecode caching therefore pays source compilation once per changed
implementation rather than reparsing the complete hot-run implementation on every command. The
standard cache is ignored by Git, is invalidated by Python's source/cache-tag contract, and grants
no state, execution, or result authority; deleting it merely makes the next fallback invocation
compile the module again.

`glaeda-hot-run` is the compiled Linux and macOS front door for the equally common measured direct case:

```bash
glaeda-hot-run \
  --resident /path/to/resident --task /path/to/resident \
  --cache target:native --measurement /private/receipt.json \
  -- cargo check --locked
```

On macOS, it executes directly inside the existing project checkout, retains native build caches
between invocations, and records schema-v6 wall time plus the command's exit or signal. For a Mac
project such as cmux, keep one canonical checkout and invoke the normal native build tool there:

```bash
glaeda-hot-run \
  --resident /path/to/cmux --task /path/to/cmux/cmux-tui \
  --cache cmux-tui/target:native --measurement /private/cmux-warm.json \
  -- cargo test --locked -p cmux-tui-core --lib
```

The first invocation warms the tool's ordinary cache; repeated invocations reuse whatever the
tool validates. Cargo, SwiftPM, Xcode or Zig still owns dependency and source invalidation. A
cache miss follows the native build tool's cold rebuild path. No result is skipped based on a
Glaeda receipt. This is ultra-trusted host execution with the caller's environment and terminal,
not a sandbox or a managed resident lease. It creates no duplicate checkout or background daemon.
Use the project's existing tagged build helper for Xcode builds.

The Mac path rejects `--timeout`, `--resource-profile`, and `--cpu-set` before running anything.
CPU/RSS accounting and native-target snapshots are unavailable (`null`); the Linux `/proc`
machine probes are unavailable on macOS too. Wall time remains measured. Cross-worktree and
private-cache requests remain unsupported rather than falling back to shared mutable state.

On Linux, it preserves the schema-v6 measurement shape, workload-scoped GNU-time CPU/RSS boundary, aggregate
machine-pressure envelope, exit-versus-signal distinction, atomic receipt publication, caller
environment and terminal, optional comparison key, runtime executable digest, and optional
descendant-bin binding without starting Python. The binary accepts only a task directory inside the
named physical Git worktree and explicit `native` cache declarations. Its `--runtime-id`,
`--runtime-sha256`, and `--runtime-bin` options preserve the contracts described below, including
PATH-first descendant selection and preflight directory revalidation. On Linux, `--timeout`
places the command and ordinary descendants that remain in its private process group there, observes
the leader through a pidfd, and forwards operator SIGINT or SIGTERM to the group. A wall-clock
deadline sends SIGTERM, allows a two-second exit grace, continues observing the group if its leader
exits first, then escalates any remaining members to SIGKILL; the receipt records exit 124 and
`deadline_exceeded`.
`--resource-profile big-red-heavy` places GNU time and the command inside the existing collected
user-systemd scope with the exact 1,200% CPU, 8/12 GiB memory and 1,024-task limits described below.
The profile and deadline compose without changing either contract. The binary does not yet
implement a cross-worktree stable-path view, state preparation, or source-mtime seeding; those
requests remain on `scripts/hot-run` rather than being silently weakened. This is still an
observation-only ultra-trusted execution path. It grants no lease, cache, residency, validation,
publication, or cleanup authority.

On Linux, when the compiled front door records an exact `target:native` declaration, its schema-v6 receipt
also carries a path-private `native_target_observation`. Sequential pre-command observations bind
the checkout commit/tree/materialization and checkout-local Cargo target generation; an
unavailable pre-command observation refuses to run an unbound measurement. After the command,
checkout and target observations are attempted independently. Either may be explicitly
`unavailable` without erasing the command's exit, signal, CPU, RSS, pressure or timing result.
Observation time remains outside command timing and resource accounting and is reported separately
for before, after, their sum, and command-plus-observation wall time. The two snapshots are not an
atomic transaction. They are non-authoritative rebuild-value evidence only and grant no target
currentness, compatibility, adoption, retention, cleanup or deletion authority.

The first physical value experiment measured implementation `55ba51845b2d` / tree
`08c66114c547` on 2026-08-30 with Rust/Cargo 1.97.1, 16 logical CPUs and four Cargo build jobs. Its
fixed `resident-eligible-rust-edit-v1` workload ran 1,440 tests successfully, retained one ignored
test, and excluded the same 16 host-fact tests on every accepted sample. Two genuinely absent-target
cold runs and six retained-target warm runs per applicable arm used one command digest and one
comparison key. Complete outer-wall medians were 45.295 seconds cold / 3.230 seconds warm for the
direct local control, 3.225 seconds warm for exact-base `8d3bd6621608` `glaeda-hot-run`, and 43.985
seconds cold / 3.355 seconds warm for value-aware Glaeda. The value-aware warm binding itself took
119.697 milliseconds median (118.008--137.501 ms), making complete warm wall 130 milliseconds /
4.03% above the exact-base wrapper; median CPU rose 140 milliseconds / 4.06%, while median outer
RSS changed by -26 KiB. The retained target occupied about 1.895 GB allocated, and its Glaeda median
cold-to-warm saving was 40.630 seconds / 92.37% / 13.11x. With only two cold observations per arm
and overlapping ranges, the lower Glaeda cold median is not a cold-build speed claim.

Both accepted Glaeda cold runs observed `target` absent before execution and a present generation
afterward. Across the following six warm samples, every pre-command target ID exactly matched the
preceding accepted post-command ID, while Cargo advanced the generation during each run. One prior
Glaeda cold-plus-three-warm cycle was excluded from performance results after a separately owned
full Cargo build was found to have overlapped it; its correctness evidence remained valid. The 58
accepted raw receipt contents hash, in fixed experiment order, to
`sha256:8095b442367f4f22fbdf2262a9d7cb7ef628b92f5b56874ffd89cc7be50be6ed`; the 12 excluded
contents hash separately to `sha256:05ac245de86e2f2e70109605b49e02467a4ac84192fee22d9900fc3c459103ac`.
The candidate/base binaries hash to `sha256:435f05167eae81a04c0c59aa939db61e7b1b6edc978acb38c4756fc22a2e9535`
and `sha256:a3850298b071f311ba6bfe6c9bcf693874d5103f9ed69b9cc618f568eba2e2dd`; the semantic
workload hashes to `sha256:0d886b3b1ec520168a8970939646d8a9ab117471521d13e2d7e5ef8f16cb09ff`.
The final publication candidate additionally compile-gates the Linux-only observer types and moves
their JSON conversion into the Linux path after the first hosted Apple cross-target check exposed
an unconditional import. The Linux observation sequence, report fields and timing boundaries are
unchanged; the full performance matrix was not relabelled or rerun as final-source evidence.

`private` starts with an empty directory and retains the task's later writes without OverlayFS
copy-up. `private-copy` atomically seeds that directory once from the warm resident parent with GNU
`cp --reflink=auto`, then reuses the private lineage on later invocations. The copy uses reflinks
where the backing filesystem supports them and ordinary allocated copies otherwise. A failed copy
never publishes the candidate directory. `overlay` starts from warmed resident bytes and retains
writes in a task-private upper; it remains useful when warm-start value exceeds copy-up cost. `ro`
exposes suitable immutable prepared state without write authority.

When a cross-worktree invocation selects the exact `target` cache path, `hot-run` also binds
`CARGO_TARGET_DIR` to that stable in-view path. This includes Cargo launched indirectly by a shell,
Makefile, or repository script and prevents an inherited caller override from bypassing the
task-private view for an unrelated shared mutable target. Other cache-path names and same-worktree
native execution retain the caller's environment; callers of a custom Cargo output path must bind
that path explicitly.

This prototype does not independently prove that the resident parent matches the task source or
toolchain, or freeze a parent that another process is still mutating. Its ultra-trusted caller must
supply the right quiesced resident generation and still run a real validator. The closed fan-out
harness now runs its bounded cold-read control with
`--page-cache-treatment resident-target-dontneed`: it fsyncs and advises away only the owned
resident target's regular-file pages before the measured window, records the exact scope, and
makes no claim that the advisory operation creates a globally cold machine. The fan-out-4 warm and
cold-advised controls select `private-copy` as the implicit cross-worktree `target` treatment: its
two-window medians beat Overlay by 6.19% warm and 11.76% cold. Explicit
path policies remain unchanged, so source and suitable dependency paths can still select Overlay
or read-only sharing while same-worktree execution remains explicit `native` observation.

The same closed harness can add `--retained-reuse` to execute the complete validator a second time
against the already-accepted edit and retained task state. Its comparison basis names this as a
distinct state phase, so first materialization and repeated command execution cannot be mixed as
one pressure treatment. This measures the repeated developer loop Glaeda is intended to improve;
it does not skip tests or grant cached-result authority.

For a short steady-state discriminator, `--retained-reuse-windows 3` or `7` keeps the exact
worktrees and state while rerunning the complete validator. Each reuse ordinal has a distinct
comparison key, so sequence position remains visible rather than being collapsed into one generic
warm label. The original single-window flag and receipt field remain compatible views of ordinal
one.

The first fan-out-4 use of that window measured two samples per arm. Complete first-use medians
were 26.55 seconds native, 27.51 seconds private-copy, and 29.43 seconds Overlay. Immediate reuse
fell to 17.92, 19.63, and 18.07 seconds respectively while every task reran and accepted the same
1,343-test validator. Overlay's apparent immediate-reuse advantage did not survive the longer
control. In a subsequent two-sample seven-reuse bracket, private-copy remained cumulatively faster
after every ordinal and completed first use plus seven full replays in 155.52 seconds versus 158.11
seconds for Overlay. Keep private lineage as the write-heavy cross-worktree `target` default through
the tested eight-command lifetime. Overlay remains the mostly-read source/dependency view and saves
about 0.806 GiB / 6.68% of allocated state in this ext4 workload; the evidence does not justify an
automatic lifetime-based switch.

Python and Node projects can explicitly reuse immutable prepared dependencies while keeping build
output private:

```bash
scripts/hot-run --resident /path/to/python-resident --task /path/to/task \
  --cache .venv:ro -- python -m pytest
scripts/hot-run --resident /path/to/node-resident --task /path/to/task \
  --cache node_modules:ro --cache .next:private -- pnpm build
```

A repository wrapper may also bind the front-door runtime before any work begins. For an
ultra-trusted current-runtime loop, the runtime ID alone makes `hot-run` hash the resolved command,
record that exact digest, and keep mutable state in the resulting digest-specific namespace:

```bash
scripts/hot-run --resident /path/to/node-resident --task /path/to/task \
  --cache node_modules:ro --cache .next:private \
  --runtime-id node-current \
  -- /path/to/node ./node_modules/.bin/next build
```

When a repository pins an independently declared generation, add its expected digest to refuse
drift before user work:

```bash
scripts/hot-run --resident /path/to/node-resident --task /path/to/task \
  --cache node_modules:ro --cache .next:private \
  --runtime-id node-v22.23.2-linux-x64 \
  --runtime-sha256 sha256:<exact-executable-digest> \
  -- /path/to/node ./node_modules/.bin/next build
```

Commands that launch descendant shebangs through `PATH` can opt into one stronger local binding:

```bash
scripts/hot-run --resident /path/to/node-resident --task /path/to/task \
  --cache node_modules:ro --cache .next:private \
  --runtime-id node-v26.8.1-linux-x64 \
  --runtime-sha256 sha256:<exact-node-digest> \
  --runtime-bin /path/to/node-v26.8.1-linux-x64/bin \
  -- node ./node_modules/.bin/next build
```

`--runtime-bin` requires a runtime ID, accepts only one absolute canonical plain directory owned by
root or the current user and not group- or world-writable, resolves the launched executable from
that directory, and places the directory first in the inherited descendant `PATH`. The receipt
records only `runtime_bin_first` plus an opaque binding digest; the private-state namespace also
includes that digest, so bound and unbound executions or replaced directories cannot share mutable
lineage. No private path is recorded.

This closes the common `/usr/bin/env node` and package-script shebang path for an explicitly
selected ultra-trusted toolchain. It does not hash the whole toolchain tree, intercept absolute
descendant paths, override explicit language-specific executable variables, identify package
manager semantics, or make mutable runtime bytes hostile-safe. A replaced directory is refused
during preflight; in-place mutation remains the caller's quiescence/generation responsibility.

The runtime ID is optional. When present, `hot-run` resolves the command executable, hashes its
content before launch, records the public ID plus exact observed digest, and places mutable
private/overlay state below a digest-specific opaque namespace. An optional `--runtime-sha256`
turns that observation into an expected-digest check and refuses drift. Supplying a digest without
an ID is invalid.

The two front-door-only forms prevent changed launched bytes from silently sharing one private
build-state lineage. Only the explicit-digest form proves agreement with a separately declared
generation. Without `--runtime-bin`, neither is automatic language-manifest discovery, a claim
about descendant executables, or a hostile-code security boundary. With `--runtime-bin`, only
ordinary descendant `PATH` selection is added. The ultra-trusted caller remains responsible for
declaring the right runtime and preventing in-place executable or toolchain replacement between
observation and execution.

`scripts/hot-observe` is the read-only companion for state that may already be resident. A
repository adapter supplies bounded public labels for canonical input files and prepared-state
anchors; Glaeda hashes the no-follow regular files, verifies shared labels have identical content,
and reports the dependency tree's current entry count plus logical and allocated bytes:

```bash
scripts/hot-observe --output json dependency \
  --project-id example --dependency-root /private/prepared/dependencies \
  --runtime-id node-v22.23.2-linux-x64 \
  --runtime-sha256 sha256:<exact-executable-digest> \
  --parent lock=/private/source/pnpm-lock.yaml \
  --anchor lock=/private/prepared/dependencies/.pnpm/lock.yaml \
  --anchor layout=/private/prepared/dependencies/.modules.yaml
```

The same tool can correlate one Linux TCP listener with the expected owner, workspace directory
inode, executable content, runtime identity, and loopback/any exposure while recording bounded age,
CPU, and RSS observations:

```bash
scripts/hot-observe --output json service \
  --project-id example --service-id web-dev-v1 \
  --workspace /private/task --port 3000 --exposure loopback \
  --runtime-id node-v22.23.2-linux-x64 \
  --runtime-sha256 sha256:<exact-executable-digest>
```

Both documents are path-free and observation-only. They contain no PID, argv, environment, file
name, or command output and grant no adoption, lease, signal, cleanup, cache, or result authority.
`anchor_aligned` proves only the repository-declared anchors, not every dependency byte or package
manager semantic. `physical_match` proves the observed listener/process facts, not which application
protocol or source semantics the process serves. Missing or drifting facts remain ordinary
`absent`, `revalidate_required`, `drift`, or `ambiguous` observations for a later cold/reset/lease
decision.

`--measurement result.json` atomically writes one bounded developer observation containing command
elapsed time, user/system CPU time, peak RSS, exit/signal, completion reason, configured timeout,
cross-worktree mode, relative cache policies, and the optional public runtime contract plus its
path-free descendant-bin binding when selected. A
`private-copy` observation also records whether the lineage was seeded or reused, its copy wall
time, and command-plus-preparation wall time. With `--seed-source-mtimes`, when a task-private
`target` is first seeded, Glaeda compares tracked regular files through beneath-root, no-follow
descriptors and gives only byte-identical, executable-mode-compatible, singly linked task files the
resident file's mtime. This
preserves Cargo freshness
for an ordinary worktree created after its exact warm parent without backdating an edited file.
The caller must already own exact warm-parent proof; the flag creates none. The matching pass is
recorded with bounded counts and time. It never runs on retained state: a
file reverted after a prior task build must remain newer so Cargo can rebuild it. Copy/matching
CPU and RSS are not included in the command resource fields. Measured commands put GNU `time`
immediately around the workload, so CPU and peak RSS cover that command tree without inheriting the
largest child previously used for Git, copying, runtime discovery, or other preparation. Profiled
commands put the same observer inside the resource scope. Commands without `--measurement` remain
unwrapped. The receipt contains no command, output, environment, repository identity, file name,
or private path and grants no verification or result-reuse authority.

The same receipt includes aggregate Linux machine observations immediately before process start and
after process settlement. Fixed kernel interfaces supply online/allowed CPU counts, load averages,
available memory, used/total swap, and CPU/memory/I/O pressure-stall information. Missing,
malformed, oversized, inconsistent, or unavailable facts remain `partial` or `unavailable`; they
are never converted to zero pressure. Observation time stays outside command elapsed/CPU/RSS, and
commands without `--measurement` perform no machine observation. These snapshots name no process,
PID, command, environment, cgroup, repository, or path. They expose benchmark-contamination and
placement evidence only and add no wait, refusal, signal, scheduling, service-control, cleanup,
cache, or result-reuse authority.

The receipt also derives signed available-memory and swap-use deltas plus monotonic PSI cumulative
counter deltas and stall fractions over command elapsed time. These values require complete
before/after endpoints; missing evidence or a counter reset remains `null`. The snapshot envelope
is very slightly wider than child elapsed, so tiny-command fractions are contamination evidence,
not an admission threshold.

This derived machine-observation shape entered hot-run measurement schema version 3. Schema version
4 added one optional caller-owned `comparison_key`: a canonical opaque SHA-256 digest covering the
exact workload/source/toolchain/cache/fan-out basis that the caller already owns. Schema version 5
adds the path-free seed-only source-metadata preparation above and includes its time in total
preparation. Schema version 6 replaces process-lifetime child peak RSS on unprofiled commands with
workload-scoped GNU-time CPU/RSS. The reducer accepts only version 6 so historical version 5 RSS
cannot silently enter the same observed range. The comparison key is accepted only with
`--measurement`, records no command or private input, and grants no semantic, result, cache,
scheduling, or admission authority. Existing version 1 through 5 receipts remain historical
observations; no producer silently relabels them as comparable.

Two or more successful schema-v6 receipts carrying the exact same key can be reduced without a
persistent history service:

```bash
scripts/hot-pressure-shadow --output json \
  --current current.json \
  --baseline earlier-a.json \
  --baseline earlier-b.json
```

The reducer refuses mixed keys, failed/interrupted inputs, symlinks, oversized documents, and
duplicate receipt content. Zero or one distinct baseline reports `insufficient_history`; two or
more report descriptive observed min/max ranges for command-plus-state-preparation latency, command
timing, CPU/RSS, memory/swap deltas, and CPU/memory/I/O PSI fractions. Missing pressure remains
`unknown`.
The shadow finding can describe a current sample as slower and/or higher-pressure than its own
observed range, but it never waits, retries, schedules, admits, routes, cancels, mutates, or claims
statistical confidence. Exact workload semantics and result validation remain with the caller that
constructed the comparison key.

Unscoped commands use child `getrusage`; profiled commands place GNU `time` inside the scope so CPU
and peak RSS describe the workload rather than the `systemd-run` launcher. Force termination can
prevent descendants or the inner timer from reporting complete usage, so those three fields are
explicitly `null` instead of fabricated on deadlines and operator interrupts.

Heavy commands must also use `--timeout SECONDS`; the resource profile is rejected without that
bounded settlement deadline. For an unprofiled command, the wall-clock
deadline owns the whole command process group, first requests termination, escalates after a
two-second grace period, returns the conventional status 124, and writes the failure receipt. It
keeps observing the owned group when its leader exits before a descendant, so a signal-ignoring
background child cannot escape the escalation. An operator interrupt similarly returns 130 without
a Python traceback and records `operator_interrupt`. A timeout deliberately creates a separate
process group; use the unbounded mode for commands that require interactive terminal job control.

Heavy local work on the measured machine may opt into `--resource-profile big-red-heavy`. It uses
one collected user systemd scope with a 1,200% CPU quota, 8 GiB memory-high threshold, 12 GiB hard
memory ceiling, and 1,024-task ceiling. The explicitly machine-specific profile reflects big-red's
12-way build point, which was within about 0.4% of 16-way Cargo while leaving interactive and
multi-agent headroom. The profile is never applied to ordinary commands implicitly; an unscoped
no-op avoided even the roughly 0.01-second scope cost. Glaeda generates the transient unit name and
uses its exact currently running executable as an in-scope entry helper. The helper stops itself
before executing the workload and is resumed through its pidfd only after Glaeda has opened the
launched process's exact cgroup directory and `cgroup.kill` control by file descriptor. A deadline
or interrupt first signals the ordinary process group; if escaped descendants remain after the
two-second grace, Glaeda writes the held fd-bound kill control and waits for `cgroup.events` through
the held directory before publishing the terminal result. Neither observation nor mutation reopens
the generated unit name. A fast-exiting leader and a child that creates a new session therefore
cannot skip or escape the resource-scope settlement check.

Explicitly deferrable trusted work may instead use `--resource-profile big-red-background`. That
profile asks the native cgroup-v2 scheduler for `CPUWeight=25` without pinning CPUs or imposing a
CPU quota, so it remains work-conserving when the machine is otherwise idle and yields relative CPU
share when foreground work competes. It has the same required deadline, stopped entry,
descriptor-bound cgroup observation/kill, process settlement, and measurement behavior as the
heavy profile. Glaeda never selects this profile implicitly; the caller owns the fact that useful
work is deferrable, and the profile grants no admission or host-wide reservation authority.

The task and resident must be worktrees of the same Git repository. Direct same-worktree execution
may declare `native` cache observations; all other explicit modes require the cross-worktree path.
The resident worktree remains the stable compiler pathname, while task source changes remain in the
ordinary task worktree and write-heavy cache writes land in a task-private state directory. Cache
paths must be relative,
unique, non-overlapping directories. One non-blocking task-state lock prevents concurrent use of
the same mutable lineage or linked Git view, including while a private parent is copied. The command
receives the caller's terminal, environment, host filesystem, devices, processes, and network and
returns the child's
status. This is explicitly an ultra-trusted performance tool, not a security boundary or
result-authority mechanism.

Unprivileged bubblewrap maps host identities outside the caller's user namespace to the overflow
identity. Commands that deliberately validate host ownership, mount identity, or other physical
namespace facts must use the ordinary host path and final verifier. The hot path is for compilation,
language tools, repository scripts, and tests whose semantics do not depend on those host facts.

On Ubuntu 26.04.1, Linux 7.0, ext4, Rust 1.97.1, and Glaeda `7f40597`, one bounded G0 probe used the
same `cargo test --locked --lib --bins --no-run` command at four Cargo jobs. A fresh ordinary
worktree/path took about 39–42 seconds to compile. An exact task over a resident target started in
0.03 seconds; after a one-line task edit, the private target upper completed in 9.21 seconds. A
direct unmodified full library/binary test execution was 2.63 seconds. These observations promote
the stable-path/private-upper experiment, not OverlayFS as the final write-heavy storage default.
The resulting script measured 0.05 seconds for no-op Bash and Python commands, 0.13 seconds for an
exact Rust no-run check, and 10.06 seconds after the same one-line edit versus a 37.56-second
path-cold rebuild.

The implicit default task state is an opaque path under the user's cache directory. Its lineage
binds the resident and task paths, cache modes, runtime contract, physical resident/task/common-Git
directory objects, Git administrative directories, and each linked worktree's stable `.git`
pointer-file witness. Reusing the same physical worktrees retains their state; removing and
recreating a worktree at the same pathname selects a fresh state even if the filesystem immediately
recycles its inode. Cross-worktree launch holds the validated task, common-Git, task-Git, and cache
objects with `O_PATH` descriptors and makes bubblewrap consume those objects through `--bind-fd`
or `--ro-bind-fd`. Overlay lowers first become route-owned read-only FD-bound mounts. Every
descriptor inherited by bubblewrap is consumed by exactly one bind operation before the workload
executes, so the command cannot reach around a private cache view through `/proc/self/fd`. The
physical witnesses are revalidated after taking the state lock and after preparation; later
pathname replacement therefore cannot substitute a new object between validation and bind
consumption.

An explicit `--state` remains caller-owned and can intentionally continue a lineage across
worktree generations. New implicit generations are first staged with an owner-private producer
manifest, then atomically published under a retained namespace lock. The manifest itself is first
written and synced under a private temporary name, moved into place with `RENAME_NOREPLACE`, and
followed by a directory sync; interruption can therefore leave an unpublished temporary file but
never a partially published final manifest. The reader requires canonical bytes and independently
recomputes the v3 state digest from all seven generation objects, Git-relative relationships and
cache modes before any retirement. The manifest carries no workflow-result or source-validity
authority. Existing default-key v1 and v2 directories remain disjoint legacy state. A manifestless
directory at the exact computed v3 path is a collision and fails before any lock, runtime or cache
state is created; it is never adopted. The collector does not delete arbitrary legacy or collision
payload. The key transition also prevents an older hot-run implementation that does not
participate in the namespace protocol from opening a new producer-managed state concurrently.

Ordinary implicit hot-run activity performs one bounded opportunistic recovery/retirement pass
when it can take the namespace lock exclusively without waiting. No age, timeout, PID, name,
occupancy total, or `/proc/locks` absence makes a live state eligible. A different manifested state
is eligible only when at least one exact recorded worktree-generation object is now absent or has a
different physical identity, every direct and runtime-subgeneration lock can be acquired
nonblocking, the manifest is unchanged on a second read, and the generation is still unreachable
on a second observation. The collector atomically renames that state with `RENAME_NOREPLACE` and
syncs the namespace before allowing a new admission to cold-reconstruct the canonical path.
Deletion stays beneath held no-follow directory descriptors on the same filesystem, never follows
symlinks, and removes at most 2,048 entries per invocation. A private retirement record closes the
final manifest-unlink/directory-removal crash window; later normal activity resumes an interrupted
publication, retirement, or bounded deletion. One pass refuses after observing more than 256
namespace entries or more than 64 entries in a candidate generation, so ordinary launch never
materializes an unbounded directory inventory. Unpublished creating-stage recovery removes only
at most two private regular manifest/final-manifest-temporary files beneath the exact stage; any
other payload is preserved as recovery debt. Explicit `--state`, legacy, malformed, forged,
noncanonical, mode-drifted, foreign-owned, active, and still-reachable state remains untouched.
#910 tracks unreachable-generation retirement. The first #926 lifecycle slice extends only the
same producer-managed implicit namespace; it does not adopt checkout-local Cargo targets or
explicit `--state` directories. After each successful implicit command, while the exact state or
runtime lock is still held, Glaeda advances one monotonic namespace use sequence in a canonical,
private, atomically replaced catalog. Each record is bound to the immutable producer manifest's
device, inode and creation witness. Failed, signaled and deadline-expired commands do not advance
use. A stale record cannot attach to a cold reconstruction of the same state name because its new
manifest object has a different witness.

Normal exclusive namespace activity observes filesystem capacity with `statvfs`. At 90% used it
atomically latches a pressure cycle; bounded later invocations continue through the hysteresis gap
until use reaches 85% or less. At ordinary capacity the policy performs no retirement. Under
pressure, it considers successful-use records in deterministic least-recently-used order and
still requires an authentic reachable manifest, exact manifest-record identity, every direct and
runtime lock acquired nonblocking, a second equal manifest and reachability observation, unchanged
locks, and the same pinned state directory before rename. Missing, corrupt, stale, explicit,
legacy, malformed, current, active and unknown-use state remains untouched. One invocation retires
at most one state through the existing no-replace rename, namespace sync, external retirement
record and descriptor-relative bounded deletion transaction.

When a caller supplies a comparison key, the catalog can retain producer-observed reconstruction
and reuse duration for later health reporting. Those caller-named comparison families never grant
eligibility or influence retirement order. They remain performance evidence only; Glaeda's own
successful-use sequence ranks states that the ownership and lock protocol has already made safe.
`--verbose` reports the path-free lifecycle and success-record dispositions. #926 remains the home
for health-view composition and for moving future ordinary Cargo targets into this managed
producer contract; arbitrary historical worktree `target/` trees remain outside deletion authority.

The bounded-discovery repair was measured at exact code `4436b3f63a7c20ae6d15bdce44a10ea6760cbfba`
against rejected control `dbb3863c14d76880a8c7d155d84888926041c019`, Python 3.14.4 and Linux
7.0.0-30-generic. Five hundred alternating calls over an empty namespace measured 4.480 microseconds
control versus 4.565 microseconds candidate median: 85 nanoseconds / 1.90% added to the ordinary
collector call. Thirty alternating calls over 10,000 foreign entries measured 3.999 milliseconds
for the old complete scan versus 142.027 microseconds for the new typed bounded refusal: 96.45%
lower / 28.16x. The large-directory results intentionally have different semantics; the candidate
does not claim a complete inventory after its 256-entry ceiling. All 1,060 timed calls preserved
both namespace state vectors. Harness and report SHA-256 values were
`293d23280ca75429735a26cccaed0d6578df7718f7e4b58b0a0e95ce667c5f29` and
`ac2872e76a6126892854ab2c6c8596ff9c34d70f7e1e0cd86e2191717da1bfd1`.

All task state is expendable: discarding it or selecting a new empty `--state` path produces a
private cold upper and a normal compiler rebuild. Bubblewrap and kernel OverlayFS are required for
cross-worktree mode; running directly in the resident worktree does not require either.

The Linux CLI can turn one explicit hot-run cache root into the existing bounded, path-free cache
status report:

```text
glaeda --output json cache observe-hot-run --root <explicit-hot-run-root>
```

This is filesystem-metadata observation only. It follows no symlinks, reads no file contents, and
stays on the root filesystem. A complete observation rejects more than 1,024 states, 2,000,000
objects, more than 512 recognized lock candidates, depth over 64, cross-state hardlinks, drift, and
unsupported filesystem shapes. Logical bytes match GNU `du --apparent-size` semantics by excluding
directory entry sizes; allocated bytes sum unique per-state `st_blocks * 512`, including
directories. The namespace root itself is excluded from the per-state aggregate. As with `du`,
`st_blocks` is inode-reported allocation rather than unique backing usage when reflinks or
block-level deduplication share extents. The secondary runtime-lock discovery pass has its own
2,000,000-entry work bound rather than relying on the first traversal's count.

Protected OverlayFS work directories and nested special nodes produce a successful path-free
schema-v2 `completeness: partial` observation after the top-level state set is revalidated. Its
state count is known, but every byte total and classifier count is JSON `null`, `states` is empty,
and `problems` identifies `permission_denied` or `unsupported_node`. Do not interpret null as zero
or run the development binary with extra privilege to fill it in. Root unavailability, invalid
top-level state shape, drift or rebinding, cross-state physical attribution, bounds, and arithmetic
failure remain hard errors.

On Linux, a complete observation also reads one bounded `/proc/locks` snapshot. An exact held
whole-file exclusive BSD flock on the descriptor-retained direct `lock` file or a direct
`runtime-<64hex>/lock` file establishes `active_lock: true` for that state. An absent, malformed or
oversized snapshot remains unknown, as do POSIX/OFD/read/partial/blocked rows and every recognized
lock file without a matching held row. The command never reports lock absence. A partial
observation never enters the classifier and therefore never emits an active-lock fact.

Every state in a complete observation remains `ownership_unknown` with generation, worktree,
reconstruction, lease, mount, open-file, process, cleanup, and quarantine evidence unknown. Lock
evidence is either definitely active at the bounded kernel snapshot or unknown. A complete
observation therefore reports disk occupancy and can conservatively identify an in-use state, but
cannot make any state reclaimable. Neither complete nor partial observation creates catalog,
marker, adoption, retirement, signaling, or cleanup authority.

An explicit checkout-local Cargo target has a separate Linux observation front door:

```text
glaeda-cargo-target-observe --checkout /canonical/checkout --output json
glaeda-cargo-target-holders --checkout /canonical/checkout --output json
```

The command brackets one bounded descriptor-relative `target` metadata walk with two equal offline
Git checkout observations. It reports an opaque target materialization identity, entry/directory/
unique-object counts, logical bytes, visible `st_blocks * 512`, the latest observed object mtime,
target/checkout and descendant-owner evidence, external-hardlink ambiguity, and metadata for the fixed `.rustc_info.json`
marker when present. It follows no symlink, reads no target file content, uses `O_NOATIME`, remains
on the checkout filesystem, and emits no checkout path or child name. Reflinks and shared extents
mean visible blocks are not exclusive or necessarily reclaimable bytes.

The holder command separately scans bounded Linux `/proc` evidence for visible process CWD, root,
open-file, mapped-file and mount-namespace references to the exact same opaque target identity. It
emits counts and coverage only: never a PID, process name, command, path, file name, map row, mount
row or environment value. The observer itself is excluded. At most 131,072 processes, 2,000,000
file descriptors, 16 MiB per pseudo-file and 256 MiB total pseudo-file content are accepted.
Unreadable process evidence and process-table churn remain explicit.

This is deliberately positive-only evidence. `holders_observed` proves that at least one visible
reference existed during the non-atomic scan. `none_observed` means only that the bounded scan saw
none; it never proves universal absence, idleness, quiescence or safe retirement. A positive report
does not authorize signaling a process, and either disposition grants no retention, cleanup or
deletion authority.

An explicit set of successful schema-v6 native measurements can be joined to the current target:

```text
glaeda-cargo-target-value --checkout /canonical/checkout \
  --cold /private/cold-a.json --cold /private/cold-b.json \
  --warm /private/warm-a.json --warm /private/warm-b.json \
  --output json
```

The value observer accepts at most 32 private regular files per arm. It rejects symlinks,
non-private/foreign-owned files, duplicate physical receipt identities, files over 1 MiB, read
drift, unsuccessful commands, mixed comparison keys/resource/runtime contracts, non-native cache
views, source mismatch, cold samples that did not start absent, and warm samples that did not reuse
the currently observed target directory. Equal checkout and complete target observations bracket
the join so observable concurrent drift fails closed. It reports cold/warm median wall, CPU and RSS
plus absolute/percentage/ratio wall savings and current visible allocation.

The input receipts remain caller-supplied performance evidence, not authenticated execution truth.
The report is path-private but intentionally includes the existing project/branch/source evidence.
It is non-atomic and grants no cache compatibility, currentness beyond the bracketed snapshots,
lease, retention, reclamation, cleanup or deletion authority. Schema v6 has no epoch timestamp, so
last successful use remains explicitly unknown; newest mtime is still not a substitute. Issue #926
owns durable producer lifecycle, last-use evidence and deterministic budget/retirement policy
before any target can become an eviction candidate.

## Linux mount path

The privileged OverlayFS mount machinery is also concrete.

The landed executor uses safe Rust through the Linux new mount API with held descriptors:

```text
fsopen("overlay")
-> fsconfig_set_fd(lower)
-> fsconfig_set_fd(upper)
-> fsconfig_set_fd(work)
-> exact second confirmation
-> FSCONFIG_CMD_CREATE
-> fsmount
-> move_mount onto exact merged target
-> reopen visible target
-> exact post-mount observation
```

No free-form `/bin/mount` option string is the product interface. Lower/upper/work/merged roles are descriptor-bound. The final safe empty-workdir confirmation is immediately before `FSCONFIG_CMD_CREATE`, which physical probes identified as the first observed operation that mutates OverlayFS work state.

Owned ARM measurements recorded mount-only fan-out around **7.7 ms / 23 ms / 62 ms for 1 / 8 / 32 mounts** in the measured path.

The syscall machinery is deliberately separate from production authority. Physical task-view activation still depends on the exact project-disk/filesystem/resident-sandbox correlation and guest-control composition owned by #565/#640/#588/#707/#708.

## Persistent project filesystem

The useful resident project filesystem is Linux-native state, not a macOS checkout projected into the guest hot path.

Current direction:

```text
Mac
  durable control / backend ownership
  Lima/VZ backing storage
        |
        v
Linux guest
  persistent project disk
  ext4/XFS as measured
  native Linux page cache
  source anchors
  Git pools
  package/compiler state
  indexes/services
  task-local OverlayFS/CoW views
```

This keeps Git, package-manager, compiler, index, cleanup, and other filesystem-heavy workload operations on Linux filesystem semantics.

The project disk itself remains working/acceleration state. Its lifecycle is single-writer, generation-bound, revalidated, and recoverable. Disk presence, path names, filesystem UUIDs, mount IDs, and surviving bytes never create ownership or result authority by themselves.

## Storage experiments still worth running

The existence of the current OverlayFS path does not settle the physical storage choice underneath it.

Continue comparing complete workloads across bounded candidates such as:

```text
resident root/ext4
dedicated persistent ext4 project disk
dedicated persistent XFS/reflink project disk
internal storage vs suitable TB5 external storage
```

Measure:

- request/edit-to-first-useful-result;
- source/task-view creation;
- compiler/build-state reuse;
- package/dependency behavior;
- cleanup/reset latency;
- guest allocated blocks;
- Mac backing-file growth;
- CPU/RAM cost;
- concurrency and tail latency;
- recovery after VM/controller interruption.

Block-level dedupe/compression or another filesystem earns a place only when current measurements expose a remaining problem it can plausibly solve.

## Backend programme

Keep project/workload semantics constant while comparing resident execution backends.

Current candidates include Lima/VZ and Apple `container` / Containerization. Native Linux hosts and additional owned fleet nodes remain important reference/target classes.

The meaningful comparison is the complete result path, not blank-environment startup alone:

```text
work becomes known
-> eligible host/backend selected
-> hot state admitted
-> required view/state available
-> useful work starts
-> useful output returns
```

A backend that boots a blank environment quicker can still lose to a resident backend holding valid project/compiler/data state. A better underlying backend should improve Glaeda without redefining Glaeda.

## Parallel preparation

When useful state is not already resident, overlap independent preparation:

```text
work observed
├─ choose host/backend
├─ reserve capacity
├─ materialize/resume execution environment
├─ hydrate missing immutable inputs
├─ resolve reusable state generations
├─ prepare integration-specific handoff where needed
└─ compile workload-specific execution/verification plan
```

Join only at real dependency boundaries.

## Hot-state classes

Do not call every reusable thing “cache.” Distinguish at least:

- **immutable reusable generation** — source/object pools, prepared images, datasets, model assets, derived artifacts;
- **lease-scoped mutable resident state** — project checkout, compiler trees, indexes, databases, resident services;
- **task-local mutable state** — edits, debugging state, private build output, task upper layers;
- **shared mutable state** — only with an explicit producer/consumer and poisoning model;
- **disposable physical cache** — freely evictable optimization state.

Each family needs exact validity parents, resource accounting, reset/revalidation behavior, and a cold reacquisition route when it is classified as rebuildable.

## Correctness rule

Hot state carries performance value. Durable owned facts and fresh exact observation carry execution authority.

When validity or ownership becomes ambiguous:

```text
revalidate
-> reset if required
-> quarantine when current ownership is unclear
-> destroy and reconstruct when cheaper or safer
```

Losing hot state should cost latency and compute, not the ability to decide the next safe action.

## First proving workloads

### Quarry

Use Quarry to exercise:

- persistent Python/research environments;
- edit-to-focused-pytest loops;
- repeated task fan-out;
- data/index residency;
- concurrent independent research jobs;
- host economics across Apple VZ and native Linux references.

### Glaeda

Use Glaeda itself to exercise:

- resident Rust source/task views;
- private Cargo incremental build state;
- `sccache` usefulness;
- immutable Git pools;
- exact one-shot guest-control transactions;
- project-disk and mount recovery;
- cold versus prepared versus resident execution.

### Non-repository compute

The generic workload seam under #770/#773 should also prove that hot admission, capacity, lifecycle, and placement are not repository-only. A bounded dataset transform, render, or similar non-repository workload can use the same execution kernel while its adapter owns result semantics.

Operator-machine handoff is also a recurring workload. On Linux,
[`glaeda-host-observe`](LINUX_HOST_OBSERVATION.md) replaces a loose collection of shell, `awk`,
`ss`, and `systemctl` probes with one bounded path-private observation of the current execution
context. It is evidence for an agent or later admission policy, never admission or cleanup
authority by itself.

## Promotion rule

A hot optimization becomes preferred only when controlled evidence shows a useful complete-loop improvement and its validity/reset contract is explicit.

A promotion receipt should record at least:

```text
workload class
baseline identity
candidate identity
exact validity inputs
p50 / p90 useful-result latency
CPU/RAM/storage effect
concurrency effect
reset/invalidation behavior
fallback path
```

The end state is simple to describe even if the internals are exacting:

> **Keep the useful world hot, materialize only what changed, and let trust decide what may survive.**
