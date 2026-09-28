//! Bounded filesystem-metadata observation of one checkout-local Cargo target.
//!
//! The observer follows no symlinks, reads no file contents, emits no paths, and performs no
//! mutation. Visible allocated blocks are not exclusive-byte or future-reclaim evidence: reflinks,
//! sparse extents, and hardlinks outside the observed tree remain explicit limitations.

use std::collections::BTreeMap;
use std::fmt;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::Path;

use rustix::fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

pub const CARGO_TARGET_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_CARGO_TARGET_OBSERVATION_ENTRIES: u64 = 2_000_000;
pub const MAX_CARGO_TARGET_OBSERVATION_DEPTH: u16 = 64;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NOATIME)
    .union(OFlags::CLOEXEC);
const ALLOCATION_BLOCK_BYTES: u64 = 512;
const TARGET_ID_DOMAIN: &[u8] = b"glaeda-cargo-target-materialization-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ObservedTimestamp {
    seconds: i64,
    nanoseconds: i64,
}

impl ObservedTimestamp {
    fn from_stat(stat: &Stat) -> Result<Self, CargoTargetObservationError> {
        let nanoseconds = i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_shape())?;
        if !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(unsafe_shape());
        }
        Ok(Self {
            seconds: stat.st_mtime,
            nanoseconds,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetAllocationScope {
    VisibleFilesystemBlocksNotExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetHardlinkCoverage {
    CompleteWithinObservedTree,
    ExternalLinksPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RustcInfoObservation {
    Absent,
    Observed {
        size_bytes: u64,
        modified: ObservedTimestamp,
    },
    UnsupportedObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CargoTargetState {
    Absent,
    Present {
        target_id: Sha256Digest,
        entry_count: u64,
        directory_count: u64,
        unique_nondirectory_object_count: u64,
        logical_bytes: u64,
        allocated_bytes: u64,
        allocation_scope: CargoTargetAllocationScope,
        latest_modified: ObservedTimestamp,
        target_owner_matches_checkout: bool,
        all_entries_match_target_owner: bool,
        hardlink_coverage: CargoTargetHardlinkCoverage,
        rustc_info: RustcInfoObservation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetObservation {
    schema_version: u8,
    state: CargoTargetState,
}

impl CargoTargetObservation {
    fn absent() -> Self {
        Self {
            schema_version: CARGO_TARGET_OBSERVATION_SCHEMA_VERSION,
            state: CargoTargetState::Absent,
        }
    }

    fn present(
        checkout: &BoundDirectory,
        target: &BoundDirectory,
        totals: TreeTotals,
    ) -> Result<Self, CargoTargetObservationError> {
        let hardlink_coverage = if totals
            .objects
            .values()
            .any(|object| object.observed_links != object.snapshot.link_count)
        {
            CargoTargetHardlinkCoverage::ExternalLinksPresent
        } else {
            CargoTargetHardlinkCoverage::CompleteWithinObservedTree
        };
        Ok(Self {
            schema_version: CARGO_TARGET_OBSERVATION_SCHEMA_VERSION,
            state: CargoTargetState::Present {
                target_id: target.snapshot.target_id()?,
                entry_count: totals.entry_count,
                directory_count: totals.directory_count,
                unique_nondirectory_object_count: u64::try_from(totals.objects.len())
                    .map_err(|_| too_large())?,
                logical_bytes: totals.logical_bytes,
                allocated_bytes: totals.allocated_bytes,
                allocation_scope: CargoTargetAllocationScope::VisibleFilesystemBlocksNotExclusive,
                latest_modified: totals.latest_modified,
                target_owner_matches_checkout: target.snapshot.uid == checkout.snapshot.uid,
                all_entries_match_target_owner: totals.all_entries_match_target_owner,
                hardlink_coverage,
                rustc_info: totals.rustc_info,
            },
        })
    }

    #[must_use]
    pub const fn state(&self) -> &CargoTargetState {
        &self.state
    }
}

/// Observe the fixed `target` child of one already-validated checkout root.
///
/// # Errors
///
/// Refuses unreadable or aliased roots, symlinked/non-directory targets, followed or
/// cross-filesystem directories, unsupported objects, target replacement, bound excess, and
/// arithmetic overflow. Errors never contain the supplied path or child names.
pub fn observe_cargo_target(
    checkout: &Path,
) -> Result<CargoTargetObservation, CargoTargetObservationError> {
    let checkout = BoundDirectory::open_root(checkout)?;
    let checkout_before = checkout.snapshot;
    let target_name = c"target";
    let target_stat =
        match rustix_fs::statat(checkout.fd.as_fd(), target_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => {
                checkout.revalidate_against(checkout_before)?;
                return Ok(CargoTargetObservation::absent());
            }
            Err(_) => return Err(unreadable()),
        };
    if !FileType::from_raw_mode(target_stat.st_mode).is_dir()
        || target_stat.st_dev != checkout.snapshot.device
    {
        return Err(unsafe_shape());
    }
    let target = BoundDirectory::open_child(&checkout.fd, target_name)?;
    if !same_directory_identity(&target_stat, &target.snapshot) {
        return Err(changed());
    }
    let mut totals = TreeTotals::new(&target.snapshot)?;
    observe_directory(&target, target.snapshot.device, 0, &mut totals)?;
    target.revalidate()?;
    let rebound = BoundDirectory::open_child(&checkout.fd, target_name)?;
    if rebound.snapshot != target.snapshot {
        return Err(changed());
    }
    checkout.revalidate_against(checkout_before)?;
    CargoTargetObservation::present(&checkout, &target, totals)
}

#[derive(Debug)]
struct TreeTotals {
    entry_count: u64,
    directory_count: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    latest_modified: ObservedTimestamp,
    all_entries_match_target_owner: bool,
    target_owner: u32,
    objects: BTreeMap<PhysicalObjectIdentity, ObservedPhysicalObject>,
    rustc_info: RustcInfoObservation,
}

impl TreeTotals {
    fn new(target: &DirectorySnapshot) -> Result<Self, CargoTargetObservationError> {
        let mut totals = Self {
            entry_count: 0,
            directory_count: 0,
            logical_bytes: 0,
            allocated_bytes: 0,
            latest_modified: target.modified,
            all_entries_match_target_owner: true,
            target_owner: target.uid,
            objects: BTreeMap::new(),
            rustc_info: RustcInfoObservation::Absent,
        };
        totals.add_directory(target)?;
        Ok(totals)
    }

    fn add_directory(
        &mut self,
        directory: &DirectorySnapshot,
    ) -> Result<(), CargoTargetObservationError> {
        self.entry_count = self.entry_count.checked_add(1).ok_or_else(too_large)?;
        self.directory_count = self.directory_count.checked_add(1).ok_or_else(too_large)?;
        self.check_entry_bound()?;
        self.add_allocated_blocks(directory.blocks)?;
        self.latest_modified = self.latest_modified.max(directory.modified);
        self.all_entries_match_target_owner &= directory.uid == self.target_owner;
        Ok(())
    }

    fn add_object(
        &mut self,
        snapshot: ObjectSnapshot,
        is_root_rustc_info: bool,
    ) -> Result<(), CargoTargetObservationError> {
        self.entry_count = self.entry_count.checked_add(1).ok_or_else(too_large)?;
        self.check_entry_bound()?;
        self.latest_modified = self.latest_modified.max(snapshot.modified);
        self.all_entries_match_target_owner &= snapshot.uid == self.target_owner;
        if is_root_rustc_info {
            self.rustc_info = if snapshot.file_type == ObservedFileType::Regular {
                RustcInfoObservation::Observed {
                    size_bytes: snapshot.size,
                    modified: snapshot.modified,
                }
            } else {
                RustcInfoObservation::UnsupportedObject
            };
        }
        let identity = PhysicalObjectIdentity {
            device: snapshot.device,
            inode: snapshot.inode,
        };
        if let Some(observed) = self.objects.get_mut(&identity) {
            if observed.snapshot != snapshot {
                return Err(changed());
            }
            observed.observed_links = observed
                .observed_links
                .checked_add(1)
                .ok_or_else(too_large)?;
            return Ok(());
        }
        self.logical_bytes = self
            .logical_bytes
            .checked_add(snapshot.size)
            .ok_or_else(overflow)?;
        self.add_allocated_blocks(snapshot.blocks)?;
        self.objects.insert(
            identity,
            ObservedPhysicalObject {
                snapshot,
                observed_links: 1,
            },
        );
        Ok(())
    }

    fn add_allocated_blocks(&mut self, blocks: u64) -> Result<(), CargoTargetObservationError> {
        let bytes = blocks
            .checked_mul(ALLOCATION_BLOCK_BYTES)
            .ok_or_else(overflow)?;
        self.allocated_bytes = self
            .allocated_bytes
            .checked_add(bytes)
            .ok_or_else(overflow)?;
        Ok(())
    }

    fn check_entry_bound(&self) -> Result<(), CargoTargetObservationError> {
        if self.entry_count > MAX_CARGO_TARGET_OBSERVATION_ENTRIES {
            Err(too_large())
        } else {
            Ok(())
        }
    }
}

fn observe_directory(
    directory: &BoundDirectory,
    target_device: u64,
    depth: u16,
    totals: &mut TreeTotals,
) -> Result<(), CargoTargetObservationError> {
    if depth > MAX_CARGO_TARGET_OBSERVATION_DEPTH {
        return Err(too_large());
    }
    let before = directory.snapshot;
    let mut entries = Dir::read_from(&directory.fd).map_err(|_| unreadable())?;
    for entry in &mut entries {
        let entry = entry.map_err(|_| unreadable())?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let first = rustix_fs::statat(directory.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unreadable())?;
        if first.st_dev != target_device {
            return Err(unsafe_shape());
        }
        if FileType::from_raw_mode(first.st_mode).is_dir() {
            let child = BoundDirectory::open_child(&directory.fd, name)?;
            if !same_directory_identity(&first, &child.snapshot) {
                return Err(changed());
            }
            totals.add_directory(&child.snapshot)?;
            observe_directory(
                &child,
                target_device,
                depth.checked_add(1).ok_or_else(too_large)?,
                totals,
            )?;
            child.revalidate()?;
            let rebound = BoundDirectory::open_child(&directory.fd, name)?;
            if rebound.snapshot != child.snapshot {
                return Err(changed());
            }
            continue;
        }
        let snapshot = ObjectSnapshot::from_stat(&first)?;
        let second = rustix_fs::statat(directory.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed())?;
        if snapshot != ObjectSnapshot::from_stat(&second)? {
            return Err(changed());
        }
        totals.add_object(snapshot, depth == 0 && bytes == b".rustc_info.json")?;
    }
    directory.revalidate_against(before)
}

#[derive(Debug)]
struct BoundDirectory {
    fd: OwnedFd,
    snapshot: DirectorySnapshot,
}

impl BoundDirectory {
    fn open_root(path: &Path) -> Result<Self, CargoTargetObservationError> {
        let fd = rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| root_unavailable())?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn open_child(
        parent: &OwnedFd,
        name: impl rustix::path::Arg,
    ) -> Result<Self, CargoTargetObservationError> {
        let fd = rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| unreadable())?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate(&self) -> Result<(), CargoTargetObservationError> {
        self.revalidate_against(self.snapshot)
    }

    fn revalidate_against(
        &self,
        expected: DirectorySnapshot,
    ) -> Result<(), CargoTargetObservationError> {
        if DirectorySnapshot::from_fd(&self.fd).map_err(|_| changed())? != expected {
            return Err(changed());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectorySnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    blocks: u64,
    modified: ObservedTimestamp,
    ctime_seconds: i64,
    ctime_nanoseconds: i64,
}

impl DirectorySnapshot {
    fn from_fd(fd: &OwnedFd) -> Result<Self, CargoTargetObservationError> {
        Self::from_stat(&rustix_fs::fstat(fd).map_err(|_| unreadable())?)
    }

    fn from_stat(stat: &Stat) -> Result<Self, CargoTargetObservationError> {
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(unsafe_shape());
        }
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
            size: u64::try_from(stat.st_size).map_err(|_| unsafe_shape())?,
            blocks: u64::try_from(stat.st_blocks).map_err(|_| unsafe_shape())?,
            modified: ObservedTimestamp::from_stat(stat)?,
            ctime_seconds: stat.st_ctime,
            ctime_nanoseconds: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_shape())?,
        })
    }

    fn target_id(&self) -> Result<Sha256Digest, CargoTargetObservationError> {
        let mut hasher = Sha256::new();
        hasher.update(TARGET_ID_DOMAIN);
        hasher.update(self.device.to_be_bytes());
        hasher.update(self.inode.to_be_bytes());
        hasher.update(self.uid.to_be_bytes());
        hasher.update(self.ctime_seconds.to_be_bytes());
        hasher.update(self.ctime_nanoseconds.to_be_bytes());
        let digest = hasher.finalize();
        let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
        value.push_str(SHA256_PREFIX);
        for byte in digest {
            value.push(char::from(HEX[usize::from(byte >> 4)]));
            value.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Sha256Digest::parse(&value).map_err(|_| unsafe_shape())
    }
}

pub(crate) fn cargo_target_materialization_id(
    stat: &Stat,
) -> Result<Sha256Digest, CargoTargetObservationError> {
    DirectorySnapshot::from_stat(stat)?.target_id()
}

fn same_directory_identity(stat: &Stat, snapshot: &DirectorySnapshot) -> bool {
    FileType::from_raw_mode(stat.st_mode).is_dir()
        && stat.st_dev == snapshot.device
        && stat.st_ino == snapshot.inode
        && stat.st_mode == snapshot.mode
        && stat.st_uid == snapshot.uid
        && stat.st_gid == snapshot.gid
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PhysicalObjectIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservedPhysicalObject {
    snapshot: ObjectSnapshot,
    observed_links: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedFileType {
    Regular,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    blocks: u64,
    link_count: u64,
    modified: ObservedTimestamp,
    ctime_seconds: i64,
    ctime_nanoseconds: i64,
    file_type: ObservedFileType,
}

impl ObjectSnapshot {
    fn from_stat(stat: &Stat) -> Result<Self, CargoTargetObservationError> {
        let file_type = FileType::from_raw_mode(stat.st_mode);
        let file_type = if file_type.is_file() {
            ObservedFileType::Regular
        } else if file_type.is_symlink() {
            ObservedFileType::Symlink
        } else {
            return Err(unsafe_shape());
        };
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
            size: u64::try_from(stat.st_size).map_err(|_| unsafe_shape())?,
            blocks: u64::try_from(stat.st_blocks).map_err(|_| unsafe_shape())?,
            link_count: widen_link_count(stat.st_nlink),
            modified: ObservedTimestamp::from_stat(stat)?,
            ctime_seconds: stat.st_ctime,
            ctime_nanoseconds: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_shape())?,
            file_type,
        })
    }
}

fn widen_link_count<T: Into<u64>>(value: T) -> u64 {
    value.into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetObservationErrorKind {
    RootUnavailable,
    Unreadable,
    UnsafeShape,
    Changed,
    TooLarge,
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetObservationError {
    kind: CargoTargetObservationErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl CargoTargetObservationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for CargoTargetObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for CargoTargetObservationError {}

const fn error(
    kind: CargoTargetObservationErrorKind,
    code: &'static str,
    problem: &'static str,
) -> CargoTargetObservationError {
    CargoTargetObservationError {
        kind,
        code,
        problem,
    }
}

const fn root_unavailable() -> CargoTargetObservationError {
    error(
        CargoTargetObservationErrorKind::RootUnavailable,
        "cargo_target_root_unavailable",
        "checkout root is unavailable for Cargo target observation",
    )
}

const fn unreadable() -> CargoTargetObservationError {
    error(
        CargoTargetObservationErrorKind::Unreadable,
        "cargo_target_unreadable",
        "Cargo target metadata is unavailable",
    )
}

const fn unsafe_shape() -> CargoTargetObservationError {
    error(
        CargoTargetObservationErrorKind::UnsafeShape,
        "cargo_target_unsafe_shape",
        "Cargo target contains an unsupported filesystem shape",
    )
}

const fn changed() -> CargoTargetObservationError {
    error(
        CargoTargetObservationErrorKind::Changed,
        "cargo_target_changed",
        "Cargo target changed during observation",
    )
}

const fn too_large() -> CargoTargetObservationError {
    error(
        CargoTargetObservationErrorKind::TooLarge,
        "cargo_target_too_large",
        "Cargo target exceeds the reviewed observation bound",
    )
}

const fn overflow() -> CargoTargetObservationError {
    error(
        CargoTargetObservationErrorKind::Overflow,
        "cargo_target_overflow",
        "Cargo target byte totals exceed the reviewed representation",
    )
}
