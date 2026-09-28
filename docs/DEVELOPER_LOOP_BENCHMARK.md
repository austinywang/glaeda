# Developer-loop benchmark

This benchmark measures one useful Rust edit-to-verification loop across execution arms. It exists to prevent fresh-clone speedups, cache hits, and semantically different test scopes from being compared as though they were the same result.

## Workload contract

The frozen local evidence used Rust source commit `b9fa23462420c13a465d635d9694f0c827c1e685`, tree `edd0b7bb9d3e59305c21c69b721b5278d8aff6da`, pinned Rust/Cargo 1.97.1, and default Cargo concurrency on a 16-logical-CPU, 30-GiB big-red host.

The edit fixture adds one documentation line to `src/lib.rs` without changing behavior. Its tracked-workload diff digest is `sha256:bfdd60e73e8b106c0129d1052310495ae2dbe1ff70bb52b35a9f9ef4911927eb`.

`scripts/benchmark-developer-loop` runs the same locked lib/bin test command in every arm. Receipt
schema v2 derives selected, executed, passed, failed, ignored, measured, and filtered counts from
every Rust test-harness terminal summary; a successful command without a consistent observed
inventory is rejected instead of emitting a stale checked-in count. It excludes exactly 16
host-fact tests that currently observe different `/usr/bin/env` ownership and parent-directory
safety inside `hot-run`'s cross-worktree user/mount namespace. The script names every exclusion
and emits a path-free JSON receipt even when the workload fails. Historical measurements below
retain the exact 1,343-test corpus they observed at those earlier source revisions.

This profile is a resident-eligible developer-loop result, not a replacement for full host verification. The full suite remains a required distinct verifier.

## First big-red control matrix

All three-sample rows report every sample and the median. Three samples are not enough to claim p90.

| Arm | Setup state | Wall seconds | Median | Peak RSS KiB | Result |
| --- | --- | ---: | ---: | ---: | --- |
| Fresh local | no target tree; shared Cargo download cache | 43.45, 44.16, 43.67 | 43.67 | 2,515,488–2,525,164 | green |
| Ordinary native prime | no target tree; unedited base | 43.39, 42.03, 43.26 | 43.26 | 2,491,872–2,518,568 | green |
| Ordinary native edit | private target primed on unedited base | 10.14, 10.62, 10.31 | 10.31 | 1,638,324–1,639,292 | green |
| Glaeda resident edit | shared resident lower target plus fresh private overlay state | 10.92, 11.43, 11.06 | 11.06 | 1,631,712–1,632,296 | green |

The current Glaeda resident path is 3.95 times faster than fresh local at the median, but ordinary Cargo incremental is 4.24 times faster than fresh local and beats Glaeda by 0.75 seconds, or 7.3%. For this edit shape, current stable-path isolation adds semantics and private writable cache state but does not yet improve latency over a normal warm worktree.

## Big-red path-class follow-up

### Superseded storage attribution

The first private-lineage follow-up used an explicit state root below `/tmp`. On big-red `/tmp` is
`tmpfs`, not the host's ext4 filesystem. Its `43.98, 41.98, 42.43`-second primes and
`9.87, 10.50, 10.78`-second edits are valid tmpfs observations, but they do not support an ext4
mechanism claim. The original state was removed in 0.49 seconds after recording its receipts. The
storage attribution and any ext4 conclusion derived from those samples are superseded by the
same-filesystem rerun below.

### Physical ext4 rerun

The correction kept the same frozen commit, tree, fixture, toolchain, command, 16-logical-CPU host,
and default Cargo concurrency. The resident `target`, every Overlay upper/work directory, and every
private lineage were freshly proven on the same ext4 filesystem. Three independent empty-private
lineages each ran the complete unedited workload once, received the exact fixture, and ran the
complete workload once more. Three separate edited tasks compared a warmed resident Overlay lower
with a full ordinary-copy private seed from that exact lower.

| Phase | Wall seconds | Median | User CPU seconds | System CPU seconds | Peak RSS KiB | Result |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Empty-private cold prime | 41.76, 42.53, 43.67 | 42.53 | 104.76, 109.09, 109.05 | 9.43, 9.81, 9.78 | 2,512,408, 2,503,152, 2,502,760 | green |
| Empty-private edit | 10.65, 10.38, 10.37 | 10.38 | 17.41, 16.88, 16.73 | 3.88, 3.91, 4.14 | 1,634,556, 1,635,252, 1,630,944 | green |
| Warmed Overlay edit | 11.85, 11.94, 12.68 | 11.94 | 16.30, 16.51, 17.87 | 3.90, 4.32, 4.40 | 1,638,348, 1,638,040, 1,637,116 | green |
| Ordinary-copy private seed | 0.43, 0.47, 0.59 | 0.47 | 0.00, 0.00, 0.00 | 0.42, 0.47, 0.58 | 2,380, 2,492, 2,512 | green |
| Copy-seeded private edit | 10.04, 10.79, 10.32 | 10.32 | 16.69, 17.37, 17.58 | 3.79, 3.69, 3.71 | 1,622,412, 1,633,428, 1,632,676 | green |

Every edit receipt carried the exact fixture digest and every run completed 1,343 executed tests
with one existing ignored test. Outer `hot-run` medians were 10.470375 seconds for empty-private,
12.074703 seconds for Overlay, and 10.450082 seconds for the already-seeded private command. Pairing
each copy with its edit produced inner request-to-result samples of 10.47, 11.26, and 10.91 seconds
(median 10.91); the corresponding outer totals were 10.603277, 11.357405, and 11.040082 seconds
(median 11.040082).

The same-filesystem `cp --reflink=always` control refused in 0.01 seconds with `Operation not
supported`. The ordinary copy therefore allocated the full exact parent: 1,960,813,503 logical
bytes and 1,963,872,256–1,963,876,352 allocated bytes. After the edit, the copy-seeded private
states occupied 2,716,748,969–2,716,749,002 logical and 2,720,026,624–2,720,038,912 allocated bytes.
The empty-private final allocation was 2,720,018,432–2,720,022,528 bytes. Overlay state occupied
2,537,993,803–2,537,993,825 logical and 2,539,700,224–2,539,720,704 allocated bytes, so it saved
only about 172 MiB versus a complete private lineage for this real workload.

After recording the receipts, the three task worktrees removed in 0.04, 0.00, and 0.00 seconds;
the warmed resident worktree and its target removed in 0.12 seconds. The three copy-seeded,
Overlay, and empty-private state families removed in 0.22, 0.11, and 0.33 seconds respectively;
receipt cleanup was below 0.01 seconds. No benchmark process or mount survived.

On this ext4 host, an empty private lineage pays the full cold compile once. Copy-seeding avoids
that prime: its 10.91-second preparation-plus-result median was 1.03 seconds, or 8.6%, faster than
the 11.94-second Overlay result, while remaining 0.60 seconds behind the earlier 10.31-second
ordinary-native median. That makes ordinary-copy seeding a measured retained-lineage candidate,
not a universal default: it consumes the full parent allocation, depends on an exact warm parent,
and still needs concurrent fan-out and cold-page-cache controls. Reflink-capable storage remains
the preferred cheap-seeding candidate. Source fan-out, Git metadata, dependencies, compiler output,
and resident services remain separate mechanism decisions.

### Executable `private-copy` candidate

The first implementation dogfood repeated the same exact edited workload through the actual
`target:private-copy` mode, with three new task/state lineages physically on ext4. The mode copied
the exact warm parent into a hidden candidate, atomically published the private directory, mounted
it at the stable resident pathname, ran the validator, and emitted one preparation-plus-command
receipt:

| Measurement | Samples (s) | Median (s) |
| --- | ---: | ---: |
| Atomic private-copy preparation | 0.434903, 0.407321, 0.418423 | 0.418423 |
| `hot-run` command | 9.520338, 10.214857, 10.198427 | 10.198427 |
| Receipt copy + command | 9.955241, 10.622178, 10.616850 | 10.616850 |
| Inner workload | 9.42, 10.11, 10.10 | 10.10 |

All three receipts reported `seeded`; all validators were green, with 1,630,948–1,632,464 KiB
peak RSS for the workload. Final lineages each allocated 2,720,026,624 bytes. One immediate reuse
reported `reused`, zero preparation time, 2.732046 seconds command time, and 2.79 seconds outside
the whole process; its inner workload was 2.63 seconds. The receipt deliberately keeps copy CPU/RSS
outside command resource accounting while exposing copy wall time and command-plus-copy wall time.
The three task worktrees removed below 0.01 seconds each, the warmed resident worktree removed in
0.12 seconds, all three private-copy states removed in 0.45 seconds, and receipt cleanup was below
0.01 seconds. No process or mount survived.

The executable candidate's 10.616850-second measured median is 1.323 seconds, or 11.1%, faster than
the corrected 11.94-second ext4 Overlay median and 0.307 seconds, or 3.0%, slower than the earlier
10.31-second ordinary-native median. This validates the mechanism and its observation surface; it
does not yet settle concurrent fan-out, cold-page-cache copy behavior, or automatic selection.

## Correctness failure retained as evidence

Running the full lib/bin command through the same Glaeda resident path took 11.50 seconds, used 1,631,808 KiB peak RSS, and exited 101 after 1,326 passed, 16 failed, and one ignored library test. Fifteen failures could not select a reviewed environment executable after namespace ownership remapping; one rootless-Podman observation saw an unsafe parent directory instead of a missing source.

That sample is not a speed result. It establishes a routing/compatibility requirement: Glaeda must either preserve those host facts, route those tests to an equivalent host verifier, or refuse to claim full verification.

## Hosted controls

`.github/workflows/developer-loop-benchmark.yml` measures two GitHub-hosted Ubuntu 24.04 controls from the same edit fixture:

1. a fresh runner applies the edit before any build;
2. a cache-prepared runner restores an exact immutable target generation compiled from the unedited base, applies the edit, and runs the same workload.

The base cache key binds runner OS/architecture, Rust 1.97.1, and the exact Actions source SHA. On a miss, the prepared job materializes and saves the unedited base before applying the fixture; later unchanged runs restore that immutable generation directly. Receipts are uploaded for both workload results. Queue, checkout, toolchain installation, cache transfer, workload, and end-to-end job times remain separately observable in the Actions run.

The first hosted distributions should be appended here after repeated unchanged runs. Node 22/24 is not a benchmark dimension for this Rust workload; earlier Node 22 evidence only discriminated one Scrapbook environment incident.

## Next decisions

- Close or explain the remaining 0.307-second seeded-private versus ordinary-native gap before
  claiming a warm Rust-loop win.
- Measure a leaf-function behavior edit and a public-type edit; the documentation edit mostly measures crate/test relink and namespace overhead.
- Add Python and current-project Node fixtures with repository-owned validators instead of pinning the product direction to legacy Node versions.
- Repeat each promoted arm enough times to publish p50 and p90, then add 1/4/8 concurrent-task fan-out with physical disk growth and cleanup latency.
- Keep semantic test selection and exact reusable verification receipts separate from filesystem/cache residency; they can compose, but neither authorizes the other.

