# Project-disk physical observation receipt

Status: accepted read-only diagnostic/evidence tool for the #565 project-disk programme
Receipt schema: `smolrunner-project-disk-physical-observation` v1

The receipt schema, `SMOLRUNNER_*` test variables, and scratch filenames below are exact v1/transition identities. They remain unchanged while current product prose uses Glaeda.

## Purpose

This runbook captures bounded physical evidence from the operator Mac and one Linux guest without granting Glaeda ownership or mutation authority.

The collector was used by #634 to establish the Lima 2.2.0 detached/attached physical schema. Current consumers include:

- #699, which evaluates durable host-object identity and same-path replacement discrimination;
- #706/#628, which need independent physical evidence for the P4 and full-correlation acceptance lanes;
- #639/#691 and their successors, which may compare the observed Lima layout with the production P2 observer.

The receipt is research/acceptance evidence. Runtime ownership, current capabilities, and mutation authority come only from their owning durable state plus fresh live observation.

## Authority boundary

The collector performs no Lima mutation and grants no authority to:

- create, attach, detach, unlock, format, resize, delete, repair, or clean up a disk;
- start or stop a resident sandbox;
- mount a project filesystem or OverlayFS task view;
- bind a physical object to a Glaeda project-disk generation;
- construct a `TrustedProjectFilesystemCorrelationProof` or another runtime capability.

Disk names, paths, entry names, symlink targets, inventory fields, device numbers, and this receipt are observations only.

## Existing discipline reused

`tests/project_disk_physical_receipt_host_identity.rs` calls the existing `LimaHostIdentityAdapter`. That path provides the reviewed VZ/aarch64 restriction, descriptor-relative private Lima-home traversal, no-follow opens, held-entry rebinding checks, and bounded host identity output.

The collector itself deliberately carries no baked-in standalone-disk backing filename, lock filename, lock meaning, `_disks/<name>` derivation, or Lima disk-list command. The operator supplies the exact observed disk directory and pre-captured Lima JSON.

## Private evidence boundary

Keep the complete receipt private. It can contain:

- exact `LIMA_HOME`, disk-directory and mountpoint paths;
- direct entry names and symlink targets;
- raw selected Lima JSON;
- host device/inode and timestamp metadata;
- guest mountinfo/device evidence.

The collector does not read project filesystem contents or a project filesystem superblock. Direct-entry inspection is metadata-only by default. A regular file is read only when the operator explicitly supplies `--read-small-entry`; the file must be at most 4096 bytes. Never use that option for the backing entry.

## Preconditions

Use a clean checkout at the exact candidate under review and select values from current accepted state plus direct physical observation:

```bash
export LIMA_HOME='... exact private Lima home ...'
export INSTANCE='... exact resident Lima instance ...'
export GUEST_CACHE_PATH='... exact reviewed guest cache path ...'
export PROJECT_MOUNT='... exact project filesystem mountpoint in the guest ...'
export DISK_DIRECTORY='... exact absolute standalone-disk directory observed on this Mac ...'

export PROJECT_IDENTITY='... exact canonical project identity ...'
export PROJECT_DISK_ID='... exact project-disk ID ...'
export PROJECT_DISK_GENERATION='... exact disk generation ...'
export PROJECT_DISK_REVISION='... exact P1 lease revision ...'
export ATTACHMENT_GENERATION='... exact P1 attachment generation ...'
export RESIDENT_SANDBOX_ID='... exact resident-sandbox ID ...'
export RESIDENT_SANDBOX_GENERATION='... exact resident-sandbox generation ...'
```

The logical IDs above are correlation labels in this receipt. Their presence does not prove that the observed physical disk belongs to them.

Create a private scratch directory:

```bash
umask 077
PRIVATE_RECEIPT_DIR="$(mktemp -d)"
LIMACTL="$(command -v limactl)"
test -n "$LIMACTL"
git rev-parse HEAD > "$PRIVATE_RECEIPT_DIR/smolrunner-commit.txt"
"$LIMACTL" --version > "$PRIVATE_RECEIPT_DIR/lima-version.txt"
```

## 1. Capture installed Lima disk JSON

Record the installed command surface first:

```bash
"$LIMACTL" --help > "$PRIVATE_RECEIPT_DIR/limactl-help.txt"
"$LIMACTL" disk --help > "$PRIVATE_RECEIPT_DIR/limactl-disk-help.txt"
```

Use those exact help files to choose the installed version's read-only JSON disk-inventory command. Save the selected result as:

```text
$PRIVATE_RECEIPT_DIR/lima-disk.json
```

Keep the exact command in a private adjacent note. The repository collector accepts the JSON as opaque evidence and does not encode a disk-list flag or field-name guess.

Use the same direct inspection to select `DISK_DIRECTORY`. The collector never derives the directory from a disk name.

## 2. Capture the resident instance

```bash
HOME=/var/empty \
LIMA_HOME="$LIMA_HOME" \
LANG=C \
LC_ALL=C \
PATH=/usr/bin:/bin:/usr/sbin:/sbin \
"$LIMACTL" --tty=false list --format=json --all-fields "$INSTANCE" \
  > "$PRIVATE_RECEIPT_DIR/resident-instance.json"
```

## 3. Capture guest filesystem-device evidence

```bash
HOME=/var/empty \
LIMA_HOME="$LIMA_HOME" \
LANG=C \
LC_ALL=C \
PATH=/usr/bin:/bin:/usr/sbin:/sbin \
"$LIMACTL" --tty=false shell "$INSTANCE" -- \
  /usr/bin/stat -Lc '%d:%i' -- "$PROJECT_MOUNT" \
  > "$PRIVATE_RECEIPT_DIR/guest-project-stat.txt"

HOME=/var/empty \
LIMA_HOME="$LIMA_HOME" \
LANG=C \
LC_ALL=C \
PATH=/usr/bin:/bin:/usr/sbin:/sbin \
"$LIMACTL" --tty=false shell "$INSTANCE" -- \
  /usr/bin/cat /proc/self/mountinfo \
  > "$PRIVATE_RECEIPT_DIR/guest-mountinfo.txt"
```

The parser requires exactly one decoded mountinfo row for `PROJECT_MOUNT`. The Linux `%d` device is decoded to major/minor and must equal the selected mountinfo row's major/minor.

## 4. Capture descriptor-bound Lima host identity

```bash
SMOLRUNNER_TEST_LIMA_HOME="$LIMA_HOME" \
SMOLRUNNER_TEST_LIMA_INSTANCE="$INSTANCE" \
SMOLRUNNER_TEST_GUEST_CACHE_PATH="$GUEST_CACHE_PATH" \
cargo test --locked --test project_disk_physical_receipt_host_identity \
  -- --ignored --nocapture \
  | sed -n 's/^SMOLRUNNER_PROJECT_DISK_HOST_IDENTITY_V1 //p' \
  > "$PRIVATE_RECEIPT_DIR/lima-host-identity.json"

test "$(wc -l < "$PRIVATE_RECEIPT_DIR/lima-host-identity.json" | tr -d ' ')" = 1
```

The emitted JSON contains bounded identity fields only. It is still observation evidence, not resident or project-disk authority.

## 5. Capture the standalone-disk directory

Choose a fresh output path. The collector creates it mode `0600` and refuses overwrite.

