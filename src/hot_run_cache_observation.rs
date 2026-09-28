//! Bounded filesystem-metadata observation of one explicitly supplied hot-run cache root.
//!
//! The observer follows no symlinks, reads no cache file contents, emits no paths, and performs no
//! mutation. It may conservatively correlate recognized lock-file identities with a bounded
//! `/proc/locks` snapshot. A matching opaque name does not establish ownership, and absence from
//! the kernel snapshot never establishes that a state is unlocked.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fmt;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::Path;

use rustix::fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use serde::Serialize;

use crate::cache_inventory::{
    CacheInventoryAuthority, CacheInventoryDocument, CacheInventoryError, CacheInventoryReport,
    CacheInventorySummary, CacheReportOperation, CacheStateId, CacheStateReport,
    MAX_CACHE_INVENTORY_STATES, build_local_hot_run_cache_report,
};
use crate::linux_kernel_file_locks::{KernelFileIdentity, observe_exclusive_whole_file_flocks};

pub const MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES: u64 = 2_000_000;
pub const MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH: u16 = 64;
pub const HOT_RUN_CACHE_OBSERVATION_REPORT_SCHEMA_VERSION: u8 = 2;
pub const MAX_HOT_RUN_CACHE_LOCK_CANDIDATES: usize = 512;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NOATIME)
    .union(OFlags::CLOEXEC);