## Controlled big-red fan-out harness

`scripts/benchmark-hot-state-fanout` runs one fixed physical window for the frozen Rust workload.
It has four closed arms—`ordinary-native`, `private`, `overlay`, and `private-copy`—and accepts only
fan-out widths 1, 4, or 8. It assigns disjoint Linux CPU-affinity sets from the harness's current
affinity, configures Cargo jobs to match each set, primes the state required by the selected
treatment, applies the exact checked-in edit to every task, starts every validator in one bounded
window, and always attempts to remove its exact worktrees and state. This is CPU enforcement, not a
memory reservation; per-task memory remains host-default and is reported from the child receipts.
The harness cannot accept an arbitrary command, repository, commit, fixture, cache path, or
validator.

Source creation is a separate closed treatment so it can be composed with one unchanged hot-state
arm. `--source-materialization git-sequential` is the typical control: it creates each task with a
separate sequential `git worktree add`. `glaeda-ordinary` invokes the Glaeda fan-out materializer
once but requests ordinary Git worktrees, isolating orchestration from filesystem copy behavior.
`glaeda-reflink` invokes the same program and requests exact same-HEAD reflink fan-out. The reflink
treatment rejects any ordinary fallback, changed Git proof, task-count drift, or receipt-shape
drift instead of silently changing the mechanism under test. Glaeda treatments bind the exact
release-binary digest as well as its checked-in CLI and library source digests.

After an unprivileged OverlayFS mount is gone, the kernel may leave its internal work directory at
mode `000`. Cleanup restores owner traversal only on owned, non-symlink directories inside the
exact disposable experiment tree, then removes that tree. It does not follow links or alter an
external target. Any ownership mismatch or remaining removal error still retains the tree as
`recovery_required` evidence.

Inspect the exact plan without mutation:

```bash
scripts/benchmark-hot-state-fanout --plan --arm private-copy --fanout 4
```

Run one physical ext4 window with the aggregate receipt outside the disposable scratch root:

```bash
scripts/benchmark-hot-state-fanout \
  --arm private-copy \
  --source-materialization git-sequential \
  --fanout 4 \
  --scratch-root /path/to/owned/ext4-scratch \
  --output /path/to/receipt.json
```

Every arm now executes through a schema-v4 `hot-run` measurement. The ordinary-native control uses
the task worktree directly with an explicit `target:native` observation; it receives no mount,
copy, state, or isolation treatment. Prime and edited work get different caller-owned comparison
keys. Each key deterministically binds the frozen source/tree/diff, exact producer-program and
source content digests, exact Rust and Cargo versions and executable digests, arm, source
materialization treatment, fan-out, Cargo concurrency, CPU-affinity sets, memory treatment,
offline/incremental settings, page-cache declaration, and creation umask. This lets repeated
same-treatment receipts feed `hot-pressure-shadow` while mixed work or treatment refuses.

The aggregate observation binds the harness commit/tree, frozen source/tree/edit, arm, fan-out,
exact CPU-affinity sets, per-task Cargo concurrency, setup and complete-window latency, every
semantic and `hot-run` receipt, mount/device/filesystem identity, backing-filesystem free bytes,
per-tree logical bytes and summed allocated file blocks before/after the window, and cleanup
disposition. Summed `st_blocks` are not unique physical usage on reflink-capable filesystems; the
filesystem-level observation keeps that distinction visible. A failed task cancels the remaining
process groups. The child environment is a closed allowlist: caller target overrides, compiler
wrappers/flags, and toolchain overrides are excluded; the accepted Cargo home is offline and held
constant. Resident priming remains setup because the treatment assumes an already-hot project.
The receipt separately reports resident-ready task-known to final trustworthy result as source
materialization plus checked fixture application plus the complete edit/validation window.
Byte-observation time remains outside that duration and is reported separately. The default
page-cache state remains uncontrolled/resident. For
`overlay` and `private-copy`, `--page-cache-treatment resident-target-dontneed` adds the bounded
cold-read discriminator: after the resident prime and byte observation, it fsyncs every exact owned
regular file in resident `target` and issues `POSIX_FADV_DONTNEED` immediately before the edit
window. The receipt records the file/byte scope and elapsed setup time, while naming this as advice
rather than claiming a globally cold cache; it never writes `/proc/sys/vm/drop_caches` or touches
unrelated trees. The harness grants no policy, cache, result-reuse, cleanup, or shared-mutation
authority.

`--retained-reuse` adds one immediate second validation window without recreating worktrees,
reapplying the edit, or replacing task state. `--retained-reuse-windows 3` or `7` extends that into
a bounded stable-lineage sequence; the two options are mutually exclusive and the single-window
flag remains the compatibility spelling for count one. The first edit and retained-reuse windows
have different comparison keys. First use binds `initial_for_source_state`; reuse binds
`retained_after_accepted_edit` plus its one-based ordinal. This prevents a pressure comparator from
treating first-use, reuse one, and reuse three as the same treatment. For `private-copy`, the first
window must prove a positive-time `seeded` preparation and every reuse must prove zero-time
`reused`; either mismatch rejects the observation. Every window still executes and validates the
complete frozen 1,343-test workload. It is therefore a command-reuse measurement, not result
reuse. Results retain `retained_reuse_window` as the first-window compatibility view and add the
complete ordered `retained_reuse_windows` sequence.

### First exact-head matched fan-out control

The first physical use of the harness ran `A-B-B-A` at fan-out 1 on big-red, where `A` was
`ordinary-native` and `B` was `private-copy`. All four observations bound exact clean harness commit
`e911d69947ba62661f085b51235ef304f8d1d250`, ext4 mount ID 44, device 259:2, the frozen workload
above, Rust/Cargo 1.97.1, and one 16-CPU affinity set. The harness held the child environment and
Cargo concurrency constant. Page cache remained resident/uncontrolled and memory remained
host-default unbounded as declared by the experiment. No competing build or test process was seen
at the initial observation; initial host load averages were 0.71, 1.03, and 1.30.

| Order | Arm | Complete edit window (s) | Inner workload (s) | Private-copy preparation (s) | Command after preparation (s) | Peak RSS KiB | Result |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| A1 | ordinary-native | 13.841862 | 13.74 | n/a | n/a | 1,637,572 | green |
| B1 | private-copy | 12.933524 | 12.26 | 0.513091 | 12.362230 | 1,633,464 | green |
| B2 | private-copy | 17.180191 | 14.79 | 1.403091 | 15.245866 | 1,633,792 | green |
| A2 | ordinary-native | 13.992653 | 13.86 | n/a | n/a | 1,636,552 | green |

The two-sample complete-window medians were 13.917258 seconds for ordinary native and 15.056858
seconds for private-copy. Private-copy was therefore 1.139600 seconds, or 8.19%, slower in this
small matched control. Its inner-workload median was 13.525 seconds versus 13.800 seconds native,
but its median `hot-run` command time was 13.804048 seconds and median first-use preparation was
0.958091 seconds. One candidate window beat the native median by 7.07%; the other missed it by
23.45%. That spread is evidence against a default change from two samples, not evidence for picking
the favorable sample.

The native tasks grew from about 2.042 GB allocated after prime to about 3.247 GB after the edit.
Each private-copy task kept the roughly 2.042 GB resident parent and allocated another roughly
3.236 GB private state because same-filesystem ext4 reflinks are unavailable here. Summed file
blocks are not a unique-filesystem accounting claim, but the result confirms that ordinary-copy
seeding is neither a latency nor a space default on this host at fan-out 1. It remains a useful
fallback mechanism, while reflink-capable private lineage, OverlayFS source views, read-only
dependency state, private-empty state, and ordinary native worktrees remain distinct path-class
choices.

Every run reported semantic acceptance, 1,343 executed tests plus one existing ignored test, and
cleanup disposition `removed` with two of two owned worktrees removed. The scratch root was empty
after the sequence. Raw receipt SHA-256 digests, in run order, were:

- A1: `302569edb2166941fb6da2f0ebdc8b58a2614c440a9a8b87692a97835d15d53d`
- B1: `8ff52deaaa6303070fc7595f375bc2d7f69d5b824f4a382ad2d01c5dcdea9ad5`
- B2: `318840c763dca386da533a3e32bf74759fd9257cf7189c47f479e8b24f2170c7`
- A2: `291694d81fe49c7d2d5dac962724508567e093991990fed2239a6e4a8f1c78a7`

This result closes only the single-task warm-page-cache discriminator. Fan-out 4/8, cold-read
behavior, and composed Python/current-Node path policies remain separate experiments.

### Fan-out-4 warm and cold-read selection

The next bounded big-red sequence closed the fan-out-4 write-heavy `target` choice. Every window
used ext4 mount 44, four disjoint four-CPU sets, 16 total Cargo jobs, the same frozen edit and
1,343-test validator, and complete cleanup. Warm receipts used accepted head `384388e`; the
cold-read treatment itself used exact clean implementation commit `e6923237e0acbf24fc39a4e606a51a508f7c4d83`
and tree `aa8369e4711bc1c79f84d41f9c6a07c32f60f674` in A-B-B-A order. The producer-program change and
page-cache treatment intentionally give warm and cold receipts different comparison keys.

| Treatment | Complete-window samples (s) | Median (s) | Median peak temporary growth |
| --- | ---: | ---: | ---: |
| ordinary native, resident/warm | 26.474802 | 26.474802 | 10,934,538,240 bytes |
| private-copy, resident/warm | 27.928636, 28.288224 | 28.108430 | 12,890,456,064 bytes |
| Overlay, resident/warm | 30.295675, 29.632275 | 29.963975 | 12,178,305,024 bytes |
| private-copy, resident-target cold-advised | 28.340464, 27.281826 | 27.811145 | 13,028,327,424 bytes |
| Overlay, resident-target cold-advised | 32.096016, 30.938088 | 31.517052 | 12,179,146,752 bytes |

Each cold setup fsynced and advised away 1,119 exact resident-target files representing about
2.028 GB logical / 2.031 GB allocated. Advice took 2.750–2.769 seconds outside the primary window.
Private-copy first-use preparation rose from a 0.955072-second warm median to 1.681559 seconds
cold-advised, evidence that the treatment removed a meaningful warm-read advantage. Its complete
window remained within ordinary run variance, while Overlay's median grew 1.553077 seconds / 5.18%.

Under cold advice, private-copy reached all four useful results 3.705907 seconds / 11.76% sooner
than Overlay. Under warm pages it was 1.855545 seconds / 6.19% sooner. Overlay saved 849,180,672
bytes / 6.52% of private-copy's cold peak temporary growth, but that capacity difference does not
outweigh the latency result for the default write-heavy `target` path. Ordinary native remains the
fastest same-worktree observation when isolation is unnecessary; source and suitable dependency
paths remain separate Overlay/read-only decisions. The implicit cross-worktree `target` selector
therefore moves to `private-copy`, which also becomes cheap CoW seeding on reflink-capable XFS.