```bash
python3 scripts/project_disk_physical_receipt.py capture \
  --host-identity-json "$PRIVATE_RECEIPT_DIR/lima-host-identity.json" \
  --disk-directory "$DISK_DIRECTORY" \
  --lima-disk-json "$PRIVATE_RECEIPT_DIR/lima-disk.json" \
  --resident-instance-json "$PRIVATE_RECEIPT_DIR/resident-instance.json" \
  --resident-sandbox-instance "$INSTANCE" \
  --guest-project-stat "$PRIVATE_RECEIPT_DIR/guest-project-stat.txt" \
  --guest-mountinfo "$PRIVATE_RECEIPT_DIR/guest-mountinfo.txt" \
  --guest-project-mountpoint "$PROJECT_MOUNT" \
  --project-identity "$PROJECT_IDENTITY" \
  --project-disk-id "$PROJECT_DISK_ID" \
  --project-disk-generation "$PROJECT_DISK_GENERATION" \
  --project-disk-revision "$PROJECT_DISK_REVISION" \
  --attachment-generation "$ATTACHMENT_GENERATION" \
  --resident-sandbox-id "$RESIDENT_SANDBOX_ID" \
  --resident-sandbox-generation "$RESIDENT_SANDBOX_GENERATION" \
  --output "$PRIVATE_RECEIPT_DIR/project-disk-first-pass.json"

python3 scripts/project_disk_physical_receipt.py validate \
  "$PRIVATE_RECEIPT_DIR/project-disk-first-pass.json" \
  > /dev/null
```

The first pass records every bounded direct entry as opaque evidence, including file kind, exact name bytes, device/inode, owner/mode/link count, logical and allocated bytes, timestamps, and platform birthtime/generation fields when available. Symlink targets are retained exactly.

The directory is opened component-by-component with no-follow semantics. The collector holds and rechecks the directory and direct entries before publication. On macOS, `/var/...` is accepted only after proving the root-owned `/var -> /private/var` compatibility alias; other intermediate symlinks are refused.

## 6. Optional bounded metadata read

If the first pass shows a small regular metadata entry whose bytes are needed to understand the observed layout, rerun with:

```text
--read-small-entry EXACT_OBSERVED_METADATA_ENTRY
```

The file must be a captured direct regular entry no larger than 4096 bytes. The held file descriptor is checked before and after the read.

## 7. Add explicit observed roles

After the first pass establishes the installed layout, rerun with exact direct names selected from that receipt:

```text
--observed-backing-entry EXACT_OBSERVED_BACKING_ENTRY
--observed-lock-entry EXACT_OBSERVED_LOCK_ENTRY
```

These are operator observation labels only. The collector applies no filename convention and does not turn the labels into ownership.

## Evidence this collector can supply

A reviewed receipt can establish:

1. exact descriptor-bound resident VZ host observation identity;
2. exact standalone-disk directory and bounded direct-entry set;
3. observed backing logical/allocated-byte metadata;
4. observed lock representation and target/content when explicitly selected;
5. installed Lima disk JSON and resident-instance JSON;
6. guest project-mount stat plus mountinfo device correlation;
7. logical project/disk/attachment/sandbox IDs as declared comparison labels;
8. same-name entry/directory replacement detection during the held observation window;
9. Darwin birthtime/generation observations needed by #699's physical-identity evaluation.

## Current interpretation

The current M6 programme deliberately keeps three facts separate:

- `ProjectDiskLimaSourceIdentity` from #691/#713 identifies the configured canonical Lima namespace and does not prove physical continuity;
- #699 owns the stronger durable physical-source/disk/backing identity policy needed across restart and inode reuse;
- this receipt is independent evidence for review/physical acceptance and is never fed into a runtime authority constructor.

#706/#628 may use the collector to inspect the final physical chain, while the production #640/#589 path must mint its runtime proof from current durable authority plus fresh guest evidence inside the reviewed live transaction.

## Stop rules

Stop and report ambiguity when an expected entry role is unknown, an observed object changes, disk/instance evidence disagrees, the guest mountpoint has zero or multiple exact mountinfo rows, or the current physical-identity policy cannot distinguish the replacement class under test.

Do not unlock, format, attach, detach, mount, delete, or broadly clean up an object because this diagnostic receipt exists.
