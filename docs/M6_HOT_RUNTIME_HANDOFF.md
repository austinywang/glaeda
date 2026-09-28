# M6 hot-runtime handoff

Status snapshot: 2026-08-25

This is the durable continuation point for SmolRunner's resident trusted-agent runtime work. Read it together with `BLAZINGLY_HOT.md`, current `main`, and the linked issues. Current accepted code and later reviewed issue/PR decisions win over this snapshot.

> **Disposable is a capability. Trust decides residency.**

## Current answer

The leading trusted-agent path is a path-class-specific resident Linux design:

```text
Mac durable control plane
        |
        v
resident Linux sandbox
        |
single-writer project disk
        |
        +------------------------------+
        |                              |
immutable source / dependency state   write-heavy build state
        |                              |
immutable Git object generation       private CoW lineage
        |                              |
private task Git metadata              |
        |                              |
        +-------- OverlayFS task view--+
                       |
                     agent
                       |
                edit -> test -> edit
```

Preferred mechanisms:

- source and mostly-read project/dependency state: immutable resident lower state plus task-local OverlayFS upper/work;
- Git task identity: task-private Git metadata backed by one exact immutable object-generation alternate;
- write-heavy compiler/build output: task-private CoW/reflink lineage where supported;
- fallback: ordinary private Git/materialized state whenever hot-path identity or authority is unavailable;
- hostile or unknown work: the strict fresh disposable worker lane remains separate.

Hot state is acceleration. Durable execution state and fresh exact observation remain authoritative.

## Current `main`

At this snapshot, `main` is `43450cf76c6cd5f388f3d79fe112ae536697f534`, merge of PR #633.


## Continuation delta — current after #633

This section is the newest durable continuation record. Where later historical snapshot text in this file still describes #614 as awaiting acceptance, #592 as lacking a sealed admin-producer plan, or `main` as older than #633, this section wins. The historical text remains below because its experiments, corrections, and authority rationale are still useful.

### Landed since the prior handoff snapshot

- **#614 / #592 publication candidate audit landed.** Final head `2da56b9f050cefd7dbbb67859d6fe5e65ef10621` passed Verify `32761168266` and Linux acceptance `32761168272`; merge is `13529efc2dfb873488c9f29f66c9fe46a544f2e2`. The O(N) walk is publication-time evidence only and remains outside hot task admission. It accepts retained #609 source authority plus a sealed retained staging candidate, proves safe entries/inode independence/nested-alternate absence/drift, and grants zero Git/root/freeze/marker/rename/publication authority by itself.
- **#633 / #592 pure admin producer plan landed.** Product head `c33e431b9cc3c008832a5487d66121a7f770357a` passed Verify `32763130073` and Linux acceptance `32763130047`; merge is `43450cf76c6cd5f388f3d79fe112ae536697f534`. It seals fixed `/usr/bin/git clone --bare --local --no-hardlinks`, a reviewed empty template, credentials/prompts disabled, a cleared allowlisted environment, exact source-generation + candidate-binding digests, and a future credential policy requiring freshly verified non-root `smolrunner-admin` UID/GID with supplementary groups cleared. It performs no account lookup, filesystem I/O, process spawn, UID/GID change, Git execution, root mutation, staging, marker write, rename, publication, or cleanup.
- **#623 / #588 host Lima argv hardening landed.** The sealed plan uses `limactl --tty=false shell <exact-instance> -- <guest-command...>` and retains the guest `sudo -- /usr/bin/env -i ...` boundary.
- **#626 / #592 guest-control vocabulary landed.** `PublishImmutableGitPoolGeneration` is a distinct canonical operation tag; it has no handler or mutation authority yet.

### Acceptance-gated exact heads

- **#618 / #565 P2 collector:** hardened head `a0bdf00fdcf98e7fd3484d8d8630904a1db37f64`, Verify `32760003780` green. The supplied standalone-disk directory is opened component-by-component with no-follow semantics; Linux guest `st_dev` is decoded and must equal the single exact project `mountinfo` major/minor row. The receipt still says physical ownership unresolved and cannot mint #607/#589 production correlation authority. Because the final repair is ownership-adjacent, merge waits for implementation-independent exact-head acceptance.
- **#619 / #617 bounded non-secret stdin:** head `9907b0a5df525170606937a653e23e02826f3cf2`, Linux acceptance `32760871369` and Verify `32760871416` green. <=4 KiB exact bytes share the existing deadline/output/process-group cleanup loop; writer setup and actual BrokenPipe failures kill/reap the owned group; stdin bytes are absent from argv/environment/ExecutionRecord surfaces. Because this is process/concurrency-sensitive, merge waits for implementation-independent exact-head acceptance.