const ALLOCATION_BLOCK_BYTES: u64 = 512;
const RUNTIME_STATE_PREFIX: &[u8] = b"runtime-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotRunCacheObservationCompleteness {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotRunCacheObservationProblem {
    PermissionDenied,
    UnsupportedNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotRunCacheObservation {
    Complete(CacheInventoryDocument),
    Partial {
        state_count: u32,
        problem: HotRunCacheObservationProblem,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotRunCacheObservationSummary {
    state_count: u32,
    in_use_count: Option<u32>,
    warm_count: Option<u32>,
    reclaimable_count: Option<u32>,
    quarantined_count: Option<u32>,
    unknown_count: Option<u32>,
    logical_bytes: Option<u64>,
    allocated_bytes: Option<u64>,
    reclaimable_allocated_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotRunCacheObservationReport {
    schema_version: u8,
    authority: CacheInventoryAuthority,
    operation: CacheReportOperation,
    mutation_performed: bool,
    completeness: HotRunCacheObservationCompleteness,
    summary: HotRunCacheObservationSummary,
    states: Vec<CacheStateReport>,
    problems: Vec<HotRunCacheObservationProblem>,
}

impl HotRunCacheObservationReport {
    #[must_use]
    pub const fn completeness(&self) -> HotRunCacheObservationCompleteness {
        self.completeness
    }

    #[must_use]
    pub const fn summary(&self) -> &HotRunCacheObservationSummary {
        &self.summary
    }

    #[must_use]
    pub fn states(&self) -> &[CacheStateReport] {
        &self.states
    }

    #[must_use]
    pub fn problems(&self) -> &[HotRunCacheObservationProblem] {
        &self.problems
    }
}

impl HotRunCacheObservationSummary {
    #[must_use]
    pub const fn state_count(&self) -> u32 {
        self.state_count
    }

    #[must_use]
    pub const fn logical_bytes(&self) -> Option<u64> {
        self.logical_bytes
    }

    #[must_use]
    pub const fn allocated_bytes(&self) -> Option<u64> {
        self.allocated_bytes
    }
}

/// Convert one filesystem observation into the stable path-free command report.
///
/// # Errors
///
/// Returns an error only when a complete observation cannot be aggregated by the cache
/// classifier. Partial observations carry no byte or lifecycle evidence into that classifier.
pub fn build_hot_run_cache_observation_report(
    observation: HotRunCacheObservation,
) -> Result<HotRunCacheObservationReport, CacheInventoryError> {
    match observation {
        HotRunCacheObservation::Complete(document) => {
            let classified = build_local_hot_run_cache_report(&document)?;
            Ok(complete_report(&classified))
        }
        HotRunCacheObservation::Partial {
            state_count,
            problem,
        } => Ok(HotRunCacheObservationReport {
            schema_version: HOT_RUN_CACHE_OBSERVATION_REPORT_SCHEMA_VERSION,
            authority: CacheInventoryAuthority::LocalHotRunFilesystemObservation,
            operation: CacheReportOperation::Status,
            mutation_performed: false,
            completeness: HotRunCacheObservationCompleteness::Partial,
            summary: HotRunCacheObservationSummary {
                state_count,
                in_use_count: None,
                warm_count: None,
                reclaimable_count: None,
                quarantined_count: None,
                unknown_count: None,
                logical_bytes: None,
                allocated_bytes: None,
                reclaimable_allocated_bytes: None,
            },
            states: Vec::new(),
            problems: vec![problem],
        }),
    }
}

fn complete_report(classified: &CacheInventoryReport) -> HotRunCacheObservationReport {
    let summary = classified.summary();
    HotRunCacheObservationReport {
        schema_version: HOT_RUN_CACHE_OBSERVATION_REPORT_SCHEMA_VERSION,
        authority: classified.authority(),
        operation: classified.operation(),
        mutation_performed: false,
        completeness: HotRunCacheObservationCompleteness::Complete,
        summary: complete_summary(summary),
        states: classified.states().to_vec(),
        problems: Vec::new(),
    }
}

fn complete_summary(summary: &CacheInventorySummary) -> HotRunCacheObservationSummary {
    HotRunCacheObservationSummary {
        state_count: summary.state_count(),
        in_use_count: Some(summary.in_use_count()),
        warm_count: Some(summary.warm_count()),
        reclaimable_count: Some(summary.reclaimable_count()),
        quarantined_count: Some(summary.quarantined_count()),
        unknown_count: Some(summary.unknown_count()),
        logical_bytes: Some(summary.logical_bytes()),
        allocated_bytes: Some(summary.allocated_bytes()),
        reclaimable_allocated_bytes: Some(summary.reclaimable_allocated_bytes()),
    }
}

#[must_use]
pub fn render_hot_run_cache_observation_human(report: &HotRunCacheObservationReport) -> String {
    let reclaimable = report
        .summary
        .reclaimable_count
        .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    let reclaimable_bytes = report
        .summary
        .reclaimable_allocated_bytes
        .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    let mut output = format!(
        "cache status\nauthority: {}\ncompleteness: {}\nstates={}, reclaimable={}, reclaimable_allocated_bytes={}, mutation_performed=false\n",
        report.authority.as_str(),
        match report.completeness {
            HotRunCacheObservationCompleteness::Complete => "complete",
            HotRunCacheObservationCompleteness::Partial => "partial",
        },
        report.summary.state_count,
        reclaimable,
        reclaimable_bytes,
    );
    for state in &report.states {
        let reasons = if state.reasons().is_empty() {
            "none".to_owned()
        } else {
            state
                .reasons()
                .iter()
                .map(|reason| reason.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        output.push_str(&format!(
            "{}: {} (allocated_bytes={}, reasons={})\n",
            state.state_id().as_str(),
            state.classification().as_str(),
            state.allocated_bytes(),
            reasons,
        ));
    }
    if !report.problems.is_empty() {
        let problems = report
            .problems
            .iter()
            .map(|problem| match problem {
                HotRunCacheObservationProblem::PermissionDenied => "permission_denied",
                HotRunCacheObservationProblem::UnsupportedNode => "unsupported_node",
            })
            .collect::<Vec<_>>()
            .join(",");
        output.push_str(&format!("problems: {problems}\n"));
    }
    output
}

/// Observe one explicit hot-run cache root without reading file contents.
///
/// # Errors
///
/// A stable top-level state set with protected interiors or nested special nodes produces a
/// partial observation with unknown bytes. Refuses missing or unreadable roots, non-directory
/// top-level entries, non-hex state names, followed or cross-filesystem directories, cross-state
/// hardlinks, drift, limit excess, and arithmetic overflow. Errors never contain the supplied path
/// or child names.
pub fn observe_hot_run_cache(
    root: &Path,
) -> Result<HotRunCacheObservation, HotRunCacheObservationError> {
    let root = BoundDirectory::open_root(root)?;
    let root_before = root.snapshot;
    let mut names = state_names(&root.fd)?;
    names.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

    for name in &names {
        let path_stat =
            rustix_fs::statat(root.fd.as_fd(), name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| unreadable())?;
        if !FileType::from_raw_mode(path_stat.st_mode).is_dir()
            || path_stat.st_dev != root.snapshot.device
        {
            return Err(unsafe_shape());
        }
        CacheStateId::parse(std::str::from_utf8(name.as_bytes()).map_err(|_| unsafe_shape())?)
            .map_err(|_| unsafe_shape())?;
    }

    let mut global_objects = BTreeMap::new();
    let mut entries_seen = 0_u64;
    let mut lock_discovery_entries_seen = 0_u64;
    let mut lock_candidates_seen = 0_usize;
    let mut states = Vec::with_capacity(names.len());
    for (state_ordinal, name) in names.iter().enumerate() {
        let path_stat =
            match rustix_fs::statat(root.fd.as_fd(), name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(path_stat) => path_stat,
                Err(errno) => {
                    return partial_or_error(&root, root_before, &names, read_error(errno));
                }
            };
        if !FileType::from_raw_mode(path_stat.st_mode).is_dir()
            || path_stat.st_dev != root.snapshot.device
        {
            return Err(unsafe_shape());
        }
        let state = match BoundDirectory::open_child(&root.fd, name.as_c_str()) {
            Ok(state) => state,
            Err(error) => return partial_or_error(&root, root_before, &names, error),
        };
        if !same_directory_identity(&path_stat, &state.snapshot) {
            return Err(changed());
        }
        let mut totals = TreeTotals::default();
        if let Err(error) = observe_directory(
            &state,
            root.snapshot.device,
            0,
            state_ordinal,
            &mut entries_seen,
            &mut totals,
            &mut global_objects,
        ) {
            return partial_or_error(&root, root_before, &names, error);
        }
        let lock_candidates = match expected_lock_identities(
            &state,
            root.snapshot.device,
            &mut lock_discovery_entries_seen,
            &mut lock_candidates_seen,
        ) {
            Ok(lock_candidates) => lock_candidates,
            Err(error) => return partial_or_error(&root, root_before, &names, error),
        };
        state.revalidate()?;
        let rebound = match BoundDirectory::open_child(&root.fd, name.as_c_str()) {
            Ok(rebound) => rebound,
            Err(error) => return partial_or_error(&root, root_before, &names, error),
        };
        if rebound.snapshot != state.snapshot {
            return Err(changed());
        }
        let state_id =
            CacheStateId::parse(std::str::from_utf8(name.as_bytes()).map_err(|_| unsafe_shape())?)
                .map_err(|_| unsafe_shape())?;
        states.push(ObservedHotRunState {
            state_id,
            logical_bytes: totals.logical_bytes,
            allocated_bytes: totals.allocated_bytes,
            lock_candidates,
        });
    }

    root.revalidate()?;
    if root.snapshot != root_before {
        return Err(changed());
    }
    let held_locks = observe_exclusive_whole_file_flocks();
    let states = states
        .into_iter()
        .map(|state| {
            let active_lock = held_locks.as_ref().and_then(|held| {
                state
                    .lock_candidates
                    .iter()
                    .any(|candidate| held.contains(&candidate.identity))
                    .then_some(true)
            });
            (
                state.state_id,
                state.logical_bytes,
                state.allocated_bytes,
                active_lock,
            )
        })
        .collect();
    let document =
        CacheInventoryDocument::from_observed_hot_run_states(states).map_err(|_| unsafe_shape())?;
    Ok(HotRunCacheObservation::Complete(document))
}

fn partial_or_error(
    root: &BoundDirectory,
    root_before: DirectorySnapshot,
    expected_names: &[std::ffi::CString],
    error: HotRunCacheObservationError,
) -> Result<HotRunCacheObservation, HotRunCacheObservationError> {
    let problem = match error.kind {
        HotRunCacheObservationErrorKind::PermissionDenied => {
            HotRunCacheObservationProblem::PermissionDenied
        }
        HotRunCacheObservationErrorKind::UnsupportedNode => {
            HotRunCacheObservationProblem::UnsupportedNode
        }
        _ => return Err(error),
    };
    root.revalidate()?;
    if root.snapshot != root_before {
        return Err(changed());
    }
    let mut names_after = state_names(&root.fd)?;
    names_after.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    if names_after != expected_names {
        return Err(changed());
    }
    root.revalidate()?;
    if root.snapshot != root_before {
        return Err(changed());
    }
    let state_count = u32::try_from(expected_names.len()).map_err(|_| too_large())?;
    Ok(HotRunCacheObservation::Partial {
        state_count,
        problem,
    })
}

#[derive(Debug)]
struct ObservedHotRunState {
    state_id: CacheStateId,
    logical_bytes: u64,
    allocated_bytes: u64,
    lock_candidates: Vec<BoundLockFile>,
}

fn state_names(root: &OwnedFd) -> Result<Vec<std::ffi::CString>, HotRunCacheObservationError> {
    let mut directory = Dir::read_from(root).map_err(|_| unreadable())?;
    let mut names = Vec::new();
    for entry in &mut directory {
        let entry = entry.map_err(|_| unreadable())?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if bytes.len() != 64
            || bytes
                .iter()
                .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(unsafe_shape());
        }
        if names.len() >= MAX_CACHE_INVENTORY_STATES {
            return Err(too_large());
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

fn observe_directory(
    directory: &BoundDirectory,
    root_device: u64,
    depth: u16,
    state_ordinal: usize,
    entries_seen: &mut u64,
    totals: &mut TreeTotals,
    global_objects: &mut BTreeMap<PhysicalObjectIdentity, usize>,
) -> Result<(), HotRunCacheObservationError> {
    if depth > MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH {
        return Err(too_large());
    }
    totals.add_directory(directory.snapshot.blocks)?;
    *entries_seen = entries_seen.checked_add(1).ok_or_else(too_large)?;
    if *entries_seen > MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES {
        return Err(too_large());
    }

    let before = directory.snapshot;
    let mut entries = Dir::read_from(&directory.fd).map_err(read_error)?;
    for entry in &mut entries {
        let entry = entry.map_err(read_error)?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let first = rustix_fs::statat(directory.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(read_error)?;
        if first.st_dev != root_device {
            return Err(unsafe_shape());
        }
        if FileType::from_raw_mode(first.st_mode).is_dir() {
            let child = BoundDirectory::open_child(&directory.fd, name)?;
            if !same_directory_identity(&first, &child.snapshot) {
                return Err(changed());
            }
            observe_directory(
                &child,
                root_device,
                depth.checked_add(1).ok_or_else(too_large)?,
                state_ordinal,
                entries_seen,
                totals,
                global_objects,
            )?;
            child.revalidate()?;
            let rebound = BoundDirectory::open_child(&directory.fd, name)?;
            if rebound.snapshot != child.snapshot {
                return Err(changed());
            }
            continue;
        }

        let file_type = FileType::from_raw_mode(first.st_mode);
        if !file_type.is_file() && !file_type.is_symlink() {
            return Err(unsupported_node());
        }
        let snapshot = ObjectSnapshot::from_stat(&first)?;
        let second = rustix_fs::statat(directory.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|errno| {
                if matches!(errno, Errno::ACCESS | Errno::PERM) {
                    read_error(errno)
                } else {
                    changed()
                }
            })?;
        if snapshot != ObjectSnapshot::from_stat(&second)? {
            return Err(changed());
        }
        *entries_seen = entries_seen.checked_add(1).ok_or_else(too_large)?;
        if *entries_seen > MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES {
            return Err(too_large());
        }
        let identity = PhysicalObjectIdentity {
            device: snapshot.device,
            inode: snapshot.inode,
        };
        match global_objects.get(&identity) {
            Some(owner) if *owner == state_ordinal => continue,
            Some(_) => return Err(ambiguous_hardlink()),
            None => {
                global_objects.insert(identity, state_ordinal);
            }
        }
        totals.add_object(snapshot.size, snapshot.blocks)?;
    }
    directory.revalidate_against(before)
}

fn expected_lock_identities(
    state: &BoundDirectory,
    root_device: u64,
    entries_seen: &mut u64,
    candidates_seen: &mut usize,
) -> Result<Vec<BoundLockFile>, HotRunCacheObservationError> {
    let mut identities = Vec::new();
    let lock_name = c"lock";
    if let Some(identity) = expected_lock_identity(&state.fd, lock_name, root_device)? {
        push_lock_candidate(&mut identities, identity, candidates_seen)?;
    }

    let before = state.snapshot;
    let mut entries = Dir::read_from(&state.fd).map_err(read_error)?;
    for entry in &mut entries {
        let entry = entry.map_err(read_error)?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        *entries_seen = entries_seen.checked_add(1).ok_or_else(too_large)?;
        if *entries_seen > MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES {
            return Err(too_large());
        }
        if !is_runtime_state_name(bytes) {
            continue;
        }
        let first = rustix_fs::statat(state.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed())?;
        if first.st_dev != root_device || !FileType::from_raw_mode(first.st_mode).is_dir() {
            continue;
        }
        let runtime = BoundDirectory::open_child(&state.fd, name)?;
        if !same_directory_identity(&first, &runtime.snapshot) {
            return Err(changed());
        }
        if let Some(identity) = expected_lock_identity(&runtime.fd, lock_name, root_device)? {
            push_lock_candidate(&mut identities, identity, candidates_seen)?;
        }
        runtime.revalidate()?;
        let rebound = BoundDirectory::open_child(&state.fd, name)?;
        if rebound.snapshot != runtime.snapshot {
            return Err(changed());
        }
    }
    state.revalidate_against(before)?;
    Ok(identities)
}

fn push_lock_candidate(
    candidates: &mut Vec<BoundLockFile>,
    candidate: BoundLockFile,
    candidates_seen: &mut usize,
) -> Result<(), HotRunCacheObservationError> {
    *candidates_seen = candidates_seen.checked_add(1).ok_or_else(too_large)?;
    if *candidates_seen > MAX_HOT_RUN_CACHE_LOCK_CANDIDATES {
        return Err(too_large());
    }
    candidates.push(candidate);
    Ok(())
}

fn expected_lock_identity(
    parent: &OwnedFd,
    name: &CStr,
    root_device: u64,
) -> Result<Option<BoundLockFile>, HotRunCacheObservationError> {
    let first = match rustix_fs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Ok(None),
    };
    if first.st_dev != root_device
        || !FileType::from_raw_mode(first.st_mode).is_file()
        || first.st_nlink != 1
    {
        return Ok(None);
    }
    let fd = rustix_fs::openat(
        parent.as_fd(),
        name,
        OFlags::PATH.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .map_err(|_| changed())?;
    let held = rustix_fs::fstat(&fd).map_err(|_| changed())?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || held.st_nlink != 1
        || held.st_dev != root_device
        || ObjectSnapshot::from_stat(&first)? != ObjectSnapshot::from_stat(&held)?
    {
        return Err(changed());
    }
    let rebound = rustix_fs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| changed())?;
    if ObjectSnapshot::from_stat(&held)? != ObjectSnapshot::from_stat(&rebound)? {
        return Err(changed());
    }
    Ok(Some(BoundLockFile {
        identity: KernelFileIdentity {
            device: held.st_dev,
            inode: held.st_ino,
        },
        _fd: fd,
    }))
}

fn is_runtime_state_name(name: &[u8]) -> bool {
    name.len() == RUNTIME_STATE_PREFIX.len() + 64
        && name.starts_with(RUNTIME_STATE_PREFIX)
        && name[RUNTIME_STATE_PREFIX.len()..]
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Debug)]
struct BoundLockFile {
    identity: KernelFileIdentity,
    _fd: OwnedFd,
}

#[derive(Debug, Default)]
struct TreeTotals {
    logical_bytes: u64,
    allocated_bytes: u64,
}

impl TreeTotals {
    fn add_directory(&mut self, blocks: u64) -> Result<(), HotRunCacheObservationError> {
        self.add_allocated_blocks(blocks)
    }

    fn add_object(&mut self, size: u64, blocks: u64) -> Result<(), HotRunCacheObservationError> {
        self.logical_bytes = self.logical_bytes.checked_add(size).ok_or_else(overflow)?;
        self.add_allocated_blocks(blocks)
    }

    fn add_allocated_blocks(&mut self, blocks: u64) -> Result<(), HotRunCacheObservationError> {
        let allocated = blocks
            .checked_mul(ALLOCATION_BLOCK_BYTES)
            .ok_or_else(overflow)?;
        self.allocated_bytes = self
            .allocated_bytes
            .checked_add(allocated)
            .ok_or_else(overflow)?;
        Ok(())
    }
}

#[derive(Debug)]
struct BoundDirectory {
    fd: OwnedFd,
    snapshot: DirectorySnapshot,
}

impl BoundDirectory {
    fn open_root(path: &Path) -> Result<Self, HotRunCacheObservationError> {
        let fd = rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| root_unavailable())?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn open_child(
        parent: &OwnedFd,
        name: impl rustix::path::Arg,
    ) -> Result<Self, HotRunCacheObservationError> {
        let fd = rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(read_error)?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate(&self) -> Result<(), HotRunCacheObservationError> {
        self.revalidate_against(self.snapshot)
    }

    fn revalidate_against(
        &self,
        expected: DirectorySnapshot,
    ) -> Result<(), HotRunCacheObservationError> {
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
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl DirectorySnapshot {
    fn from_fd(fd: &OwnedFd) -> Result<Self, HotRunCacheObservationError> {
        Self::from_stat(&rustix_fs::fstat(fd).map_err(|_| unreadable())?)
    }

    fn from_stat(stat: &Stat) -> Result<Self, HotRunCacheObservationError> {
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
            mtime: stat.st_mtime,
            mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_shape())?,
            ctime: stat.st_ctime,
            ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_shape())?,
        })
    }
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
struct ObjectSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    blocks: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl ObjectSnapshot {
    fn from_stat(stat: &Stat) -> Result<Self, HotRunCacheObservationError> {
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
            size: u64::try_from(stat.st_size).map_err(|_| unsafe_shape())?,
            blocks: u64::try_from(stat.st_blocks).map_err(|_| unsafe_shape())?,
            mtime: stat.st_mtime,
            mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_shape())?,
            ctime: stat.st_ctime,
            ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_shape())?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotRunCacheObservationErrorKind {
    RootUnavailable,
    Unreadable,
    PermissionDenied,
    UnsupportedNode,
    UnsafeShape,
    AmbiguousHardlink,
    Changed,
    TooLarge,
    ArithmeticOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotRunCacheObservationError {
    kind: HotRunCacheObservationErrorKind,
    message: &'static str,
}

impl HotRunCacheObservationError {
    #[must_use]
    pub const fn kind(&self) -> HotRunCacheObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.kind {
            HotRunCacheObservationErrorKind::RootUnavailable => {
                "hot_run_cache_observation_root_unavailable"
            }
            HotRunCacheObservationErrorKind::Unreadable => "hot_run_cache_observation_unreadable",
            HotRunCacheObservationErrorKind::PermissionDenied => {
                "hot_run_cache_observation_permission_denied"
            }
            HotRunCacheObservationErrorKind::UnsupportedNode => {
                "hot_run_cache_observation_unsupported_node"
            }
            HotRunCacheObservationErrorKind::UnsafeShape => {
                "hot_run_cache_observation_unsafe_shape"
            }
            HotRunCacheObservationErrorKind::AmbiguousHardlink => {
                "hot_run_cache_observation_ambiguous_hardlink"
            }
            HotRunCacheObservationErrorKind::Changed => "hot_run_cache_observation_changed",
            HotRunCacheObservationErrorKind::TooLarge => "hot_run_cache_observation_too_large",
            HotRunCacheObservationErrorKind::ArithmeticOverflow => {
                "hot_run_cache_observation_arithmetic_overflow"
            }
        }
    }
}

impl fmt::Display for HotRunCacheObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for HotRunCacheObservationError {}

const fn error(
    kind: HotRunCacheObservationErrorKind,
    message: &'static str,
) -> HotRunCacheObservationError {
    HotRunCacheObservationError { kind, message }
}

fn root_unavailable() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::RootUnavailable,
        "hot-run cache observation root is unavailable",
    )
}

fn unreadable() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::Unreadable,
        "hot-run cache observation could not read the reviewed tree",
    )
}

fn permission_denied() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::PermissionDenied,
        "hot-run cache observation found protected state",
    )
}