All four cold receipts succeeded, accepted all semantic validators, observed four simultaneous
tasks, removed five of five worktrees, and left the scratch root empty. Receipt SHA-256 digests in
run order:

- private-copy A1: `b37e2f1223750c5aa9e81a7dc3ecbbafbdaf9b40093dbe0277d1697f6b028cf0`
- Overlay B1: `9f8ed0484a2311c57d24a83c95783564ea8b7bf67683653c03e352d94e8da2b4`
- Overlay B2: `cbf8ee5adb63d42670b5fc78fa933fe778ac7891e23254be64c87a9f259c2db6`
- private-copy A2: `dcf52a4e16de43bbf3e8714eebcea703933eb6fa4252dd13d23df4a4c2d7de1c`

### Immediate retained-reuse discriminator

Candidate `8fb5232fe38e7dc8d7fe5103c7c28b75b4aed0ad` / tree
`98a60e35cfa651a6d5797ccf97399c561fc926a2` added and physically exercised the distinct
retained-reuse window on big-red. Six runs used ext4 mount 44, fan-out 4, four disjoint four-CPU
sets, 16 total Cargo jobs, resident/uncontrolled page cache, and the same edited source plus complete
1,343-test validator in both measured windows. The two-sample controls were native, private-copy,
and Overlay; run order was native, private-copy A1, Overlay B1/B2, private-copy A2, native.

| Arm | First-use samples (s) | First median (s) | Retained-reuse samples (s) | Reuse median (s) | Reduction from first median |
| --- | ---: | ---: | ---: | ---: | ---: |
| ordinary native | 25.872505, 27.227278 | 26.549892 | 17.996679, 17.846947 | 17.921813 | 32.50% |
| private-copy | 28.038855, 26.975499 | 27.507177 | 18.452164, 20.805015 | 19.628590 | 28.64% |
| Overlay | 29.582546, 29.279602 | 29.431074 | 18.199325, 17.942500 | 18.070913 | 38.60% |

First use reproduced the prior direction: private-copy reached all four results 1.923897 seconds /
6.54% sooner than Overlay. The immediate-reuse samples reversed that ordering: Overlay's median was
1.557677 seconds / 7.94% sooner than private-copy. Ordinary native remained the no-isolation
control; its reuse median was only 0.149100 seconds below Overlay, which this experiment cannot
resolve as a meaningful difference. Two samples do not justify changing the path selector. They do
show that expected lineage lifetime belongs in the next selector experiment rather than assuming
the first-use winner is also the steady-state winner.

Every private-copy first window proved `seeded` preparation in 0.863135–1.033674 seconds; every
reuse window proved `reused` with exactly zero preparation time. Observed allocated file blocks
grew between first and second windows by 16–20 KiB native, 64 KiB private-copy, and zero in both
Overlay samples. All 48 measured task validators were accepted, all windows observed four
simultaneous tasks, all six cleanups removed five of five worktrees, and the scratch root ended
empty.

Host aggregate CPU PSI `some` fractions were 1.24–1.99% during first use and 0.46–0.95% during
reuse. I/O PSI `some` was much larger and variable: 8.67–16.32% first use and 13.08–21.45% reuse.
The receipts therefore support the large within-lineage reuse reduction, but not fine-grained
sub-second ordering. A longer retained sequence with more repetitions should decide whether
Overlay's apparent steady-state advantage survives quieter I/O and later commands.

Receipt SHA-256 digests:

- native A1: `026bd905d9ccb128a244c003a8bbd463ea75cddc224f355476896c6b2c3fa6c9`
- private-copy A1: `8570318d2a946d3da11750b1aecd0e25d27d4b932ccc898bd007409fa8040dc5`
- Overlay B1: `e1bc3b0f50e8ca577a2ce2a6a38af1746fc3c5f8af8492dd82d48b9bc5b57b16`
- Overlay B2: `df4d3130a4a994d6e86333dd2469c6a3a5d887f5042e3a7ff25087f4dee525ec`
- private-copy A2: `2fa4216756108b90c1b35f6bbbd50092cf8e2517a618ee470e8a49a05600ceeb`
- native A2: `ea08811d6e93ef595ab94479457f6f6765bc54abedabdad053a0a3d67484696e`

### Stable-lineage retained sequence

Exact clean candidate `e57c049fe22363393a2b228d6f0fa541bf173328` / tree
`1d16068de71d74070bb24187aa0172f72b70aff6` extended the compatibility window into bounded one-,
three-, or seven-reuse sequences. Every ordinal receives its own comparison key, while the legacy
single-window result remains the exact ordinal-one view. The physical controls kept the same ext4
host, source, edit, validator, fan-out 4, four disjoint four-CPU sets, 16 total Cargo jobs, and
resident/uncontrolled page-cache declaration.

The first A-B-B-A bracket ran three reuses for native, private-copy, and Overlay. Its two-sample
medians were:

| Arm | First use (s) | Reuse 1 (s) | Reuse 2 (s) | Reuse 3 (s) | All-reuse median (s) | Reuse range (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ordinary native | 26.552257 | 18.222615 | 18.577368 | 17.875250 | 18.249267 | 17.700468–18.604845 |
| private-copy | 28.284841 | 18.200026 | 18.152647 | 18.353079 | 18.200026 | 17.902273–18.755589 |
| Overlay | 30.283737 | 18.001930 | 18.102296 | 17.796089 | 17.822915 | 17.748304–18.403516 |

Private-copy reached first use 1.998897 seconds / 6.60% sooner than Overlay. Overlay's median was
0.198096, 0.050352, and 0.556990 seconds lower at the three reuse ordinals, but the ranges overlapped
and private-copy still completed the cumulative first-use-plus-three sequence 1.193459 seconds
sooner. Ordinary native remained the fastest no-isolation first-use control and converged into the
same roughly 18-second reuse band.

Because that result predicted a possible later crossover, a second A-B-B-A bracket compared
private-copy and Overlay through the supported seven-reuse bound. Each entry below is the median of
two complete lineages; ordinal zero is the edited first use.

| Completed through ordinal | Private window (s) | Overlay window (s) | Private cumulative (s) | Overlay cumulative (s) | Private cumulative lead (s) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 27.458494 | 29.609687 | 27.458494 | 29.609687 | 2.151193 |
| 1 | 18.828968 | 18.150454 | 46.287462 | 47.760140 | 1.472679 |
| 2 | 17.975798 | 18.497924 | 64.263260 | 66.258063 | 1.994804 |
| 3 | 18.302575 | 18.223315 | 82.565834 | 84.481378 | 1.915544 |
| 4 | 18.277601 | 18.777487 | 100.843435 | 103.258865 | 2.415430 |
| 5 | 17.876091 | 18.521902 | 118.719526 | 121.780767 | 3.061241 |
| 6 | 18.374546 | 18.274976 | 137.094071 | 140.055743 | 2.961671 |
| 7 | 18.427896 | 18.052837 | 155.521968 | 158.108580 | 2.586612 |

There was no measured crossover. Private-copy stayed cumulatively faster after every ordinal and
finished the eight-command lineage 2.586612 seconds / 1.64% sooner. Its pooled 14-window reuse
median was also slightly lower at 18.224005 versus 18.328246 seconds. Individual lineage totals
overlapped—154.640634 and 156.403301 seconds private-copy versus 156.115406 and 160.101753 seconds
Overlay—so this is a bounded ext4 selector result, not a generic filesystem speed claim.

Overlay allocated a median 11.251150 GiB of task state versus 12.056736 GiB for private-copy, a
0.805586 GiB / 6.68% saving. Overlay showed zero additional allocated state through ordinal seven;
private-copy's median growth was 64 KiB. Host-aggregate I/O `full` PSI fractions remained large and
overlapping: medians were 15.62% Overlay and 16.83% private-copy, with maxima 19.16% and 19.90%.
CPU `some` PSI medians stayed below 0.7%. The cumulative ordering therefore decides only the tested
path policy: retain private-copy as the write-heavy cross-worktree `target` default through the
observed first use plus seven replays; do not introduce a speculative lifetime switch. Overlay
remains preferred for mostly-read source/dependency views and when its measured space advantage is
the controlling constraint.

The three-window bracket accepted all 96 measured task validators; the seven-window bracket
accepted all 128. Every validator executed the same 1,343-test workload, every window observed four
simultaneous tasks, all private-copy first preparations were positive-time `seeded`, all 80
private-copy reuse preparations were exact zero-time `reused`, every cleanup removed five of five
worktrees, and the scratch root ended empty. Receipt SHA-256 digests:

- three-window native A1: `1ce830ed290917674aa0a8e94e32dd411f32ba03358c9af2db17d5ab989ccf61`
- three-window private-copy A1: `603d76244b41c5d68764e8cacd953638eb7efc0f2ccbb7051856483e0c88faa8`
- three-window Overlay B1: `a9b8ea3b1038a9cafa52767157da3d9f617801e554575910e8b9640c74beb870`
- three-window Overlay B2: `bcd4ab981447597a0abb22f73f9122381127ecc150d1e4e001d3720e0ab0b442`
- three-window private-copy A2: `9033f0d9ce639fc4532f41937a789e408ef201fa5cf28f262a2b352424ee7d15`
- three-window native A2: `9186e58df7359c893a7f34c35b17fbb2b7a7c5735016d29c7d30a2f1fd72d315`
- seven-window private-copy A1: `368faf31cc9895268fdc2dc94d805d92db91d5dae8e616409c8537d4723b1b2e`
- seven-window Overlay B1: `5cec1b6b0f6c8693701bbd47f724be7209d7f53226bd6f5b1d3d70c11542b6b1`
- seven-window Overlay B2: `4dcdefef81329972aefbef8b90accfc6e0cea3010a2dcb73687908a24ef44c05`
- seven-window private-copy A2: `55d4b0b155cda1468c971ecec907e352297604fe8904cf4d90dcb10f966420dc`

## Current-source inventory and native/private-copy controls — 2026-08-30

Exact clean physical candidate `067328cf2d2f883113fd90fb109104b69cd1bfe8` / tree
`549f4bb70f39e8cb9e402a73e4a584a04661d1bc` was exercised on big-red's native ext4 filesystem
with Rust/Cargo 1.97.1, 16 logical CPUs, and four Cargo jobs. All twelve benchmark receipts derived
the same current-source inventory from three Rust terminal summaries: 1,360 selected, 1,359
executed and passed, one ignored, 16 filtered, and zero failed or measured tests. The exact command,
source, exclusions, and test-inventory source are receipt-bound. “Private-empty” below means an
empty Cargo target with the host's already resident source/toolchain/dependency state; it is not a
cold machine or cold package-cache claim.

| Control/treatment | Samples (s) | Median (s) | Median peak RSS | Relative to private-empty |
| --- | ---: | ---: | ---: | ---: |
| native private-empty target | 46.43, 46.90 | 46.665 | 2,425,398 KiB | control |
| native immediate target reuse | 2.63, 2.61 | 2.620 | 41,146 KiB | 17.81x faster |
| isolated private-copy first use, outer | 10.24, 10.92 | 10.580 | 1,591,572 KiB | 4.41x faster |
| isolated private-copy retained reuse, outer | 2.87, 2.86 | 2.865 | 79,320 KiB | 16.29x faster |

The private-copy first-use treatment split into 0.641756 and 1.222349 seconds of seeding (median
0.932053) plus 9.499932 and 9.604233 seconds in `hot-run` (median 9.552083). Its inner workload
median was 9.400 seconds. Retained reuse performed exactly zero preparation, with a 2.782687-second
`hot-run` median and 2.650-second inner workload median. The retained isolated inner workload was
only 0.030 seconds / 1.15% above native warm; including the wrapper, retained isolation was 0.245
seconds / 9.35% above native warm.

The resident target occupied 2,017,067,008 allocated bytes. Reconciled task-private states occupied
2,741,698,560 and 2,741,686,272 bytes: median overhead 724,625,408 bytes / 35.92% on ext4. Every
timed child tree exited zero and reported zero swaps. The resident target had been compiled at the
native worktree path, while private tasks executed through `hot-run`'s stable task mount. Because
ordinary-copy preparation was under one second but first execution remained about 9.55 seconds,
the first interpretation attributed the gap to path-context reconciliation. The later causal
discriminator below refutes that explanation: `hot-run` already presents the identical absolute
project path, while ordinary post-build worktree mtimes alone reproduce the rebuild.

The selected policy remains: native warm is the fastest no-isolation same-worktree path and
private-copy provides near-native retained performance for cross-worktree isolation. Do not add
path remapping from this superseded hypothesis; preserve freshness only through the seed-bound,
exact-content treatment below.

Raw receipt SHA-256 digests:

- native private-empty A/B: `fc8acb4b670745d836b04aaf05c9a6fc6c426b0577b22a15289b8de05b1c89f7`, `90daf53040212f29f5c667b1e3cfdec5473d477e74f1ce7e1003ef5e35e0130b`
- native warm A/B: `e9dcc2f9175f775141cd75b9d641b5440c7293e32bffa78ac04019bb6625a1f6`, `e59ca9c1882033c77c3525867ab68493c5b7fd6f0723cd41a1ba05f507877b75`
- private first-use benchmark A/B: `c87846f5ec6b6aa4398f77b63f27b30c706363d99acdbd1e29c4c0a0ec88d173`, `7248bdab82415bff5b846cd3bcea0ef3310979dfd2ab21735d66315e7b00d761`
- private first-use `hot-run` A/B: `848aa1f073aba131951c9d43e8b6c9d7907468bd860f5405f4cc4968f86873ac`, `ee59516c9f2171ddb8835d1df1893e25ab8a0482b6bf26ec8b2243365c64fe1b`
- private retained benchmark A/B: `2b47cdd37c717a1a1eba3716f9037785bb5103f7e9999ac9cf1aaf615aa0654c`, `33c73747b2a75348edf743411d7646c30d77e4af1b41885219bf06bed4f5e3bb`
- private retained `hot-run` A/B: `e8cf2c687cebba48e756083696f9874e70eed5f465932213d98ccfb047fe8a6c`, `659b1e229c31fbb5c272187b3f125f896963e4066228ebd9ac1938262af1e4cc`

### Seed-only source freshness discriminator

Exact workload source `b4ac5b64c7b390acd24006bd3fd90f18b74ac348` / tree
`5a5f6c444e17a293a04a4ac24edd4da6fa0f9983` was primed on big-red ext4 after one task worktree
already existed. That older task then reused a copied target in 2.878737 seconds with no dirty
Cargo fingerprint. An otherwise identical task created after the prime took 9.453875 seconds.
Cargo's own fingerprint log named newer task source mtimes than both copied dep-info references as
the cause. Giving 493 byte-identical tracked regular files their resident mtimes restored a
2.883836-second command and zero dirty fingerprints. The absolute project path was identical in
all three arms.

Candidate `d0ae2c59b98a44a4a65a08d3b8a0f0b42b6d45dc` / tree
`838c847b3f9232a515866a5361b4594f4774a351` implements that treatment only when a task-private
`target` is first copied. It compares contents and executable mode through held, beneath-root
no-follow descriptors, rejects multiply linked task files and files that move or change during
comparison, changes only the task-private inode's mtime, and records path-free counts/time in
hot-run schema v5. The final interface requires the
explicit `--seed-source-mtimes` treatment after caller-owned exact warm-parent proof; the flag
creates no proof or cache authority. A retained lineage performs no source normalization: a file
reverted after a prior task build must remain newer than the retained output so Cargo rebuilds it.

The physical A-B-B-A bracket kept the exact workload source, resident target, Rust/Cargo 1.97.1,
four Cargo jobs, stable project path, semantic validator, and fresh post-prime worktree timing
constant. A used the landed schema-v4 producer without source preparation; B used the exact
candidate above.

| Arm | Command samples (s) | Command median (s) | Preparation median (s) | Seed-to-result median (s) | Median peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| ordinary post-build worktree | 9.451920, 9.556124 | 9.504022 | 4.538835 | 14.042857 | 1,601,956 KiB |
| seed-only exact-content mtime | 2.826547, 2.783282 | 2.804914 | 6.167517 | 8.972431 | 79,330 KiB |

The candidate reduced command wall by 6.699107 seconds / 70.49% (3.39x) and seed-to-result wall by
5.070426 seconds / 36.11%, despite unfavorable and variable ordinary-copy time in the candidate
positions. Command peak RSS fell 95.05% because Cargo executed the tests without recompiling the
crate. Both candidate samples normalized all 493 tracked paths in 0.107179 and 0.103555 seconds;
both produced zero dirty fingerprints. Every arm derived the same three terminal summaries: 1,371
selected, 1,370 executed/passed, one ignored, 16 filtered, and zero failed/measured tests. The
same exact resident control took 35.16 seconds to reconstruct and 2.66 seconds on immediate native
reuse.

After composing the explicit gate with merged protected-cache store main, exact clean producer
`e9b40406e17affd52f09622963021824ce81d6a3` ran one same-producer pair. Without the flag it
reported four dirty units, an 8.698599-second command and 11.722752-second seed-to-result total.
With the flag it normalized 493/493 files in 0.144376 seconds, reported no dirty unit, ran the
command in 2.780546 seconds, and completed seed-to-result in 5.178758 seconds: 68.03% and 55.82%
lower respectively. Exact retained reuse took 2.727571 seconds with zero preparation and
`retained_state_unchanged`. All three results passed the identical 1,370-test validator.

The semantic controls remained fail-open to real work but fail-closed to stale reuse:

- a fresh task containing the frozen `src/lib.rs` edit normalized 492 files, classified one as
  different, and rebuilt only from that changed source; its command took 10.601218 seconds and all
  1,370 tests passed;
- editing an already retained lineage took 11.013320 seconds with zero source preparation and
  Cargo named `src/lib.rs` dirty;
- reverting that file on the same retained lineage took 10.351173 seconds, again with zero source
  preparation and `src/lib.rs` dirty, proving the prior edited output was not reused;
- clean retained reuse took 2.772691 seconds with exact `retained_state_unchanged` evidence.

The post-run resident target occupied 9,548,783,616 allocated bytes and the task states occupied
roughly 9.11–9.55 GB each. Those are a capacity warning, not unique-byte accounting: ext4 ordinary
copies remain the dominant complete preparation cost and stale artifacts were not separated in
this run. XFS reflink composes directly with the freshness fix by making the same seed CoW-cheap;
the separate protected-generation catalog/store and an eviction/reconstruction policy must decide
which target generation is worth retaining or copying.

Raw receipt SHA-256 digests:

- ordinary A/B hot-run: `e7b43f67ec955ac46fbaee5ba33e178eeb74c075593458a113432d1d1e643545`, `edf7378bc82a24dfebe66549e3466bb13e6325f3d4469e06ed30a3bccedce382`
- ordinary A/B benchmark: `5c0a38ff5596aea10586d9b8312922a88328da655b8ef59584f332e170e31f73`, `d2b8185b9eaf17a02d1c7af026f192d81a78a4a443f66976ef5c08d5e6cddf86`
- candidate A/B hot-run: `184909e61bba9c1e2a8926ca08d8093a4d1d0341208ebe64ef8a9512ab5243b7`, `cf46d2b8154fee73d0de626ebd6d37506e91e2653d2a44ffbe07f44fed2e7687`
- candidate A/B benchmark: `3936c59890cf3f705f9a1ac89a2251e9c37ea6698349daf8bb4e66de9590cbc4`, `3971514d8ce110a15033279c30cfbdcf52bbf6ea887a411b31322b2c8efbe6a4`
- clean retained hot-run/benchmark: `4132b9fffcee562f8cf251a293d46ae96873586d0116e7ec64316f74f5907e49`, `9f9bbd82bf4e1f61ab2e16c522a27e5e0bb42d5b3e10346cd63bf521b184fe98`
- retained edit hot-run/benchmark: `0619c614a80c7357244ac6ee93f5acf7d727a0be59f712dd5b8a3067a6c9df12`, `83491b09cada3eb0323d4b2239c14c53a06262212c4d196f324c880dd6d05ca6`
- retained revert hot-run/benchmark: `33d807ac320a90747b14067a289edba2f2cf7518dd08ec86240adfa68442d42b`, `7b1ab215af81819b912d8e084a3ca17aa58a543795a0dbe68d3407ecde04ff74`
- edited seed hot-run/benchmark: `d29459936348a0739814a9ff595072297af1b51cb1d160f0498c20d1ce4b4cf9`, `94dabe09bb2fface339ca3d8d7a786d0758998071d5f84c00f2bf7c7a7dc2302`
- final same-producer control hot-run/benchmark: `5c56371de54b921acd81eeba9346e1ea29e834c5d6f0ff76e0b9add999e8c5ed`, `d38ba656c505338929a363775f9f90c7cb51479112aa01e8a019ea89a1077071`
- final same-producer candidate hot-run/benchmark: `0266d7ff6ebbb3aa21eed1250017187c828ebc77be02ddecc8d6f5e1f7e6e15d`, `9cefe858dbe997d34677d3af57be4b0962b7909ee81bcca24f4110449a08fbae`
- final retained candidate hot-run/benchmark: `4561f17b0f81ad04f8aec366db5f942ab05c87f66d9befeaf6e281609414b335`, `95a6e0aa78653e574bdbd5162e7dc12b36c4165ee6080cc2115e0cec89ada0ef`

## Composed XFS source and compiler-state loop — 2026-08-30

Exact clean producer `decd8ea38f3e6d8df5b91897df980175f7db99a9` / tree
`7789bf78967d708c5fd0bbcc80c84c435bf998bd` composed task source materialization with the existing
`private-copy` compiler-state arm. Its release `glaeda-reflink-task` was 1,799,784 bytes with
SHA-256 `ff1b9081ab20eb00eac946bf160efc0e01caf49a17a530f39a0a0c6f40e7851c`.
The physical backend was a route-owned 32 GiB loop image on big-red Ubuntu 26.04, kernel
7.0.0-30-generic, XFS/xfsprogs 6.18, `reflink=1`, 4 KiB blocks, and `noatime`. The frozen source was
`b9fa23462420c13a465d635d9694f0c827c1e685` / tree
`edd0b7bb9d3e59305c21c69b721b5278d8aff6da`: 475 tracked regular files and 9,285,337 logical bytes
per task source.

The source controls changed one dimension while retaining the same resident prime, checked fixture,
private-copy target lineage, pinned Rust/Cargo 1.97.1, disjoint 16-CPU grant, and complete semantic
validator:

- `git-sequential`: one ordinary `git worktree add` process per task, in sequence;
- `glaeda-ordinary`: one bounded Glaeda fan-out invocation using ordinary Git worktrees;
- `glaeda-reflink`: the same Glaeda invocation requesting reflink fan-out, with any fallback rejected.

The fan-out-1 smoke demonstrated that reflink proof is not free. Source creation was 48.435 ms for
sequential Git, 67.642 ms for Glaeda ordinary, and 99.607 ms for Glaeda reflink. Resident-ready
task-known-to-result was 14.947136, 15.725905, and 15.353203 seconds respectively. Outer wall was
61.30, 64.92, and 65.05 seconds; GNU-time outer maximum-RSS observations were 2,503,464, 2,447,652,
and 2,494,540 KiB. The candidate should therefore not replace the simple width-one path for speed.

The primary fan-out-4 bracket ran A-B-C-C-B-A, two samples per treatment:

| Source treatment | Source samples (ms) | Source median (ms) | Edit-window median (s) | Task-known median (s) | Outer-wall median (s) | Outer max-RSS median (KiB) | Peak XFS growth median (bytes) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential Git | 196.034, 176.616 | 186.325 | 26.660149 | 26.854811 | 74.885 | 2,501,790 | 7,263,135,744 |
| Glaeda ordinary | 88.363, 110.860 | 99.612 | 25.782925 | 25.891474 | 73.625 | 2,509,122 | 7,263,141,888 |
| Glaeda reflink | 81.510, 77.793 | 79.652 | 26.714207 | 26.801249 | 75.175 | 2,521,654 | 7,224,225,792 |

At width four, bounded Glaeda orchestration alone reduced source setup 46.54% / 1.87x versus
sequential Git. Reflink reduced it 57.25% / 2.34x versus sequential Git and another 20.04% / 1.25x
versus Glaeda ordinary. The candidate used 38,909,952 bytes / 37.107 MiB / 0.536% less peak XFS
growth than sequential Git, consistent with sharing four 9.285 MB source trees. The full
task-known medians differed by only 53.563 ms / 0.20% between sequential Git and reflink because
the 80–187 ms source step is small beside a 25–28 second compiler/test window. The differing
edit-window and outer medians are ordinary run variance, not evidence that source reflinks change
compiler semantics or performance.

One exploratory fan-out-8 A-B-C scaling bracket produced:

| Source treatment | Source (ms) | Task-known (s) | Outer wall (s) | Outer max RSS (KiB) | Peak XFS growth (bytes) |
| --- | ---: | ---: | ---: | ---: | ---: |
| sequential Git | 359.378 | 52.846806 | 102.35 | 2,506,268 | 12,551,753,728 |
| Glaeda ordinary | 120.495 | 52.448307 | 103.53 | 2,514,088 | 12,551,704,576 |
| Glaeda reflink | 100.159 | 49.869747 | 97.07 | 2,503,764 | 12,469,665,792 |

Glaeda ordinary was 2.98x and Glaeda reflink 3.59x faster than sequential Git for source setup at
width eight; reflink also used 82,087,936 bytes / 78.285 MiB less peak XFS growth. The candidate's
2.977-second / 5.63% full-loop advantage is not promoted because this scaling bracket has one
sample per treatment and no reverse-order repeat.

All 12 physical runs succeeded. Every one of 51 task validators passed the same 1,343 executed
tests with one ignored and zero failures; all 17 requested reflink tasks reported `reflinked`, zero
ordinary fallback, the exact commit/tree, and the final per-task Git proof. Cleanup removed all 63
registered worktrees, the XFS scratch root was empty after every run, and no benchmark worker or
failed unit remained. The outer RSS observations above are process-lifetime GNU-time maxima, not
concurrent aggregate memory.

Logical cleanup did not reclaim sparse host backing by itself. After the runs, the empty mounted
XFS image still had 16,189,345,792 allocated host bytes. One explicit `fstrim` reported the full
32 GiB range trimmed and reduced host allocation to 4,094,177,280 bytes: 12,095,168,512 bytes /
11.265 GiB / 74.71% reclaimed. The complete trim-and-reobserve command took 0.13 seconds. The loop
device advertised 4 KiB discard granularity, 4 GiB maximum discard, and `DISC-ZERO=0`; XFS still
reported 690,143,232 logical used bytes. This proves task/worktree deletion and host-backing
reclamation are separate lifecycle phases. A persistent sparse project disk should record both,
and should test bounded trim at idle/eviction watermarks rather than issuing it on every task. The
remaining backing allocation is observed, not assumed reclaimable.

Raw aggregate JSON / GNU-time SHA-256 pairs:

- fan-out 1 sequential/Glaeda ordinary/Glaeda reflink: `25133022cf2bf9038f6d903fdacdc28a8b9533e5b88b12fa420464da30d1bf3d` / `8bc9ef53609e80586d330ad9398603b648ec265b6eb0a03fcd9104de3667d3bc`, `2df1c4c70e5a4141f2193de82d70aaa530245704de55b5da1f01fcf97914fe8d` / `9a1e4f62f6badfacd966373779953ef269f2c50001c1cc1e3eb5d3eb50b5bfd7`, `25e7a4e8a90cc54994617c5a30bf93e59bdc0c8d4fc1c82be3448820a27766fc` / `41bf1d830d8ff85415668eb7c1e52b7c43fe310666afb419f322bab6474f9004`;
- fan-out 4 sequential A1/A2: `f2638f5ff83f6be3fc7e508a638692f59c023f14278c947fcfe6ec0786767bb8` / `772295ef12c227ec38a0211a864c47781bfa231879d754fdc8b1461e3bfa6f24`, `dad0f7459b9f43ffe063cf0e95c5e0eb851d050d31ea4aace9cdc47ab4ac8666` / `88f2f853cae5047c2ce0aeae6ff5ad8c4c0abe2befa4cac2aad807cad52c4e72`;
- fan-out 4 Glaeda ordinary B1/B2: `ef1c597c8f8076a87f5834296e4bd17ada8a8cb5a594ee1c935a65927cb8bb63` / `da455483cca1dc6ec9cb1403472d0bf7bd5f6df3f09fc995eacc940247588a0d`, `b6091157bf7e767bea4f2427300c8613e5c73cb9391ce206e387a9e0cd2ee82c` / `2d70f4abe45fdabc4cf4d64066a95dd853a8d46555015e0bc10f5694bf067772`;
- fan-out 4 Glaeda reflink C1/C2: `e33adc1528fa40182e16d1c572912f63e5f4d54a6cccf623a3ab3b95853f3fdc` / `f5870a74f30b5727ec4ce903e2706d360ac27b45b988b16372c0a08a8416e3f1`, `bf0bad735baa4037ab3f9b8f51f2fe1e4ad3259cbaa922a3bad09a1c92afbda5` / `beb94e13c69ad79aeab398772c94b90beb694f9202f8c914322c393ae5ff4a7d`;
- fan-out 8 sequential/Glaeda ordinary/Glaeda reflink: `c9b4f97e6c23c8037a89645eef02164617b3faf6e0cb1dd03ce0dfe6888ae116` / `52631d684e0bf6f36a5671b4a6be2dfe24ee96c850825f954574150abe15dc77`, `0556db8832354a6aa8b459fda382b005f4906e33b71e4963a8b2f09a7fd57973` / `a298dc61673c4d47f80016b9484041ac9e03211e9f02a88ab03f506bd598defd`, `ad977bfdaa04cb8f9402c6c970d44415c5fa3aae29803e6d17f39199b702af37` / `5de6dc03329b0f3bc68302db11e1b938782771dbd644987a7c15dc03c958ddcb`.

## Default hot-state generation discriminator — 2026-08-30

The same-path replacement control used a fixed linked-worktree pathname, the tiny
`examples:private-copy` cache, a route-owned XDG cache, and one marker written only into generation
A's private cache. Exact clean main `c31bf1066028df1ddccb2900ad041029e8becc6a` keyed default
state only by paths and cache declarations. Generation A reported `seeded`; after removing it and
recreating generation B at the identical pathname and commit, B printed `inherited`, reported
`reused`, and performed zero preparation. Command-plus-preparation took 5.865 ms and 4.143 ms. This
was stale cross-generation reuse, not a speed win.

Exact clean candidate `f635771abaef36a2d932219d3611078eb09d2e1e` added physical
worktree/Git-object identity and a stable linked-worktree pointer-file witness to the implicit
lineage, then held the validated task, Git, and cache objects through bubblewrap's FD-bound mount
interface. The backing filesystem immediately recycled the task `.git` file's device/inode
(`66306:2884673`) across the two generations, while its ctime witness changed. Generation B printed
`absent`; A and B both reported `seeded`, selected two distinct 0700 state roots, and took 5.243 ms
and 5.388 ms command-plus-preparation. An immediate third B invocation printed B's `second` marker,
reported `reused`, and took 5.727 ms with zero preparation. The discriminator therefore preserved
same-generation reuse while replacing an invalid approximately 1.7 ms shortcut with a fresh
private lineage. A deterministic bind-FD regression also atomically replaced the validated source
pathname before bubblewrap consumed it and proved the held generation—not the replacement—was
mounted, then proved the consumed descriptor was absent from the payload's `/proc/self/fd` view.
Explicit `--state` remained outside lineage selection so callers can deliberately own a
cross-generation lineage.

Raw hot-run receipt SHA-256 digests:

- exact-main control A/B: `84e8dcaaac372defc0b27a1c2a1d2c8145a09bdd44619c5447eee3a105226a27`, `aaa69dac28ba1bb4055a5f145e573b056ec58fb2f10863e388fed035dfbd1782`;
- exact candidate A/B/B-reuse: `cc982bedb9a6b4b46a8f178a19b3171093b8bad337d764aba2fe67f2d882ae22`, `837d887235bff7cf125a4fc7c32ac01e1f9698157d5b3a127e8aec61e548337f`, `91de07cc575d42403a4f64bba926baddf89aacc671a41941e14df1617e6b5f2f`.

## Bounded hot-run cache observation — 2026-08-30

Exact clean base `27081b365b8f867b96d994d0eddd6f6aff7cbcf9` could classify a hand-authored
cache inventory but had no producer for physical `hot-run` state. Exact code candidate
`e37b236c6c95f17f90af3836251b5aff337976c8` / tree
`c96bf541fbe87a8fd62774513229d32a6f323989` added one explicit-root, descriptor-bound,
metadata-only Linux observer. The physical fixture was big-red's unchanged ext4 hot-run namespace:
four opaque legacy states, 1,090,037,078 logical bytes and 1,093,828,608 allocated state bytes. The
namespace directory itself occupied another 4,096 bytes. One legacy Overlay state accounted for
1,093,472,256 allocated bytes. None of the states had enough ownership or lifecycle evidence to be
called reclaimable.

The complete comparison used three warmups and 30 measured samples per arm in rotating/reversed
four-arm order. All arms observed the same state. The primary metric was complete command wall
time; secondary metrics were process maximum RSS, byte agreement, terminal status, path-free
output, and mutation absence.

| Treatment | Validity | Median (ms) | p90 (ms) | Max RSS (KiB) |
| --- | --- | ---: | ---: | ---: |
| ordinary-user GNU `du`, apparent + allocated passes | invalid: both passes returned 1 on inaccessible Overlay work internals | 10.294 | 11.976 | 9,136 |
| privileged GNU `du`, apparent + allocated passes | complete metadata control | 20.701 | 24.107 | 9,136 |
| existing supplied-inventory classifier | valid classification only; manually supplied bytes, no producer cost | 1.381 | 1.863 | 5,072 |
| Glaeda explicit-root observer | complete one-pass observation plus classification | 8.091 | 10.307 | 6,684 |

The candidate exactly matched the sum of GNU `du`'s four per-state logical and allocated totals,
classified all four states `unknown`, reported zero reclaimable bytes, printed no private path or
child name, and left root/state atime, mtime, and ctime unchanged in the contract test through
`O_NOATIME`. It was 2.56 times faster than the complete two-pass GNU control while returning the
typed classifier result rather than raw paths. The 1.381 ms supplied-inventory arm is intentionally
not an end-to-end competitor: a human or separate producer had already paid for every observation.

A free hosted GitHub Actions arm is inapplicable to this decision because a hosted runner cannot
observe operator-local hot state without changing the storage and trust boundary. Reset is cold
reconstruction or selecting a new state, not deletion by this command. The observer has no catalog,
ownership, generation, non-use, reconstruction, retirement, or cleanup authority; later lifecycle
work must supply those facts before bounded eviction can exist.

## Definitely-held hot-run lock observation — 2026-08-30

Exact clean base `b92a1a6e67d50ea606bfe2fc9f677a48338f284b` / tree
`828606368f0a34219a390096e942baefba788f71` retained the bounded physical observer above and
classified every state with an unknown active-lock fact. Exact measured candidate
`89779b371595e5d0b795e3aac17dfad434090079` / tree
`6692ec5e9a48be725af3ea439017a9f82d302abd` added a bounded, read-only Linux `/proc/locks`
snapshot. It recognizes only definitely-held whole-file exclusive `flock` locks on the exact
direct or runtime-generation lock inode retained through an `O_PATH` descriptor. Missing,
released, malformed, ambiguous, partial, POSIX, read, blocked, changed, symlinked, or hardlinked
evidence remains unknown. The observer still grants no ownership, adoption, cleanup, or eviction
authority. The lock table is capped at 1 MiB / 16,384 rows, and candidate descriptors are capped at
512 across the observation, leaving substantial headroom under the normal inherited 1,024-file
soft descriptor limit. An already descriptor-heavy caller can still fail closed as unreadable.

The physical control was the same unchanged four-state big-red namespace: 1,090,037,078 logical
bytes, 1,093,828,608 allocated bytes, and zero reclaimable bytes. One existing direct state lock
was held transiently with `flock`; its device, inode, size, mode, owner, atime, mtime, and ctime were
identical before and after. Releasing it restored a nonblocking acquisition and the candidate
returned to four unknown states. Base and candidate release binaries were built from isolated
target generations and identified by SHA-256
`85f653e59b45cc083fc3a401116eab374b7982b37fc548de13a66031d8bee6b1` and
`311e7d475d1ca569e9b032b55203f402ad7640dd1b395f2473077caba5042865` respectively.

Each phase used three warmups and 40 measured samples per arm in rotating/reversed A-B-B-A order.
Every sample validated the exact byte totals and zero reclaimable result. Base always reported four
unknown states; the candidate reported four unknown states while no lock was held and exactly one
in-use plus three unknown states while the control lock was held.

| Phase | Arm | Median (ms) | p90 (ms) | Min–max (ms) | Max RSS (KiB) |
| --- | --- | ---: | ---: | ---: | ---: |
| no lock held | base | 7.725 | 8.235 | 7.275–8.438 | 4,492 |
| no lock held | candidate | 7.985 | 16.140 | 7.385–20.110 | 4,524 |
| one exact lock held | base | 7.652 | 8.459 | 7.410–11.168 | 4,480 |
| one exact lock held | candidate | 7.881 | 18.442 | 7.234–24.578 | 4,492 |

Known-active classification cost 0.230 ms / 3.00% at the median; the inactive cost was 0.260 ms /
3.37%. Candidate p90 and maximum were noisier in both phases, so the median improvement in truth is
not presented as a latency win. The product win is replacing one false unknown with kernel-proven
in-use evidence for about a quarter millisecond while keeping absence non-authoritative.

One invalid build attempt exposed a separate cache-identity control. After building the base, a
candidate build pointed at the same `CARGO_TARGET_DIR` returned in 0.02 seconds and emitted the
byte-identical base binary even though the worktree was at the candidate commit. The changed source
mtimes preceded the shared output mtime, so Cargo's local freshness check did not distinguish the
worktree generations. Rebuilding in an isolated candidate target took 58.10 seconds and produced
the distinct candidate digest above. Glaeda must therefore bind cross-worktree compiler-cache
reuse to validated source/toolchain/target identity or normalize freshness deliberately; an
unqualified shared target directory is neither a valid benchmark arm nor a safe fast path.

## Cross-worktree Cargo target environment binding — 2026-08-30

While building the lock-observer candidate, a deliberate shared-`CARGO_TARGET_DIR` control exposed
a real stale-output failure. Base `b92a1a6e67d50ea606bfe2fc9f677a48338f284b` first populated the
target. Different candidate source `89779b371595e5d0b795e3aac17dfad434090079` had older mtimes;
Cargo exited successfully in 0.02 seconds and returned the byte-identical 5,083,248-byte base
binary, SHA-256 `85f653e59b45cc083fc3a401116eab374b7982b37fc548de13a66031d8bee6b1`.
An isolated candidate target instead took 58.10 seconds and produced the distinct correct
5,059,856-byte binary, SHA-256
`311e7d475d1ca569e9b032b55203f402ad7640dd1b395f2473077caba5042865`.
The invalid output was rejected before any performance sample.

Code inspection then found that cross-worktree `hot-run` mounted its selected `target` view but
still inherited a caller `CARGO_TARGET_DIR`, allowing Cargo launched directly or through another
tool to bypass the view. Exact candidate `e98ddabc5ce1f74b39fc475fb28e989899bd5ca5` / tree
`9c33a18e5b0de9df72917e0c362fea1c09bfd10b` binds that environment variable to the stable in-view
path whenever the exact `target` cache is selected. A physical temporary Git/Cargo fixture proved
the semantic difference: current main exited zero while writing only to the ambient target and
not the task-private state; the candidate exited zero while writing only to the task-private state
and left the ambient target untouched.

The complete-command performance control used Rust/Cargo 1.97.1, one Cargo job, three warmups per
arm, and six rotating order permutations on big-red. Raw local, current cross-worktree `hot-run`,
and candidate cross-worktree `hot-run` operated on the same tiny crate and independently warmed
targets:

| Workload | Samples/arm | Raw median / p90 | Current median / p90 | Candidate median / p90 | Current / candidate max RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `/bin/true` | 30 | 1.064 / 1.166 ms | 70.094 / 76.830 ms | 68.713 / 76.131 ms | 25,216 / 25,096 KiB |
| warm `cargo check --offline --quiet` | 20 | 12.296 / 12.652 ms | 81.234 / 88.679 ms | 82.809 / 89.411 ms | 25,056 / 25,164 KiB |

The candidate changed the warm-Cargo median by +1.575 ms / +1.94% and the no-op median by
-1.381 ms / -1.97%; overlapping distributions and unchanged RSS show no material wrapper cost.
The raw arm remains the no-isolation latency floor. A hosted Actions arm is not comparable because
the decision concerns an inherited operator-shell variable and a local mounted cache view, not
checkout or runner provisioning. The raw receipt was 2,002 bytes with SHA-256
`8efed1ecb293c622a5b5dd57783a8699e9b19ae94b7c569913e0eb21550f9620`; its private temporary paths
were not retained. This correction grants no shared-cache selection, publication, adoption,
reclamation, or deletion authority.

## Read-only project checkout front door — 2026-08-30

Issue [#912](https://github.com/teamleaderleo/glaeda/issues/912) measured one bounded Unix CLI
invocation over the
already-landed bounded `ProjectCheckoutObserver`. Exact measured code head
`71c2d21d26ca0152936246bad559404002ae67d0` / tree
`8f9b29affd22c7ff09be95083813a3d312d5a49e` produced a 1,786,864-byte release
`glaeda-project-observe`, SHA-256
`be59f243d3eb2cef55b82b48cf095a9090516204f707640b3390a87f41ab7d44`.

The command accepts one explicit canonical absolute checkout and human or JSON output. It invokes
the fixed `/usr/bin/git` through the existing sealed timed executor, requires two equal snapshots,
and emits exact commit/tree, canonical GitHub source where provable, dirty/untracked/local-ahead
recovery facts, worktree/submodule topology and one opaque physical materialization identity. It
adds no catalog, adoption, residency, execution, remote-freshness, cache, cleanup or mutation
authority.

An independent Python semantic oracle issued the same two preflights and two seven-command
snapshots, normalized every output fact separately, and produced the exact same 749-byte report.
Canonical report SHA-256 was
`1feea1e94f4ae97c020d8bf343692e2132b30b7a07c4bc821e4a33624919e716`.
A mode-0600 process/network trace observed exactly the candidate plus 16 `/usr/bin/git` children,
zero unexpected executables and zero network-class syscalls; trace SHA-256 was
`a6c5b760a35c0c9f14e95466e8016e9194155463136845bba5e40633dbf83d87`.

The first claimed matrix under #911 is invalid and grants no performance evidence. Its timed
control also parsed the complete report in Python, so Python startup/parsing made it stronger and
slower than the preregistered weak lower bound. The invalid 30-pair receipt remains identified by
SHA-256 `a3da3c2233047b9190e476b6f663076c0d9bd117bac2d5fba36cc33400c546d5`.

#912 froze the correction before new samples. The timed control was one env-clean Bash process
issuing the same 16 fixed Git observations, validating statuses and discarding all output. Its
trace observed `/usr/bin/env`, `/bin/bash`, exactly 16 Git children and no unexpected executable.
Bash made two failed local AF_UNIX NSS connections to `/var/run/nscd/socket` before spawning Git;
there were zero AF_INET/AF_INET6 calls and zero socket calls in every Git child. The 14,003-byte
mode-0600 trace SHA-256 was
`68e0ef50dccff421642c829f547d5a4543694cedcf7ee36f0332d87b9caa68e6`.

Two warmup pairs preceded 30 measured serial A-B/B-A pairs on big-red, Python 3.14.4, Git 2.53.0,
Linux 7.0.0-30-generic. The frozen clean checkout state covered 506 tracked/untracked entries and
29 Git-state files; its content/metadata vector SHA-256
`bef67482e5ef363b01175af6fdf31511f8ac30ab19e457d45e8de8b7735ec7d0` remained equal after every
pair.

| Complete process | n | Median | Mean | Stddev | Min | p95 | Max | Max RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| weak Bash/16-Git lower bound | 30 | 18.579190 ms | 18.4686544333 ms | 0.730956627 ms | 16.115688 ms | 19.629664 ms | 19.657880 ms | 17,048 KiB |
| typed Glaeda candidate | 30 | 15.6433895 ms | 18.5451014333 ms | 10.644055161 ms | 14.799264 ms | 41.307917 ms | 68.944897 ms | 16,920 KiB |

The candidate median was 2.9358005 ms / 15.8015527049% lower, or 1.18767035750x. Its mean was
0.076447 ms higher, p95 was 21.678253 ms higher and maximum was 49.287017 ms higher than the lower
bound. That real tail is consistent with the existing 25 ms bounded-capture poll and is not hidden
by the median. The candidate nevertheless passed the preregistered interactive limits: median no
more than 25 ms above the weak control, p95 at most 50 ms and maximum at most 75 ms. Maximum RSS was
128 KiB / 0.750821210699% lower than control, inside the 25% ceiling.

Every measured process exited zero; candidate report hashes stayed exact; control stdout and all
stderr remained empty; and every state vector remained unchanged. One pre-matrix wrapper attempt
aborted on its first warmup control because it mistakenly tried to parse the deliberately empty
control stdout as JSON. It produced no matrix or receipt. Including that setup error, the semantic
pair, control smoke/trace, warmups and corrected matrix used 71 complete invocations against the
cap of 72.

The corrected mode-0600 raw receipt was 25,750 bytes with SHA-256
`3c3ca2c73ee0a028b507160e7855ba119d95ce98236552647a4b597df33d6a0e`. Frozen lower-bound,
semantic-control and final harness SHA-256 values were respectively
`1a93fdeafeb16a01591941f9eb2d1341daa36120562de1a015eff2eec17a6b80`,
`b911f4c5af9a9d2154c9df52cec058a925cba6edb7c731719ab37ca88b7f57fd`, and
`56a9b85a03cd2e57c5c16380a14ccedb8d05df0368eb0ea3bb609492e9fae66f`.
No shared process-executor, hot-run, cache/generation/lifecycle, task-worktree, reflink/storage or
Quarry code changed. The capture tail remains separate owner evidence, not scope for this command.

## Bounded Linux machine observation — 2026-08-30

Issue [#919](https://github.com/teamleaderleo/glaeda/issues/919) selected a recurring big-red
handoff loop that agents previously assembled from separate `/proc`, `systemctl`, `ss`, and text
processing commands. Exact measured code head
`39fb05b777166e7b6ece0775b2bc983fab542b43` / tree
`d42a50dfffc78f095ce7545c113ee9fb4b36917f` produced a 1,739,088-byte release
`glaeda-host-observe`, SHA-256
`46d5bf3990fe58e64561cc14e83925b2a2bab58f2e338f69bb0494b2d50521c4`.

The command reads bounded fixed `/proc` CPU/load, memory, PSI, and TCP tables directly and starts
only two fixed `/usr/bin/systemctl` children for failed system/current-user unit counts. Human and
JSON forms come from the same typed report. Output contains no paths, addresses, unit names, PIDs,
command lines, environment values, repository content, or arbitrary logs. Systemd unavailability
is explicit without discarding valid kernel evidence. The report is scoped to the
`current_execution_context`; it does not prove physical-host or namespace identity and grants no
admission, ownership, process/service mutation, cleanup, scheduling, or cache authority.

Three local complete-process arms observed the same current big-red execution context:

1. an ordinary Bash composition using one Bash, five `awk` children, `date`, `ss`, and two
   `systemctl` children;
2. one Python 3.14 process reading `/proc` directly and starting only the same two systemd
   observations;
3. the compiled typed Glaeda candidate.

One warmup per arm preceded 24 serial measurements per arm in six rotating order permutations.
Every timed command ran under the same GNU `time` wrapper for max-RSS collection. The harness used
its monotonic clock for complete wall time and waited-child resource deltas for CPU. All 72 reports
passed shape/range validation; logical CPU count, memory/swap totals, failed-unit states, and the
watched-port vector produced one exact stable signature, SHA-256
`a6a0a45ce8e543420f0bd6276aceaba94e6b74625aa1dafb150b5f9c3bb52a8c`. Dynamic timestamps,
load, available memory, and PSI were range-checked, and cumulative PSI totals never moved
backwards within an arm. Their expected movement produced 24 distinct output digests per arm.

| Complete process | n | Median | p95 | Maximum | Median child CPU | Max RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ordinary shell composition | 24 | 26.1697515 ms | 27.916760 ms | 31.911187 ms | 25.917000 ms | 9,244 KiB |
| single-process Python | 24 | 25.603824 ms | 35.235668 ms | 38.723993 ms | 23.366500 ms | 13,064 KiB |
| typed Glaeda candidate | 24 | 7.960407 ms | 9.466500 ms | 9.906237 ms | 6.223500 ms | 7,864 KiB |

Against the ordinary shell composition, Glaeda reduced median wall by 18.2093445 ms / 69.5816%,
p95 by 66.0903%, median child CPU by 75.9868%, and max RSS by 14.9286%. Against the direct Python
control, it reduced median wall by 17.643417 ms / 68.9093%, p95 by 73.1338%, median child CPU by
73.3657%, and max RSS by 39.8040%.

Separate direct mode-0600 traces excluded the common timing wrapper. The shell arm executed 11
process images including its `/usr/bin/env` wrapper; Python and Glaeda each executed four, namely
the wrapper, their own process, and two `systemctl` children. No arm created an AF_INET/AF_INET6
socket or connection. Each made exactly two successful AF_UNIX connections to the system and user
systemd managers. Final trace SHA-256 values were
`ad405a0094126199ac88ac3e6b6e833922b42b66a562199816c36797bb86139d`,
`bc8b349cd7c16337c2648e4ee1664e47deed80c12a6a676bb08fa90f3df008b8`, and
`5fa8900c8871c3591dfe7ea982634b257c9fa85867f0a73168eb95dd9c4b395a` for shell, Python, and
Glaeda respectively.

The complete bounded receipt SHA-256 was
`0b25a542c769163c60f613c991f44072ded5c5fbedfe94aa113f85909d9ccda7`. Frozen shell, Python, and
harness SHA-256 values were respectively
`5719f26e0c149e3c39386a0101a29e488edbafe21228f654c445dc9ff9f3cbdd`,
`4ad8f882bd0dcdea64913dbaa36108b9deaee92ab5fcee294cb7fa5fda920757`, and
`51c20e0b4b2149bb5aa4702820f24253e6b409f92ea71e4a8679a496a12b2b69`.
Free hosted GitHub Actions is not a comparable arm because it cannot observe operator-local
machine state; a self-hosted Actions job would invoke this same local command. No hot-run, G0,
resident/cache lifecycle, project/worktree/storage/reflink, protected-cache, or Quarry code
changed.

Post-merge verification exposed one platform-packaging error outside the measured Linux path. The
first binary used a non-Linux `compile_error!`, so repository-wide Apple Silicon `--all-targets`
checking correctly failed after PR #920 merged. Exact fix
`48ccb95c83809305f7e2d2bff2fc454093a0756c` replaces that compile-time rejection with a target-gated
non-Linux stub that exits two with a static unsupported-platform message; all Linux implementation
items remain Linux-gated. The exact
`cargo check --locked --all-targets --all-features --target aarch64-apple-darwin` control then
passed. This correction changes no Linux observation semantics, benchmark control, or authority.

## Checkout-local Cargo target cost observation — 2026-08-30

Issue [#926](https://github.com/teamleaderleo/glaeda/issues/926) replaces path/age guesses with one
bounded observation-only Cargo-target cost surface. Exact candidate
`317491650fc682763ea199ac439d5a961de3466f` / tree
`18325f1479473941d8f917a02206bf102244ac0e` produced a 1,809,080-byte release binary, SHA-256
`467d2efb3d9f9c5e47a483a7e9347d70e0e4e2dfe8de17ef74d35f885e4d8dcc`.

The command accepts one explicit canonical checkout. Two equal offline Git observations bind the
commit, tree and physical worktree generation around one descriptor-relative `target` walk. The
walk follows no symlink, reads no file content, uses `O_NOATIME`, remains on one filesystem, and
rejects drift, unsupported objects, more than 2,000,000 entries or depth above 64. Its typed report
contains no checkout path or child name. Visible `st_blocks * 512` is explicitly not exclusive or
necessarily reclaimable when reflinks/shared extents exist. External hardlinks are reported rather
than mislabeled as savings.

The first all-worktree pass also found a legitimate existing Git shape: four configured branches
had an upstream name but no `branch.ab` because their tracking refs were unavailable. The existing
observer model already represents that state as `upstream_configured=true` and ahead count
`unknown`, but its parser rejected it. The candidate now accepts that exact recovery fact while
still refusing an ahead/behind record without any upstream; a focused regression covers the
correction.

The performance control used exact clean main
`54cc3462220a520687f81e635cf6320d1c0ae664` / tree
`1eaff79aa67b18796c264bc9b1fb5902d09a2201` and its unchanged retained Cargo target. One warmup
per arm preceded 30 samples per arm in rotating six-permutation order. The incomplete lower bound
was one GNU `du` allocated-byte pass; the normal complete control ran separate apparent and
allocated passes; the candidate returned both totals plus Git, owner, hardlink, marker, count and
mtime evidence.

| Complete observation | n | Median wall | p95 wall | Median child CPU |
| --- | ---: | ---: | ---: | ---: |
| one allocated-only GNU `du` pass | 30 | 11.903998 ms | 14.274691 ms | 12.774000 ms |
| apparent + allocated GNU `du` passes | 30 | 23.559662 ms | 28.240295 ms | 25.337500 ms |
| typed Glaeda report | 30 | 41.140622 ms | 80.158346 ms | 45.757500 ms |

Glaeda is not a faster raw directory walk: the additional identity and safety evidence cost
17.580960 ms / 74.6231% at the median over the two-pass control. The absolute complete latency is
still about 41 ms. Formal max-RSS is unavailable because the harness's `posix_spawn` child starts
with the resident Python parent's prior RSS high-water mark; those equal inherited values were
discarded rather than presented as command memory. Every candidate report and both `du` totals
agreed on 5,327,636,580 logical and 5,341,753,344 allocated bytes across 6,948 entries. Source,
target identity and the target/marker metadata vector remained unchanged. Matrix and harness
SHA-256 values are respectively
`0692b87d64365d0d9701d0fb803d73bd26842767f8f8127fe67b143c3490ce60` and
`ef209fd3eb8475bcb95da64dc1c38ff9ac246ada3d4bd811c1f38909e1b7f20f`.

The same exact binary observed all 13 registered Glaeda worktrees while no Cargo/rustc process was
active. Eleven targets were present and two absent. The present targets contained 111,858 entries,
95,312,350,828 logical bytes and 95,514,734,592 visible allocated bytes; per-target allocation
ranged from 3,162,734,592 to 18,379,030,528 bytes. All eleven target/checkout and descendant-owner
checks passed, all hardlink coverage was complete inside the observed trees, and every byte total
matched the two GNU `du` controls. The 3,162,734,592-byte minimum is this candidate's disposable
verification target; excluding it leaves 92,352,000,000 allocated bytes in ten pre-existing
targets. Current activity, last successful use, rebuild cost, retention value and deletion
authority remain unknown for every row. Inventory and inventory-harness SHA-256 values are
`c4755a0a496f267ff1ba845da08d930ba8e959d2e72dd949d96013449711fe68` and
`d72bc472fd29a3c601391bb785bda23bdd89b24d4fbb3faa52041b633895ba6d`.

This slice performs no holder scan, benchmark-result reuse, cache adoption, worktree cleanup,
retirement or deletion. The next #926 experiment must attach exact cold-versus-warm rebuild cost
and direct holder evidence to these opaque target generations before proposing any retention
policy.

## Checkout-local Cargo target positive-holder observation — 2026-08-30

The next issue #926 slice adds positive-only Linux reference evidence without turning a zero count
into absence or cleanup authority. Exact measured code head
`ab22d7b379865b74dc36e2818c3127e554d8832a` / tree
`556b37c7b96a0c0c8ac4c474c395ecbd242efb61` produced a 1,830,392-byte release
`glaeda-cargo-target-holders`, SHA-256
`9698871c4c29bbd518b4d1900aca029893ebccc9a4df754a3b5c7ab2f64c0385`.

The command brackets one bounded `/proc` scan with equal offline checkout observations and binds
the result to the cost observer's exact physical target identity. It counts visible process CWD,
root, open-FD, mapped-file and unique mount-namespace references. Output retains no PID, process
name, command, path, file name, map row, mount row or environment value. Foreign/root processes
whose evidence the caller cannot read are counted as incomplete. The scan is explicitly non-atomic,
and `universal_absence_proven` is always false. Its only dispositions are `holders_observed` and
`none_observed`; neither authorizes retention, signaling, cleanup or deletion.

The no-holder matrix compared three complete processes on the same unchanged target:

1. conventional recursive `lsof 4.99.4 +D`, which walks the named tree and emits raw file/process
   fields;
2. an independent Python 3.14.4 implementation of the same positive `/proc` categories and
   bounds, without Glaeda's Git/target identity or typed error surface;
3. the compiled Glaeda command, including two complete checkout observations and the typed holder
   report.

One warmup per arm preceded 30 measurements per arm in rotating six-permutation order. Wall and
child CPU came from direct shell-free `posix_spawn`/`wait4` execution. Max RSS was measured in a
separate ten-sample rotating matrix using GNU `time %M` around each exact command so timing did not
include the measurement wrapper.

| Complete observation | n | Median wall | p95 wall | Median child CPU | Median / max RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| recursive `lsof +D` | 30 | 139.637090 ms | 156.596921 ms | 136.757500 ms | 5,934 / 6,284 KiB |
| direct Python `/proc` | 30 | 80.8384935 ms | 86.404635 ms | 80.714500 ms | 14,736 / 14,744 KiB |
| typed Glaeda report | 30 | 79.5290195 ms | 91.491790 ms | 85.210000 ms | 5,704 / 5,784 KiB |

Against recursive `lsof`, Glaeda reduced median wall by 60.1080705 ms / 43.0459%, p95 by
41.5750%, and median child CPU by 37.6926%; median RSS was 230 KiB / 3.8760% lower. Against the
direct Python oracle, Glaeda reduced median wall by only 1.309474 ms / 1.6199%. It was not a
uniform latency or CPU win: p95 was 5.087155 ms / 5.8876% higher and median child CPU was
4.495500 ms / 5.5696% higher. Median RSS was 9,032 KiB / 61.2921% lower. The additional work buys
typed checkout/target identity, fixed bounds, fail-closed drift handling and a stable path-private
schema rather than a raw `/proc` speedup.

Every timed arm produced `none_observed`; both typed implementations kept
`universal_absence_proven=false`. Glaeda examined 433-435 process entries, completed 145-147 and
marked 287-288 incomplete, examined 9,207-9,227 FDs, 386-388 readable maps and three mount
namespaces, and read 5,126,473-5,139,299 pseudo-file bytes. The target identity remained exact.

A separate physical positive oracle started one owned `/usr/bin/sleep` with its CWD at the target
root and stdin open on one fixed regular target file. Python and Glaeda each reported exactly one
holder, CWD process and open-FD process; Glaeda excluded itself and retained
`universal_absence_proven=false`. `lsof +D` emitted a matching positive field record but still
exited one on this recursive walk, proving that its exit status alone cannot distinguish no match
from a positive incomplete traversal here. The harness sent SIGTERM only to its owned fresh process
group, reaped it, and proved its `/proc` entry absent.

Final no-holder matrix, Python control, matrix harness, positive harness and positive receipt
SHA-256 values are respectively
`afa1380face7cc075dccdd7981be96d527c9390ec536365ec1d677cc42c8b0cf`,
`cfe0a98294be93dff335e31c684803dd1ba802008d8b88725db70a7e505ed704`,
`9b4e2991bf15d5a5612a8fdf030b2e4c64625aaad87600fc2179584f2275db87`,
`a1e04cf67c69ccf1a39c5601f7a3fbbd81e8476f632b93429fca202b4a868dcc`, and
`b27e09e3a37afc2cebfd5d154d73c7e908b660503e6540bd48e4d1606ef74a53`.

One earlier matrix used direct `wait4.ru_maxrss` and returned the identical 19,836 KiB launcher
high-water mark for all three arms. Its latency results were semantically valid but its RSS column
was not discriminating, so the entire receipt, SHA-256
`8228bf9a23b03d65f52a5fed704c6088b6ed4df2fbc3af243118e01f4ae85fca`, is excluded from reported
results. No benchmark process survived. This slice still adds no lock, lease, liveness proof,
retention policy, reclamation or deletion path; cold-versus-warm rebuild value remains the next
independent decision input.

## Checkout-local Cargo target rebuild-value composition — 2026-08-30

The next issue #926 slice consumes the schema-v6 measurements produced by `glaeda-hot-run` rather
than copying benchmark numbers into policy by hand. Measured production and integration-test source
SHA-256 values were respectively
`da5f9dc9f93a8385be749fdd95b4dfbfbd1e81c58181ff8a783bd47615e53cda` and
`38f17147b6112f0e62196615acfbcbd030fb8e8477f5a96981583b782fb4caca`.

The typed command accepts 1-32 explicit private cold receipts and 1-32 warm receipts. It requires
schema v6, one `target:native` cache view, a successful command, exact comparison/resource/runtime
equality, the current checkout observation, absent-before cold samples and current-target-ID warm
reuse. Descriptor-bound file reads reject symlinks, foreign ownership, group/other permission bits,
more than 1 MiB, metadata drift and duplicate device/inode identities. Equal complete
checkout/target snapshots bracket the join. The result remains non-atomic, labels caller-supplied
receipt authenticity unproven, and grants no retention or mutation authority. It explicitly reports
last successful use as unknown because schema v6 has no epoch timestamp.

The physical workload was a dependency-free Rust 2024 library at exact commit
`fb75586274c0178e4130d6165875fb4e997c5873`. Five cold `cargo test --locked --quiet` samples each
started after removal of the route-owned fixture's `target`; twenty warm samples retained the final
target. All 25 commands passed. The exact comparison key was
`sha256:bc792d28c871d388676d3ae61597c8554a7b93dce40b80538065a052cea7317e`.

| Workload result | n | Median wall | Median total CPU | Median max RSS |
| --- | ---: | ---: | ---: | ---: |
| cold rebuild | 5 | 118.094 ms | 200 ms | 196,544 KiB |
| warm reuse | 20 | 24.564 ms | 20 ms | 160,098 KiB |

The retained target occupied 8,130,560 visible allocated bytes. Warm reuse saved 93.530 ms /
79.1996% at the median, a 4.8076x speedup, plus 180 ms median CPU; median max RSS was 36,446 KiB
lower. These tiny-crate results prove the composition and reset discriminator, not Glaeda's
full-suite economics. The separate full Glaeda workload above remains the representative result:
43.985 seconds cold versus 3.355 seconds warm, saving 40.630 seconds / 92.37% / 13.11x while
retaining about 1.895 GB.

The value command itself was compared with two standalone Python 3.14.4 controls over the identical
25-file corpus. `python_simple` parsed and calculated medians only. `python_checked` additionally
validated the receipt/source/target relationships but did not freshly observe Git or the target
filesystem. Glaeda performed the bounded descriptor checks plus two complete current checkout and
target observations. One warmup preceded 30 rotating-order runs per arm; ten separately rotated GNU
time runs measured RSS.

| Complete process | n | Median wall | p95 wall | Median child CPU | Median / max RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Python arithmetic-only lower bound | 30 | 31.748246 ms | 34.865142 ms | 31.567500 ms | 16,888 / 17,044 KiB |
| Python receipt relationship checks | 30 | 32.275685 ms | 34.797623 ms | 32.103000 ms | 17,042 / 17,120 KiB |
| typed Glaeda plus fresh physical binding | 30 | 43.621420 ms | 67.015977 ms | 50.267500 ms | 5,298 / 5,372 KiB |

Glaeda's additional current-state binding cost 11.345736 ms / 35.15% median wall over the checked
Python control and had a wider p95, while using 11,744 KiB / 68.91% less median RSS. All 90 timed
processes returned exactly equal cold/warm/savings values. The result is a safety/evidence tradeoff,
not a parser-speed claim.

Final report, accepted-receipt-set, benchmark harness and independent control SHA-256 values were
respectively `9d0c4e1845e6675a26f0e5967c03f3006902c95e5d1d455084ce699618174bfa`,
`e08712d62e09656d29fc88e58c5d8a474471fc069a1d01dd3e9019acc18b6e92`,
`1734c4340a02ee7de377c764149a0f12031e11dead92cf4402d6621c10142729`, and
`41c22c69bb640a6d9bedf36e60a360f2651f2521641c32379b2fe0cc4acc8b07`. One earlier corpus was
invalidated before timing because the fixture had not ignored `target/`, so the cold build changed
its checkout observation. That corpus is excluded rather than averaged. No benchmark worker or
listener survived; only the named route-owned evidence directory remained for publication.