### Physical and execution blockers now have explicit owners

- **#628** owns one observation-only receipt on the real operator Apple Silicon Mac / installed pinned Lima environment. It must derive the available read-only disk inventory command from installed help, directly observe the standalone-disk directory, keep raw private evidence off GitHub, and end with **YES / NO / AMBIGUOUS** for exact host-disk + attachment + resident-sandbox + guest-filesystem correlation. Missing or ambiguous state stops without create/attach/unlock/format/resize/delete.
- **#629** owns publication and command-free observation of one exact root-owned, non-task-writable Linux/aarch64 guest-control binary generation. A surviving path never blesses arbitrary bytes.
- **#631** owns the read-only Mac invocation-target proof. Only exact accepted limactl bytes/generation, private protected `LIMA_HOME`, the exact running VZ/aarch64 resident sandbox generation, and the exact accepted #629 guest binary may mint the crate-private `TrustedGuestControlInvocationTarget`.

### Current #592 transaction boundary

With #614 and #633 landed, the next privileged pool-publication executor must compose existing authority instead of inventing it:

```text
root: exact private staging envelope + retained parent authority
  -> fresh exact immutable source + smolrunner-admin account evidence
  -> seal #633 producer plan
  -> drop to verified admin UID/GID and clear supplementary groups
smolrunner-admin: execute only the sealed fixed Git plan
  -> exit
root: prove producer absence + exact retained candidate
  -> #614 candidate audit
  -> reviewed Git/object reachability proof
  -> recursive root ownership/frozen modes
  -> write exact #590 marker from the same transaction nonce
  -> #609 observe staged frozen generation
  -> no-replace descriptor-relative promotion + parent fsync
  -> #609 observe final published generation
```

The executor is high-risk privilege/process/publication work and therefore needs implementation-independent exact-head acceptance. It must never run project-sensitive Git as root or let path survival grant staging ownership.

### Current execution order

1. Get independent exact-head acceptance for #618, then run #628 on the real operator Mac. A **YES** result may feed a separate #565 P2 production-correlation implementation; **NO/AMBIGUOUS** keeps #607 sealed.
2. Get independent exact-head acceptance for #619. In parallel, implement #629 guest-control binary publication/observation and #631 invocation-target proof.
3. Build the #592 privileged admin-producer/freeze/marker/promotion transaction around merged #633 + #614 + #609.
4. Implement #580 task-private Git creation with `--reference <accepted-generation> --no-local`, exact alternate validation, origin removal, inherited exact index publication, and final non-mutating Git/source proof.
5. After #619/#629/#631 acceptance, build the #588 one-shot transport adapter from merged #612/#626 protocol plus #615/#623 invocation plan; transport loss/drift remains reconciliation debt and arbitrary Lima argv stays unrepresentable.
6. Compose current authority -> #565 correlation -> #607 all-FD mount -> #580 Git/index proof -> bounded receipt -> ready publication.
7. Run complete cold and warm resident trusted-agent edit/test loops and compare #563/#566 receipts against the ordinary private/cold fallback.


Important M6 capabilities already landed:

| Capability | Issue / PR | Current role |
| --- | --- | --- |
| bounded hot-execution performance receipts | #563 / #564 | comparable latency/resource/storage observations |
| single-writer project-disk lease core | #565 / #567 | logical disk/attachment generations; physical Lima correlation still pending |
| Git index-v2 stat-cache patcher | #568 / #569 | pure verified index-byte handling |
| source-anchor/task-view lifecycle | #570 / #571 | exact leases and ready/run/cleanup ordering |
| trusted OverlayFS mount plan | #572 / #575 | exact lower/upper/work/merged identities, per-role ownership/mode trust, and fixed mount policy |
| hot-state path policy | #573 / #579 | overlay/private-CoW/private-empty/reviewed-share selection |
| immutable Git object-pool lease core | #581 / #583 | pool generation identity, consumer leases, draining and retirement |
| mount-plan reconfirmation | #576 / #582 | fresh authority/evidence before privileged mount work |
| exact OverlayFS role descriptor lease | #584 / #587 | retained lower/upper/work/merged descriptors |
| immutable Git-pool marker | #586 / #590 | strict 64-byte binding digest + nonzero generation nonce |
| merged-parent descriptor + basename | #593 / #594 | descriptor-relative visible-target reopen after attach |
| generic Git worktree observation vocabulary | #591 / #601 | `OverlayGitWorktreeObservation` primary, compatibility alias retained |
| sealed all-FD OverlayFS transaction | #589 / #607 | safe-rustix FD-only mount execution; production correlation proof remains sealed |
| immutable Git-pool ownership observer | #585 / #609 | root-owned frozen generation, exact marker, retained `objects/`, race-safe nested-alternates absence |
| one-shot guest-control protocol envelope | #588 / #612 | exact attachment/sandbox/binary binding, closed operations/debt, canonical digests; pure protocol |
| sealed one-shot Lima invocation plan | #588 / #615 + #623 | fixed exact instance/root guest argv; host `shell <instance> -- ...` and guest `sudo -- ...` boundaries; execution adapter pending |
| immutable-pool publication guest operation | #592 / #626 | distinct closed protocol tag for pool publication; no handler or mutation authority |

#607 contains physical mount mechanics, while normal product code still cannot mint the required project-filesystem correlation proof. Physical activation waits for accepted #565 P2 evidence.

Mount-plan role trust declares its enforcement status and cites the exact path plus symbol the claim rests on, the same way `docs/THREAT_MODEL.md` does.

- **enforced** — every OverlayFS role directory must be owned by root or the observing effective identity and must carry no group or world write bit, and `upperdir`/`workdir` must additionally grant full owner access. `src/trusted_overlay_mount_plan.rs::observe_directory` applies `src/trusted_overlay_mount_plan.rs::require_role_trust` before any snapshot becomes plan evidence, so a foreign-owned or untrusted-writable `lowerdir` can never become a task's read-only base. Refusals are `overlay_mount_role_owner_foreign`, `overlay_mount_role_untrusted_writable`, and `overlay_mount_role_owner_access_missing`.
- **enforced** — later ownership or mode drift on an admitted role is refused as observation drift, because `src/trusted_overlay_mount_plan.rs::revalidate_directory` and `src/trusted_overlay_mount_plan.rs::require_held_directory` compare the complete recorded `DirectorySnapshot`, which includes `uid`, `gid`, and `mode`.
- **required** — the role directories' parent chains. This observer binds each role by device and inode and holds descriptors, so a swapped role is caught as drift, but it states no policy about who may write the directories above a role. The operator's layout owner must place the four roles under a private, non-foreign-writable parent.

## Performance and kernel evidence

### Ordinary Git worktree metadata is already cheap

Owned hosted-ARM measurements put `git worktree add --no-checkout` around 1.8–2 ms on a representative SmolRunner checkout. Full tree materialization was roughly 26–31 ms. The main source-view cost is writing the tree, not worktree registration metadata.

### OverlayFS is the leading multi-agent source view

A representative hosted-ARM/XFS fan-out using an exact resident source, task Git metadata, OverlayFS lower sharing, and inherited clean source index measured approximately:

| Tasks | Ordinary Git materialization | OverlayFS + inherited source index | Task-view growth |
| ---: | ---: | ---: | ---: |
| 1 | 76.2 ms | 15.9 ms | ~8 MiB -> ~4 KiB |
| 8 | 224.3 ms | 52.4 ms | ~64 MiB -> ~32 KiB |
| 32 | 695.6 ms | 175.2 ms | ~257 MiB -> ~160 KiB |

All 41 views passed non-mutating Git proof. The same source-view model also worked on ext4, removing XFS as a prerequisite for resident source fan-out.

### Private CoW remains better for write-heavy build output

OverlayFS lower sharing performed poorly when warmed compiler output lived under the lower tree and Cargo copied up write-heavy `target/` data. Giving each task a private XFS reflink lineage for build output cut one eight-task synthetic run from roughly 5.70 s to 2.77 s.

### OverlayFS mutation boundary is settled

Owned #597 timing evidence showed:

```text
initial                 workdir entries = 0
after lower SET_FD      workdir entries = 0
after upper SET_FD      workdir entries = 0
after work SET_FD       workdir entries = 0
after FSCONFIG_CREATE   workdir entries = 1
after fsmount           workdir entries = 1
after move_mount        workdir entries = 1
```