fn unsupported_node() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::UnsupportedNode,
        "hot-run cache observation found an unsupported node",
    )
}

fn read_error(errno: Errno) -> HotRunCacheObservationError {
    if matches!(errno, Errno::ACCESS | Errno::PERM) {
        permission_denied()
    } else {
        unreadable()
    }
}

fn unsafe_shape() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::UnsafeShape,
        "hot-run cache observation found an unsupported filesystem shape",
    )
}

fn ambiguous_hardlink() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::AmbiguousHardlink,
        "hot-run cache observation found cross-state physical attribution",
    )
}

fn changed() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::Changed,
        "hot-run cache observation changed during traversal",
    )
}

fn too_large() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::TooLarge,
        "hot-run cache observation exceeds the reviewed bound",
    )
}

fn overflow() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::ArithmeticOverflow,
        "hot-run cache observation byte aggregate overflows",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::fs::OpenOptions;
    use std::os::unix::fs::{MetadataExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::fs::{self as rustix_fs, FlockOperation};

    use crate::cache_inventory::{CacheStateClassification, CacheStateReason};

    use super::{
        HotRunCacheObservationCompleteness, HotRunCacheObservationErrorKind,
        HotRunCacheObservationProblem, MAX_HOT_RUN_CACHE_LOCK_CANDIDATES,
        MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH, build_hot_run_cache_observation_report,
        observe_hot_run_cache,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "glaeda-hot-run-cache-observation-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary observation root");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn state(&self, byte: char) -> PathBuf {
            let state = self.0.join(byte.to_string().repeat(64));
            fs::create_dir(&state).expect("create state");
            state
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn bytes(metadata: &fs::Metadata) -> (u64, u64) {
        (metadata.size(), metadata.blocks() * 512)
    }

    #[test]
    fn observes_metadata_once_per_hardlinked_object_and_never_follows_symlinks() {
        let root = TempRoot::new();
        let state = root.state('a');
        let data = state.join("data");
        fs::write(&data, b"abc").expect("write fixture");
        fs::hard_link(&data, state.join("alias")).expect("hardlink fixture");
        let outside_root = TempRoot::new();
        let outside = outside_root.path().join("outside");
        fs::write(&outside, vec![0_u8; 1_000_000]).expect("write outside fixture");
        let link = state.join("link");
        symlink(&outside, &link).expect("symlink fixture");

        let (_, directory_allocated) = bytes(&fs::metadata(&state).unwrap());
        let (data_logical, data_allocated) = bytes(&fs::metadata(&data).unwrap());
        let (link_logical, link_allocated) = bytes(&fs::symlink_metadata(&link).unwrap());
        let observation = observe_hot_run_cache(root.path()).expect("observe fixture");
        let report = build_hot_run_cache_observation_report(observation).expect("classify fixture");

        assert_eq!(
            report.completeness(),
            HotRunCacheObservationCompleteness::Complete
        );
        assert_eq!(report.summary().state_count(), 1);
        assert_eq!(
            report.summary().logical_bytes(),
            Some(data_logical + link_logical)
        );
        assert_eq!(
            report.summary().allocated_bytes(),
            Some(directory_allocated + data_allocated + link_allocated)
        );
        assert_eq!(report.states()[0].classification().as_str(), "unknown");
    }

    #[test]
    fn reports_nested_special_nodes_as_partial_without_byte_or_identity_evidence() {
        let root = TempRoot::new();
        let state = root.state('a');
        let private_name = "private-socket-name-do-not-print";
        let status = Command::new("/usr/bin/mkfifo")
            .arg(state.join(private_name))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "create fixture FIFO");

        let observation = observe_hot_run_cache(root.path()).expect("observe partial fixture");
        let report =
            build_hot_run_cache_observation_report(observation).expect("build partial report");

        assert_eq!(
            report.completeness(),
            HotRunCacheObservationCompleteness::Partial
        );
        assert_eq!(report.summary().state_count(), 1);
        assert_eq!(report.summary().logical_bytes(), None);
        assert_eq!(report.summary().allocated_bytes(), None);
        assert!(report.states().is_empty());
        assert_eq!(
            report.problems(),
            &[HotRunCacheObservationProblem::UnsupportedNode]
        );
        assert!(!format!("{report:?}").contains(private_name));
    }

    #[test]
    fn partial_observation_never_classifies_a_held_lock() {
        let root = TempRoot::new();
        let state = root.state('a');
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state.join("lock"))
            .expect("open held lock fixture");
        rustix_fs::flock(&lock, FlockOperation::LockExclusive).expect("hold lock fixture");
        let status = Command::new("/usr/bin/mkfifo")
            .arg(state.join("protected-shape"))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "create fixture FIFO");

        let observation = observe_hot_run_cache(root.path()).expect("observe partial fixture");
        let report =
            build_hot_run_cache_observation_report(observation).expect("build partial report");
        let encoded = serde_json::to_value(&report).expect("encode partial report");

        assert_eq!(
            report.completeness(),
            HotRunCacheObservationCompleteness::Partial
        );
        assert!(report.states().is_empty());
        assert!(encoded["summary"]["in_use_count"].is_null());
        assert!(encoded["summary"]["unknown_count"].is_null());
    }

    #[test]
    fn classifies_permission_errors_without_private_evidence() {
        let error = super::read_error(rustix::io::Errno::ACCESS);
        assert_eq!(
            error.kind(),
            HotRunCacheObservationErrorKind::PermissionDenied
        );
        assert_eq!(
            error.to_string(),
            "hot-run cache observation found protected state"
        );
    }

    #[test]
    fn rejects_names_and_top_level_entries_that_cannot_be_state_ids() {
        let root = TempRoot::new();
        fs::write(root.path().join("not-a-state"), b"x").expect("write invalid entry");
        let error = observe_hot_run_cache(root.path()).expect_err("reject invalid entry");
        assert_eq!(error.kind(), HotRunCacheObservationErrorKind::UnsafeShape);
        assert!(
            !error
                .to_string()
                .contains(root.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn rejects_physical_objects_shared_between_states() {
        let root = TempRoot::new();
        let first = root.state('a');
        let second = root.state('b');
        fs::write(first.join("data"), b"x").expect("write fixture");
        fs::hard_link(first.join("data"), second.join("data")).expect("cross-state hardlink");
        let error = observe_hot_run_cache(root.path()).expect_err("reject cross-state hardlink");
        assert_eq!(
            error.kind(),
            HotRunCacheObservationErrorKind::AmbiguousHardlink
        );
    }

    #[test]
    fn rejects_depth_beyond_the_reviewed_bound() {
        let root = TempRoot::new();
        let mut directory = root.state('a');
        for index in 0..=MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH {
            directory = directory.join(format!("d{index}"));
            fs::create_dir(&directory).expect("create nested directory");
        }
        let error = observe_hot_run_cache(root.path()).expect_err("reject deep tree");
        assert_eq!(error.kind(), HotRunCacheObservationErrorKind::TooLarge);
    }

    #[test]
    fn direct_and_runtime_kernel_flocks_are_definitely_in_use_but_absence_stays_unknown() {
        let root = TempRoot::new();
        let direct = root.state('a');
        let direct_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(direct.join("lock"))
            .expect("open direct lock fixture");
        rustix_fs::flock(&direct_lock, FlockOperation::LockExclusive)
            .expect("hold direct lock fixture");

        let runtime_state = root.state('b');
        let runtime = runtime_state.join(format!("runtime-{}", "c".repeat(64)));
        fs::create_dir(&runtime).expect("create runtime state fixture");
        let runtime_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(runtime.join("lock"))
            .expect("open runtime lock fixture");
        rustix_fs::flock(&runtime_lock, FlockOperation::LockExclusive)
            .expect("hold runtime lock fixture");

        let observation = observe_hot_run_cache(root.path()).expect("observe held locks");
        let report =
            build_hot_run_cache_observation_report(observation).expect("classify held locks");
        assert_eq!(report.summary().state_count(), 2);
        for state in report.states() {
            assert_eq!(state.classification(), CacheStateClassification::InUse);
            assert_eq!(state.reasons()[0], CacheStateReason::ActiveLock);
            assert!(
                state
                    .reasons()
                    .contains(&CacheStateReason::OwnershipUnknown)
            );
        }

        drop(runtime_lock);
        drop(direct_lock);
        let observation = observe_hot_run_cache(root.path()).expect("observe unlocked files");
        let report =
            build_hot_run_cache_observation_report(observation).expect("classify unlocked files");
        for state in report.states() {
            assert_eq!(state.classification(), CacheStateClassification::Unknown);
            assert!(
                state
                    .reasons()
                    .contains(&CacheStateReason::ActiveLockUnknown)
            );
        }
    }

    #[test]
    fn symlinked_and_hardlinked_lock_names_never_establish_activity() {
        let root = TempRoot::new();
        let symlink_state = root.state('a');
        let outside_root = TempRoot::new();
        let outside_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(outside_root.path().join("outside-lock"))
            .expect("open outside lock fixture");
        symlink(
            outside_root.path().join("outside-lock"),
            symlink_state.join("lock"),
        )
        .expect("symlink lock fixture");
        rustix_fs::flock(&outside_lock, FlockOperation::LockExclusive)
            .expect("hold outside lock fixture");

        let hardlink_state = root.state('b');
        let hardlink_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(hardlink_state.join("lock"))
            .expect("open hardlink lock fixture");
        fs::hard_link(
            hardlink_state.join("lock"),
            hardlink_state.join("lock-alias"),
        )
        .expect("hardlink lock alias");
        rustix_fs::flock(&hardlink_lock, FlockOperation::LockExclusive)
            .expect("hold hardlinked lock fixture");

        let observation = observe_hot_run_cache(root.path()).expect("observe ambiguous locks");
        let report =
            build_hot_run_cache_observation_report(observation).expect("classify ambiguous locks");
        for state in report.states() {
            assert_eq!(state.classification(), CacheStateClassification::Unknown);
            assert!(
                state
                    .reasons()
                    .contains(&CacheStateReason::ActiveLockUnknown)
            );
            assert!(!state.reasons().contains(&CacheStateReason::ActiveLock));
        }
    }

    #[test]
    fn lock_candidate_descriptors_have_an_aggregate_bound() {
        let root = TempRoot::new();
        let state = root.state('a');
        for index in 0..=MAX_HOT_RUN_CACHE_LOCK_CANDIDATES {
            let runtime = state.join(format!("runtime-{index:064x}"));
            fs::create_dir(&runtime).expect("create bounded runtime fixture");
            fs::write(runtime.join("lock"), b"").expect("create bounded lock fixture");
        }

        let error = observe_hot_run_cache(root.path()).expect_err("reject excess lock candidates");
        assert_eq!(error.kind(), HotRunCacheObservationErrorKind::TooLarge);
    }

    #[test]
    fn secondary_lock_discovery_has_an_independent_entry_bound() {
        let root = TempRoot::new();
        let state_path = root.state('a');
        fs::write(state_path.join("ordinary-entry"), b"").expect("create discovery fixture");
        let state = super::BoundDirectory::open_root(&state_path).expect("bind state fixture");
        let mut entries_seen = super::MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES;
        let mut candidates_seen = 0;

        let error = super::expected_lock_identities(
            &state,
            state.snapshot.device,
            &mut entries_seen,
            &mut candidates_seen,
        )
        .expect_err("reject excess secondary discovery work");

        assert_eq!(error.kind(), HotRunCacheObservationErrorKind::TooLarge);
        assert_eq!(candidates_seen, 0);
    }
}