The final safe empty-workdir second confirmation belongs after all `fsconfig_set_fd` calls and immediately before `FSCONFIG_CMD_CREATE`.

Pinned `rustix 1.1.4` exposes the required safe APIs (`fsopen`, `fsconfig_set_fd`, `fsconfig_create`, `fsmount`, `move_mount`). Project unsafe code remains forbidden. Ordinary mount ID is transaction-local observation only.

## Immutable Git object-pool findings

### Private task Git metadata is preferred

Linked worktrees remain an excellent fallback/comparator because registration is extremely cheap. Multiple linked worktrees still share common repository metadata. Task-private Git directories isolate refs, config, index, and new objects with modest overhead, so private metadata is the preferred hot-path direction when object sharing is proven.

### Active pool ownership belongs outside the task UID

A task-UID-owned seed with mode `0555` is weak against a hostile owner because the owner can chmod it writable. Active immutable generations are owned outside `smolrunner-runner`; the current guest design uses root-owned frozen generations.

The useful account split is:

```text
root              control / publication / mount authority
smolrunner-admin  trusted non-task Git producer/preparer
smolrunner-runner agent/workflow execution
```

### Producer generations require inode independence

Local Git clone can hardlink producer objects. Changing ownership of a newly cloned seed can therefore affect the producer repository through shared inodes.

V1 publication requires distinct regular-file inodes before root takes ownership. The initial producer command is deliberately boring:

```text
git clone --bare --local --no-hardlinks <exact immutable source> <exact staging candidate>
```

A later reflink-aware producer is acceptable only after distinct inode/ownership semantics are proven.

### Admin Git gets a completely scrubbed environment

An earlier `sudo -u <admin> -H` probe still attempted to read the runner user's Git attributes configuration. `-H` alone is insufficient.

Admin Git production uses the semantic equivalent of:

```text
env -i
HOME=<fixed reviewed value>
PATH=/usr/bin:/bin
GIT_CONFIG_NOSYSTEM=1
GIT_CONFIG_GLOBAL=/dev/null
XDG_CONFIG_HOME=<exact empty/reviewed config root>
```

plus fixed absolute Git argv, bounded input/output/deadline, disabled credentials/network where appropriate, and no project-sensitive Git preparation as root.

### #590 marker is generation identity

The fixed marker binds the exact #583 pool identity and a nonzero generation nonce. Git refs, HEAD, config, and objects provide content evidence. They never substitute for ownership/generation identity.

### Active generations stay frozen

Fetch/repack/GC/thaw never modify an active pool in place. Build and promote a successor generation, then drain old #583 consumers before retirement.

## Cross-owner private Git alternates are proven

The first security-correct private-clone experiment proved runner checkout/add/commit/GC and seed immutability, but the expected alternates file was empty. That result was recorded as incomplete evidence.

A dedicated non-shallow owned ARM run `32748702131` / job `97500255676` settled the question. A root-owned frozen bare seed was produced with `git clone --bare --local --no-hardlinks`; admin and runner commands used scrubbed environments.

Both forms created the exact seed `objects` alternate:

```text
git clone --shared --no-checkout ...
git clone --reference <seed> --no-local --no-checkout ...
```

Both also worked with `--separate-git-dir`. After handoff to the runner account, checkout, edit, add, commit, `git gc --prune=now`, and object lookup succeeded. The alternate survived GC and the root seed digest/ownership/frozen modes stayed unchanged.

V1 task admission should prefer:

```text
git clone
  --reference <exact-accepted-generation>
  --no-local
  --no-checkout
  --separate-git-dir <exact-private-task-gitdir>
  --template <reviewed-empty-template>
  <exact-accepted-generation>
  <exact-task-target>
```

The explicit reference expresses the shared-object dependency in argv and disables local hardlink optimization. `--shared` remains a measured comparator.

## #585 hot-path observer is complete

PR #609 completed the intentionally narrow #585 contract. It keeps per-task admission O(1)-ish with respect to object count:

- exact protected generation parent + basename;
- retained generation root and `objects/` descriptors;
- root-owned frozen ownership/mode checks;
- root/`objects` same-filesystem relation;
- exact #590 marker verification;
- race-safe `objects/info/alternates` absence;
- descriptor/name rebinding and final revalidation;
- no Git command, recursive object walk, byte accounting, publication, or mutation.

The retained `objects/` handle is intentionally `O_PATH` identity authority.

## #592 publication audit status

The first recursive candidate audit, PR #608, had useful walker mechanics but an unacceptable authority seam: caller-selected paths could mint a positive publication receipt, and the result lacked exact #583/#585 source identity plus staging-transaction identity. #608 was closed as diagnostic evidence.

PR #614 rebuilds that audit around retained authority. Exact head `3b2996d9292f25454ed8dbceef797bcb850677a9` currently has green repository Verify (`32759201142`) and Linux acceptance (`32759201261`) and is ready for implementation-independent acceptance.

#614's accepted design target:

- source comes from the retained #609 observation, never a caller-selected source path;
- #609's `O_PATH` `objects/` handle is upgraded only through descriptor-relative `openat(fd, ".")`, followed by exact identity equality;
- candidate comes from a sealed retained staging descriptor lease with no public constructor;
- candidate lease binds exact candidate #583 identity plus staging-transaction identity;
- receipt binds source/candidate #590 binding digests, staging transaction identity, and opaque physical input digests;
- source and candidate are reconfirmed after the O(N) walk;
- candidate regular files must have one link;
- source/candidate object `(st_dev, st_ino)` sets must be disjoint;
- symlinks/special entries, nested alternates, count/depth overflow, and drift fail closed;
- repeated audit on the same retained leases must yield the same receipt;
- logical bytes are reported; `st_blocks` is never presented as unique physical allocation.

#614 remains publication evidence only. It grants no root mutation, Git execution, ownership transfer, marker write, rename, or cleanup authority.

## #592 next transaction

After #614 receives independent acceptance, continue the privileged guest-local transaction in this order:

```text
root: exact private staging envelope + retained parent authority
  -> delegate only exact producer candidate to smolrunner-admin
smolrunner-admin: scrubbed absolute-Git clone --bare --local --no-hardlinks
  -> exit
root: prove producer absence + exact candidate descriptor
  -> #614 candidate audit
  -> reviewed Git/object content/reachability proof
  -> recursive ownership/mode normalization
  -> root:root, dirs 0555, regular files 0444
  -> write exact #590 marker from the same transaction nonce
  -> #609 observe frozen staged generation
  -> no-replace descriptor-relative promotion
  -> fsync publication parents
  -> #609 observe final published generation
```

Crash recovery uses the exact staging transaction identity and retained parent evidence. Directory names alone never authorize adoption or deletion.

## #580 task-private Git lane

#580 owns task Git metadata, private `.git` identity, inherited reviewed source index, final Git/source proof, and ready composition around the landed #607 mount transaction.

V1 requirements include:

- exact #571 anchor/task leases;
- exact #583 pool consumer lease;
- fresh #609 pool confirmation immediately before private Git preparation;
- `--reference <accepted-generation> --no-local` private clone under the trusted admin account;
- exact post-clone alternate = accepted pool `objects`;
- bounded reviewed local config and reviewed empty template;
- remove clone-created local `origin` before ready publication;
- atomic exact task `.git` pointer publication into the OverlayFS upper;
- exact inherited clean source-index digest and task-private index destination;
- no successful-path checkout/reset that rewrites the shared source view;
- final non-mutating Git/source proof with hooks/fsmonitor/credentials/network disabled;
- cleanup only after workflow process absence and exact task Git/mount evidence.

## #588 guest-control status

PR #612 landed the pure canonical one-shot request/receipt envelope. It binds exact project-disk revision/generation, attachment generation, resident sandbox generation, guest-control binary generation/digest, one closed operation tag, and canonical payload/request digests.

#615 landed the pure sealed Mac invocation plan. #623 then hardened the host CLI boundary so the exact plan is `limactl --tty=false shell <instance> -- <guest-command...>` while retaining the guest `sudo -- /usr/bin/env -i ...` separator and scrubbed environment.

#626 landed a distinct closed `PublishImmutableGitPoolGeneration` protocol operation. Pool publication therefore has its own canonical one-shot tag instead of being hidden under `PrepareTrustedTaskView`. The execution adapter is still pending and should consume the bounded plain-stdin process mode only after #619 receives its required independent concurrency acceptance.

## #565 physical blocker

The main production physical blocker remains #565 P2: exact operator-Mac evidence tying the standalone Lima disk generation/attachment to the guest filesystem identity. #618 is the current observation-only receipt lane on hardened head `a0bdf00fdcf98e7fd3484d8d8630904a1db37f64`; exact Verify `32760003780` is green. The collector now opens the supplied disk-directory path component-by-component with no-follow semantics and requires decoded Linux guest `st_dev` major/minor to equal the single exact project `mountinfo` row. It still leaves physical ownership unresolved and adds no writable #589 proof constructor. Because the final repair is ownership-adjacent, #618 remains gated on implementation-independent exact-head acceptance.

#607 stays sealed until accepted #565 P2 evidence can mint the project-filesystem correlation proof.

## #568 native same-HEAD XFS task materialization

The bounded P2/P3 research spike now has a native Linux implementation. It is deliberately one
materialization primitive rather than another pipeline or lifecycle controller:

```text
exact clean held-stable source at one commit/tree
-> detached no-checkout linked worktree + read-tree
-> FICLONE exact tracked regular files
-> publish only target index-v2 stat-cache fields through the existing pure patcher
-> exact detached HEAD + non-mutating diff-files + diff-index proof
-> ready research receipt, or complete ordinary Git fallback
```

The CLI accepts one to 32 distinct targets. A fan-out shares one tree inventory and the opening and
closing source-clean observations. Four bounded file workers each open a disjoint source-file range
once and clone it across all eligible targets. Each held source descriptor is streamed through
SHA-1 once and must equal the exact tree-entry blob OID; the opening and closing stat snapshots span
both that proof and every clone. Target files inherit the verified source timestamps before their
final stat publication, avoiding Git's otherwise-correct racily-clean content rehash without
forging the index timestamp or weakening the final proof. Per-target failures remain independent
and reset through ordinary Git. The target `.git` control file, linked-worktree backlink, and
detached `HEAD` are read as bounded regular files with no-follow opens and must bind under the
previously observed source common Git directory. Git subprocesses use the fixed absolute Git path,
bounded concurrent output capture, a cleared environment, and disabled optional locks, replacement
objects, lazy fetch, credentials, prompts, hooks, fsmonitor, external diffs, and protocols.
Symlinks, gitlinks, unsupported index variants, dirty/moved/wrong-content source evidence,
filesystem mismatch, and unavailable reflink support all fail to ordinary materialization. Returned
reports contain identities, bounded counts, phase durations, dispositions, and fixed fallback
classes but no paths or output.

This code grants no resident-source lease, filesystem-generation lease, task adoption, execution,
cache, cleanup, or reusable-checkout authority. The caller must still hold those lifecycle facts
and authorize Git's required linked-worktree registration under the source common administration
directory. Interrupted targets and their linked-worktree registrations are discardable cleanup
debt; directory presence never proves readiness. The research adapter has no local deadline or
cancellation protocol. Its outer Glaeda runtime must own the Git child process group and prove that
child is gone before it cleans or reconstructs an interrupted target or linked registration.

### Exact big-red control

The corrected implementation benchmark head was `ea3ccc1f66ca58e73678d8b9bdfb824279b4a094`, tree
`79e70042b3e68797ae35e54896227645e44f646a`, release-binary SHA-256
`733b9455590d55cb1724f8e1994fafa877dfe0db4188cc40971914d8bdb1e60f`. The source stayed clean at
that exact head before and after every arm. The host used kernel `7.0.0-30-generic`; the disposable
4 GiB XFS filesystem reported `reflink=1`, `crc=1`, `bigtime=1`, 4 KiB blocks, and `noatime`.

Each count used three A-B-B-A brackets, giving six ordinary and six reflink samples. Both arms ran
through the same release binary, source, commit, task count, final Git validator, resident and
otherwise uncontrolled page cache, and sequential `git worktree remove --force` cleanup. Parent
monotonic wall time includes binary startup. Child CPU/RSS came from GNU `time`. Physical deltas are
mount-level `statvfs` used bytes sampled only after `syncfs` on the exact experiment mount, outside
the timed materialization window; this avoids treating summed file `st_blocks` as unique usage for
shared extents. Each invocation had a 30-second process-group deadline; none approached it.

| Tasks | Ordinary median | Reflink median | Latency delta | Ordinary new bytes | Reflink new bytes | Byte reduction |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 66.264 ms | 33.773 ms | -49.0% | 11,538,432 | 421,888 | 96.3% |
| 8 | 88.654 ms | 61.011 ms | -31.2% | 92,282,880 | 3,350,528 | 96.4% |
| 32 | 227.162 ms | 177.322 ms | -21.9% | 369,041,408 | 13,312,000 | 96.4% |

All 36 timed task sets completed with the requested disposition, exact commit/tree, zero candidate
fallbacks, and the final proof. Reflink child RSS maxima were 6.40, 6.45, and 13.76 MiB at 1/8/32;
ordinary maxima were 12.11, 12.21, and 11.46 MiB. Reflink median cleanup was also lower: 8.324,
72.400, and 340.650 ms versus ordinary 10.083, 80.856, and 381.286 ms.

An exact-head physical follow-up reflinked all 501 regular files / 10,036,925 logical bytes. For
`src/linux_reflink_task_materialization.rs`, `xfs_bmap -v` reported the same
`6319368..6319519` physical block range and shared-extent flag `100000` in source and target. Both
files had SHA-256 `176e217764c90ce0d0a19690214a00b29e5a2b785debd8947da5eb7472e7444e` and exact matching
source-derived timestamps before cleanup.

### Scaling decision

The first native fan-out opened the same 501 source files independently in every task thread and
made the 32-way reflink median 958.850 ms. Entry-major traversal reduced redundant source opens;
four disjoint file workers produced the result above. Eight file workers were rejected after a
32-way smoke raised preparation to 471.373 ms and total wall time to 542.277 ms. XFS metadata
concurrency has a measured knee on this host; more threads are not automatically faster.

Independent review then exposed that the earlier fast candidate did not bind opened source bytes to
tree-entry blob OIDs. The first corrected allocation-heavy hash implementation measured 121.876 ms
versus 104.901 ms ordinary at eight tasks (+16.2%). Streaming the exact hash reduced that to
105.532 ms versus 101.620 ms (+3.9%). Publishing verified source timestamps removed redundant
racily-clean Git rehashes and produced the final 61.011 ms versus 88.654 ms result (-31.2%). These
are separate exact-code A-B-B-A matrices, not an attribution inferred from one uncontrolled run.

The spike therefore passes the narrow research decision: one, eight, and 32 tasks are faster while
allocating roughly 27 times fewer new bytes. It is suitable for integration behind exact
resident-source and filesystem-generation evidence, ordinary fallback, and the existing outer
admission/resource controller. It is not a generic XFS speed claim or a product-default grant.

## Failed experiments and corrected assumptions

Keep these visible so the lane does not repeat them.

### Reflink worktree v1 rewrote the CoW win

Pre-populated reflinked files were followed by `git reset --hard`, which rewrote most of the tree. Git correctness survived; the physical sharing benefit did not.

**Correction:** successful optimized population must avoid a forced checkout/reset that rewrites already-proven files.

### Reflink worktree v2 started with an empty index

`git worktree add --no-checkout` leaves the target index empty. `update-index --refresh` cannot make an empty index represent the target tree.

**Correction:** populate the exact target index first (`read-tree` or equivalent reviewed path) or inherit the exact reviewed clean source index when the commit/tree relation permits it.

### Python reflink fan-out overstated scaling cost

Hundreds of Python-launched clone operations polluted the 32-task result with interpreter/process/thread overhead.

**Correction:** judge kernel primitives with native/bulk implementations.

### `git update-index --refresh` moved whole-tree work

Refresh shifted the cost earlier instead of eliminating it.

**Correction:** direct index stat publication and inherited clean source index are the preferred hot paths.

### Early XFS allocation deltas were noisy

Delayed allocation/free caused noisy and occasionally negative filesystem-used deltas.

**Correction:** synchronize allocation samples and avoid treating summed per-file `st_blocks` as unique physical space on reflink-capable filesystems.

### Early pool read-only result overstated immutability

The seed was task-UID-owned with write bits removed.

**Correction:** active generation ownership belongs outside the task UID; current V1 uses root-owned frozen generations.

### Early local pool clone risked hardlink aliasing

Local clone could share producer object inodes.

**Correction:** `--no-hardlinks` plus explicit inode-independence proof before ownership transfer.

### `sudo -u -H` leaked Git environment state

The admin probe still attempted the runner user's attributes config.

**Correction:** fully cleared environment with fixed HOME/PATH/config roots and fixed absolute Git argv.

### First private clone did not prove object sharing

Task Git operations worked and the root seed stayed unchanged, but the expected alternates file was empty.

**Correction:** the later non-shallow scrubbed run `32748702131` established exact cross-owner alternates for both `--shared` and `--reference --no-local`.

### #597 initially failed in its C harness

The first workdir-timing C program ignored `system()`'s return under `-Werror`, so later steps were skipped.

**Correction:** fix the harness, rerun, and cite only the successful evidence. The corrected run established CREATE as the first workdir mutation.

### A combined #597 Git fixture used a shallow source

`--reference` correctly refused the shallow reference repository.

**Correction:** the dedicated non-shallow follow-up settled the behavior.

### Push-only research branches sometimes produced ambiguous absence

Several result branches failed to carry evidence cleanly or queued long enough that branch absence said little.

**Correction:** use owned draft PRs/normal Actions for critical physical evidence, and treat missing result branches as missing evidence.

## Current active lanes

- #580 — task-private Git metadata/index/Git-source proof and ready composition;
- #581 — immutable pool generation lifecycle and private task Git direction;
- #588 — one-shot guest-control execution path; #612/#615/#623/#626 landed; the process adapter waits on #619 independent concurrency acceptance;
- #592 — root/admin immutable-pool publication; #614 candidate audit still waits on implementation-independent exact-head acceptance;
- #565 P2 — operator-Mac project-disk physical evidence; #618 hardened head is Verify-green and waits on implementation-independent exact-head acceptance;
- #619 / #617 — bounded non-secret stdin process primitive is Verify/Linux-green on `9907b0a5df525170606937a653e23e02826f3cf2` and waits on implementation-independent concurrency acceptance;
- #566 — owned performance/research receipts.

Completed adjacent slices: #585/#609, #589/#607 mechanics, #591/#601, #593/#594.

## Next implementation sequence

1. Accept #614 independently on its exact head; keep the O(N) walk outside hot task admission.
2. Build #592's root-owned staging envelope and narrow admin-producer execution boundary around fixed absolute Git + cleared environment.
3. Compose producer absence, #614 audit, content/reachability proof, root freeze/marker, no-replace promotion, and final #609 observation.
4. Accept #619 independently on its exact concurrency/process-lifecycle head, then build the one-shot execution adapter from the merged #612/#626 protocol plus #615/#623 invocation plan instead of reconstructing Lima argv.
5. Implement #580 task-private Git creation using `--reference <accepted-generation> --no-local`, exact alternate validation, origin removal, exact index publication, and final non-mutating Git/source proof.
6. Finish #565 P2 real operator-Mac evidence through #618 and mint #607's first production correlation proof only from accepted evidence.
7. Compose the one-shot resident task transaction: current authority -> descriptors -> correlation -> #607 mount -> #580 Git/index proof -> bounded receipt -> Mac publishes ready.
8. Run a real resident trusted-agent edit/test loop and compare complete-loop receipts against the #563 baseline.

## Rules to preserve

- Hostile or unknown work uses fresh disposable workers.
- Names, PIDs, paths, disk names, mount presence, Git refs/config, cache presence, and surviving hot bytes carry zero independent ownership authority.
- One mutable project filesystem has one writable sandbox attachment owner at a time.
- Private task upper/work state may be runner-owned; immutable lower/pool/control state stays outside task ownership.
- Project-sensitive Git preparation runs as the trusted non-task admin account, never as root or the workflow UID.
- Admin Git receives a cleared, fixed environment.
- Active immutable Git pool generations are never fetched/repacked/GC'd/thawed in place.
- #590 marker is generation identity; Git data is content evidence.
- Destructive recovery requires exact retained/durable authority; same-name survival never authorizes adoption.
- `unsafe_code = "forbid"` remains intact.
- Performance observations grant no execution or cleanup authority.
- Benchmark complete cold/warm agent loops, not only micro-operations.

## Next major milestone

```text
accepted resident project/sandbox/disk generation
-> independently accepted immutable pool publication
-> exact private task Git identity + immutable alternate
-> exact all-FD OverlayFS task view
-> final non-mutating Git/source proof
-> agent edit/test command
-> retain valid hot state
-> exact cleanup or residency transition
```

with an ordinary cold/private fallback and the same durable authority/recovery discipline.
