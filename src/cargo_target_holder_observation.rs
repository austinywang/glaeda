//! Positive-only Linux holder observation for one checkout-local Cargo target.
//!
//! The observer binds every result to the same opaque physical target identity used by the Cargo
//! target cost observer. It emits counts only: never process IDs, process names, commands, paths,
//! file names, maps, mount rows, or environments. A zero count means `none_observed`, not absence.
//! Process-table churn, inaccessible process evidence, and the non-atomic nature of `/proc` remain
//! explicit, so this report cannot authorize retention, retirement, cleanup, or deletion.

use std::collections::BTreeSet;
use std::ffi::{CStr, OsStr};
use std::fmt;
use std::fs::{self, File};
use std::io::Read as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::cargo_target_observation::cargo_target_materialization_id;

pub const CARGO_TARGET_HOLDER_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_CARGO_TARGET_HOLDER_PROCESSES: usize = 131_072;
pub const MAX_CARGO_TARGET_HOLDER_FDS: u64 = 2_000_000;
pub const MAX_CARGO_TARGET_HOLDER_PROC_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_CARGO_TARGET_HOLDER_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NOATIME)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetHolderObservationAuthority {
    PositiveObservationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetHolderDisposition {
    HoldersObserved,
    NoneObserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CargoTargetHolderCounts {
    cwd_processes: u64,
    root_processes: u64,
    open_fd_processes: u64,
    open_fd_references: u64,
    mapped_file_processes: u64,
    mount_namespaces: u64,
    mount_references: u64,
    holder_processes: u64,
}

impl CargoTargetHolderCounts {
    #[must_use]
    pub const fn any_observed(self) -> bool {
        self.cwd_processes > 0
            || self.root_processes > 0
            || self.open_fd_processes > 0
            || self.mapped_file_processes > 0
            || self.mount_namespaces > 0
    }

    #[must_use]
    pub const fn cwd_processes(self) -> u64 {
        self.cwd_processes
    }

    #[must_use]
    pub const fn root_processes(self) -> u64 {
        self.root_processes
    }

    #[must_use]
    pub const fn open_fd_processes(self) -> u64 {
        self.open_fd_processes
    }

    #[must_use]
    pub const fn open_fd_references(self) -> u64 {
        self.open_fd_references
    }

    #[must_use]
    pub const fn mapped_file_processes(self) -> u64 {
        self.mapped_file_processes
    }

    #[must_use]
    pub const fn mount_namespaces(self) -> u64 {
        self.mount_namespaces
    }

    #[must_use]
    pub const fn mount_references(self) -> u64 {
        self.mount_references
    }

    #[must_use]
    pub const fn holder_processes(self) -> u64 {
        self.holder_processes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CargoTargetHolderCoverage {
    process_entries_started: u64,
    process_entries_completed: u64,
    process_entries_incomplete: u64,
    fd_entries_examined: u64,
    maps_examined: u64,
    mount_namespaces_examined: u64,
    mount_namespace_observations_incomplete: u64,
    proc_bytes_read: u64,
    observer_process_excluded: bool,
    process_table_rescan_equal: bool,
    atomic_process_snapshot: bool,
    universal_absence_proven: bool,
}

impl CargoTargetHolderCoverage {
    #[must_use]
    pub const fn process_entries_started(self) -> u64 {
        self.process_entries_started
    }

    #[must_use]
    pub const fn process_entries_completed(self) -> u64 {
        self.process_entries_completed
    }

    #[must_use]
    pub const fn process_entries_incomplete(self) -> u64 {
        self.process_entries_incomplete
    }

    #[must_use]
    pub const fn process_table_rescan_equal(self) -> bool {
        self.process_table_rescan_equal
    }

    #[must_use]
    pub const fn universal_absence_proven(self) -> bool {
        self.universal_absence_proven
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CargoTargetHolderState {
    Absent,
    Present {
        target_id: Sha256Digest,
        disposition: CargoTargetHolderDisposition,
        counts: CargoTargetHolderCounts,
        coverage: CargoTargetHolderCoverage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetHolderObservation {
    schema_version: u8,
    authority: CargoTargetHolderObservationAuthority,
    state: CargoTargetHolderState,
}

impl CargoTargetHolderObservation {
    fn absent() -> Self {
        Self {
            schema_version: CARGO_TARGET_HOLDER_OBSERVATION_SCHEMA_VERSION,
            authority: CargoTargetHolderObservationAuthority::PositiveObservationOnly,
            state: CargoTargetHolderState::Absent,
        }
    }

    #[must_use]
    pub const fn authority(&self) -> CargoTargetHolderObservationAuthority {
        self.authority
    }

    #[must_use]
    pub const fn state(&self) -> &CargoTargetHolderState {
        &self.state
    }
}

/// Observe positive Linux process and mount references to one checkout-local `target`.
///
/// # Errors
///
/// Refuses a non-canonical checkout, symlinked/non-directory/cross-filesystem target, target drift,
/// unavailable `/proc`, malformed mount evidence, bound excess, and arithmetic overflow. Errors
/// never contain the supplied path, a process identifier, or a child name.
pub fn observe_cargo_target_holders(
    checkout: &Path,
) -> Result<CargoTargetHolderObservation, CargoTargetHolderObservationError> {
    observe_cargo_target_holders_at(checkout, Path::new("/proc"))
}

fn observe_cargo_target_holders_at(
    checkout_path: &Path,
    proc_root: &Path,
) -> Result<CargoTargetHolderObservation, CargoTargetHolderObservationError> {
    if !checkout_path.is_absolute() {
        return Err(root_unavailable());
    }
    let canonical_checkout = fs::canonicalize(checkout_path).map_err(|_| root_unavailable())?;
    if canonical_checkout != checkout_path {
        return Err(unsafe_shape());
    }
    let checkout = BoundDirectory::open_root(checkout_path)?;
    let checkout_before = checkout.snapshot;
    let target_name = c"target";
    let target_stat =
        match rustix_fs::statat(checkout.fd.as_fd(), target_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => {
                checkout.revalidate_against(checkout_before)?;
                return Ok(CargoTargetHolderObservation::absent());
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
    let target_id = cargo_target_materialization_id(&target_stat).map_err(|_| unsafe_shape())?;
    let target_path = canonical_checkout.join("target");
    let scan = scan_proc(proc_root, &target_path, std::process::id())?;

    target.revalidate()?;
    let rebound = BoundDirectory::open_child(&checkout.fd, target_name)?;
    if rebound.snapshot != target.snapshot {
        return Err(changed());
    }
    checkout.revalidate_against(checkout_before)?;
    let disposition = if scan.counts.any_observed() {
        CargoTargetHolderDisposition::HoldersObserved
    } else {
        CargoTargetHolderDisposition::NoneObserved
    };
    Ok(CargoTargetHolderObservation {
        schema_version: CARGO_TARGET_HOLDER_OBSERVATION_SCHEMA_VERSION,
        authority: CargoTargetHolderObservationAuthority::PositiveObservationOnly,
        state: CargoTargetHolderState::Present {
            target_id,
            disposition,
            counts: scan.counts,
            coverage: scan.coverage,
        },
    })
}

#[derive(Debug)]
struct ProcScan {
    counts: CargoTargetHolderCounts,
    coverage: CargoTargetHolderCoverage,
}

#[derive(Debug, Default)]
struct ProcessReferenceFlags {
    cwd: bool,
    root: bool,
    open_fd: bool,
    mapped_file: bool,
}

impl ProcessReferenceFlags {
    fn any(&self) -> bool {
        self.cwd || self.root || self.open_fd || self.mapped_file
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MountNamespaceIdentity {
    device: u64,
    inode: u64,
}

fn scan_proc(
    proc_root: &Path,
    target: &Path,
    observer_pid: u32,
) -> Result<ProcScan, CargoTargetHolderObservationError> {
    let started = process_entries(proc_root)?;
    let mut counts = CargoTargetHolderCounts {
        cwd_processes: 0,
        root_processes: 0,
        open_fd_processes: 0,
        open_fd_references: 0,
        mapped_file_processes: 0,
        mount_namespaces: 0,
        mount_references: 0,
        holder_processes: 0,
    };
    let mut completed = 0_u64;
    let mut incomplete = 0_u64;
    let mut fd_entries_examined = 0_u64;
    let mut maps_examined = 0_u64;
    let mut mount_namespaces_examined = 0_u64;
    let mut mount_namespace_observations_incomplete = 0_u64;
    let mut proc_bytes_read = 0_u64;
    let mut mount_namespaces = BTreeSet::new();

    for pid in &started {
        if *pid == observer_pid {
            continue;
        }
        let process_root = proc_root.join(pid.to_string());
        let mut process_incomplete = false;
        let mut flags = ProcessReferenceFlags {
            cwd: read_link_reference(&process_root.join("cwd"), target, &mut process_incomplete),
            root: read_link_reference(&process_root.join("root"), target, &mut process_incomplete),
            ..ProcessReferenceFlags::default()
        };

        match fs::read_dir(process_root.join("fd")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => {
                            process_incomplete = true;
                            continue;
                        }
                    };
                    let name = entry.file_name();
                    if parse_decimal(name.as_bytes()).is_none() {
                        process_incomplete = true;
                        continue;
                    }
                    fd_entries_examined = checked_add(fd_entries_examined, 1)?;
                    if fd_entries_examined > MAX_CARGO_TARGET_HOLDER_FDS {
                        return Err(too_large());
                    }
                    match fs::read_link(entry.path()) {
                        Ok(link) if path_references_target(&link, target) => {
                            flags.open_fd = true;
                            counts.open_fd_references = checked_add(counts.open_fd_references, 1)?;
                        }
                        Ok(_) => {}
                        Err(_) => process_incomplete = true,
                    }
                }
            }
            Err(_) => process_incomplete = true,
        }

        match read_bounded(
            &process_root.join("maps"),
            &mut proc_bytes_read,
            MAX_CARGO_TARGET_HOLDER_PROC_FILE_BYTES,
        ) {
            Ok(maps) => {
                maps_examined = checked_add(maps_examined, 1)?;
                flags.mapped_file = maps_reference_target(&maps, target)?;
            }
            Err(error) if error.kind() == CargoTargetHolderObservationErrorKind::TooLarge => {
                return Err(error);
            }
            Err(_) => process_incomplete = true,
        }

        match fs::metadata(process_root.join("ns/mnt")) {
            Ok(metadata) => {
                let identity = MountNamespaceIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                };
                if mount_namespaces.insert(identity) {
                    match read_bounded(
                        &process_root.join("mountinfo"),
                        &mut proc_bytes_read,
                        MAX_CARGO_TARGET_HOLDER_PROC_FILE_BYTES,
                    ) {
                        Ok(mountinfo) => {
                            mount_namespaces_examined = checked_add(mount_namespaces_examined, 1)?;
                            let references = mountinfo_references_target(&mountinfo, target)?;
                            if references > 0 {
                                counts.mount_namespaces = checked_add(counts.mount_namespaces, 1)?;
                                counts.mount_references =
                                    checked_add(counts.mount_references, references)?;
                            }
                        }
                        Err(error)
                            if error.kind() == CargoTargetHolderObservationErrorKind::TooLarge =>
                        {
                            return Err(error);
                        }
                        Err(_) => {
                            mount_namespace_observations_incomplete =
                                checked_add(mount_namespace_observations_incomplete, 1)?;
                            process_incomplete = true;
                        }
                    }
                }
            }
            Err(_) => {
                mount_namespace_observations_incomplete =
                    checked_add(mount_namespace_observations_incomplete, 1)?;
                process_incomplete = true;
            }
        }

        if flags.cwd {
            counts.cwd_processes = checked_add(counts.cwd_processes, 1)?;
        }
        if flags.root {
            counts.root_processes = checked_add(counts.root_processes, 1)?;
        }
        if flags.open_fd {
            counts.open_fd_processes = checked_add(counts.open_fd_processes, 1)?;
        }
        if flags.mapped_file {
            counts.mapped_file_processes = checked_add(counts.mapped_file_processes, 1)?;
        }
        if flags.any() {
            counts.holder_processes = checked_add(counts.holder_processes, 1)?;
        }
        if process_incomplete {
            incomplete = checked_add(incomplete, 1)?;
        } else {
            completed = checked_add(completed, 1)?;
        }
    }

    let finished = process_entries(proc_root)?;
    Ok(ProcScan {
        counts,
        coverage: CargoTargetHolderCoverage {
            process_entries_started: u64::try_from(started.len()).map_err(|_| too_large())?,
            process_entries_completed: completed,
            process_entries_incomplete: incomplete,
            fd_entries_examined,
            maps_examined,
            mount_namespaces_examined,
            mount_namespace_observations_incomplete,
            proc_bytes_read,
            observer_process_excluded: started.contains(&observer_pid),
            process_table_rescan_equal: started == finished,
            atomic_process_snapshot: false,
            universal_absence_proven: false,
        },
    })
}

fn process_entries(proc_root: &Path) -> Result<Vec<u32>, CargoTargetHolderObservationError> {
    let entries = fs::read_dir(proc_root).map_err(|_| proc_unavailable())?;
    let mut processes = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| proc_unavailable())?;
        let name = entry.file_name();
        let Some(pid) = parse_decimal(name.as_bytes()) else {
            continue;
        };
        let pid = u32::try_from(pid).map_err(|_| unsafe_shape())?;
        if pid == 0 {
            return Err(unsafe_shape());
        }
        if processes.len() >= MAX_CARGO_TARGET_HOLDER_PROCESSES {
            return Err(too_large());
        }
        processes.push(pid);
    }
    processes.sort_unstable();
    if processes.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(unsafe_shape());
    }
    Ok(processes)
}

fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.iter().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
    })
}

fn read_link_reference(path: &Path, target: &Path, incomplete: &mut bool) -> bool {
    match fs::read_link(path) {
        Ok(link) => path_references_target(&link, target),
        Err(_) => {
            *incomplete = true;
            false
        }
    }
}

fn path_references_target(path: &Path, target: &Path) -> bool {
    path.is_absolute() && path.starts_with(target)
}

fn read_bounded(
    path: &Path,
    total: &mut u64,
    limit: u64,
) -> Result<Vec<u8>, CargoTargetHolderObservationError> {
    let file = File::open(path).map_err(|_| unreadable())?;
    let mut bytes = Vec::new();
    file.take(limit.checked_add(1).ok_or_else(too_large)?)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if u64::try_from(bytes.len()).map_err(|_| too_large())? > limit {
        return Err(too_large());
    }
    *total = checked_add(*total, u64::try_from(bytes.len()).map_err(|_| too_large())?)?;
    if *total > MAX_CARGO_TARGET_HOLDER_TOTAL_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

fn maps_reference_target(
    maps: &[u8],
    target: &Path,
) -> Result<bool, CargoTargetHolderObservationError> {
    for line in maps.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let mut offset = 0;
        for _ in 0..5 {
            while offset < line.len() && line[offset].is_ascii_whitespace() {
                offset += 1;
            }
            let start = offset;
            while offset < line.len() && !line[offset].is_ascii_whitespace() {
                offset += 1;
            }
            if start == offset {
                return Err(unsafe_shape());
            }
        }
        while offset < line.len() && line[offset].is_ascii_whitespace() {
            offset += 1;
        }
        if offset == line.len() {
            continue;
        }
        let path = Path::new(OsStr::from_bytes(&line[offset..]));
        if path_references_target(path, target) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn mountinfo_references_target(
    mountinfo: &[u8],
    target: &Path,
) -> Result<u64, CargoTargetHolderObservationError> {
    let mut references = 0_u64;
    for line in mountinfo.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(unsafe_shape)?;
        if separator < 6 || fields.len() < separator + 4 {
            return Err(unsafe_shape());
        }
        let root = decode_mountinfo_path(fields[3])?;
        let mountpoint = decode_mountinfo_path(fields[4])?;
        if path_references_target(&root, target) || path_references_target(&mountpoint, target) {
            references = checked_add(references, 1)?;
        }
    }
    Ok(references)
}

fn decode_mountinfo_path(value: &[u8]) -> Result<PathBuf, CargoTargetHolderObservationError> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut offset = 0;
    while offset < value.len() {
        if value[offset] != b'\\' {
            decoded.push(value[offset]);
            offset += 1;
            continue;
        }
        if offset + 4 > value.len() {
            return Err(unsafe_shape());
        }
        let escape = &value[offset + 1..offset + 4];
        decoded.push(match escape {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return Err(unsafe_shape()),
        });
        offset += 4;
    }
    Ok(PathBuf::from(OsStr::from_bytes(&decoded)))
}

fn checked_add(left: u64, right: u64) -> Result<u64, CargoTargetHolderObservationError> {
    left.checked_add(right).ok_or_else(too_large)
}

#[derive(Debug)]
struct BoundDirectory {
    fd: OwnedFd,
    snapshot: DirectorySnapshot,
}

impl BoundDirectory {
    fn open_root(path: &Path) -> Result<Self, CargoTargetHolderObservationError> {
        let fd = rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| root_unavailable())?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn open_child(
        parent: &OwnedFd,
        name: &CStr,
    ) -> Result<Self, CargoTargetHolderObservationError> {
        let fd = rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| unreadable())?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate(&self) -> Result<(), CargoTargetHolderObservationError> {
        self.revalidate_against(self.snapshot)
    }

    fn revalidate_against(
        &self,
        expected: DirectorySnapshot,
    ) -> Result<(), CargoTargetHolderObservationError> {
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
    ctime_seconds: i64,
    ctime_nanoseconds: i64,
}

impl DirectorySnapshot {
    fn from_fd(fd: &OwnedFd) -> Result<Self, CargoTargetHolderObservationError> {
        Self::from_stat(&rustix_fs::fstat(fd).map_err(|_| unreadable())?)
    }

    fn from_stat(stat: &Stat) -> Result<Self, CargoTargetHolderObservationError> {
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(unsafe_shape());
        }
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
            ctime_seconds: stat.st_ctime,
            ctime_nanoseconds: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_shape())?,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetHolderObservationErrorKind {
    RootUnavailable,
    ProcUnavailable,
    Unreadable,
    UnsafeShape,
    Changed,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetHolderObservationError {
    kind: CargoTargetHolderObservationErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl CargoTargetHolderObservationError {
    #[must_use]
    pub const fn kind(&self) -> CargoTargetHolderObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for CargoTargetHolderObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for CargoTargetHolderObservationError {}

const fn error(
    kind: CargoTargetHolderObservationErrorKind,
    code: &'static str,
    problem: &'static str,
) -> CargoTargetHolderObservationError {
    CargoTargetHolderObservationError {
        kind,
        code,
        problem,
    }
}

const fn root_unavailable() -> CargoTargetHolderObservationError {
    error(
        CargoTargetHolderObservationErrorKind::RootUnavailable,
        "cargo_target_holder_root_unavailable",
        "checkout root is unavailable for Cargo target holder observation",
    )
}

const fn proc_unavailable() -> CargoTargetHolderObservationError {
    error(
        CargoTargetHolderObservationErrorKind::ProcUnavailable,
        "cargo_target_holder_proc_unavailable",
        "Linux process evidence is unavailable",
    )
}

const fn unreadable() -> CargoTargetHolderObservationError {
    error(
        CargoTargetHolderObservationErrorKind::Unreadable,
        "cargo_target_holder_unreadable",
        "Cargo target holder evidence is unreadable",
    )
}

const fn unsafe_shape() -> CargoTargetHolderObservationError {
    error(
        CargoTargetHolderObservationErrorKind::UnsafeShape,
        "cargo_target_holder_unsafe_shape",
        "Cargo target holder evidence has an unsupported shape",
    )
}

const fn changed() -> CargoTargetHolderObservationError {
    error(
        CargoTargetHolderObservationErrorKind::Changed,
        "cargo_target_holder_changed",
        "Cargo target changed during holder observation",
    )
}

const fn too_large() -> CargoTargetHolderObservationError {
    error(
        CargoTargetHolderObservationErrorKind::TooLarge,
        "cargo_target_holder_too_large",
        "Cargo target holder evidence exceeds the reviewed bound",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        CargoTargetHolderDisposition, CargoTargetHolderObservationErrorKind,
        CargoTargetHolderState, observe_cargo_target_holders_at, read_bounded,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        checkout: PathBuf,
        proc_root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "glaeda-cargo-target-holder-observation-{}-{sequence}",
                std::process::id()
            ));
            let checkout = root.join("checkout");
            let proc_root = root.join("proc");
            fs::create_dir_all(&checkout).expect("create checkout");
            fs::create_dir(&proc_root).expect("create proc root");
            Self {
                root,
                checkout,
                proc_root,
            }
        }

        fn target(&self) -> PathBuf {
            let target = self.checkout.join("target");
            fs::create_dir_all(target.join("debug")).expect("create target");
            fs::write(target.join("debug/artifact"), b"artifact").expect("write artifact");
            target
        }

        fn process(&self, pid: u32, target: &Path) {
            let process = self.proc_root.join(pid.to_string());
            fs::create_dir_all(process.join("fd")).expect("create fd directory");
            fs::create_dir_all(process.join("ns")).expect("create namespace directory");
            fs::write(process.join("ns/mnt"), b"namespace").expect("write namespace identity");
            symlink(target.join("debug"), process.join("cwd")).expect("link cwd");
            symlink("/", process.join("root")).expect("link root");
            symlink(target.join("debug/artifact"), process.join("fd/7")).expect("link fd");
            fs::write(
                process.join("maps"),
                format!(
                    "00400000-00401000 r--p 00000000 00:00 0 {}\n",
                    target.join("debug/artifact").display()
                ),
            )
            .expect("write maps");
            fs::write(
                process.join("mountinfo"),
                format!(
                    "36 25 8:1 / {} rw - ext4 /dev/root rw\n",
                    target.join("mounted").display()
                ),
            )
            .expect("write mountinfo");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn absent_target_skips_process_observation() {
        let fixture = Fixture::new();
        let observation = observe_cargo_target_holders_at(&fixture.checkout, &fixture.proc_root)
            .expect("observe absent target");
        assert!(matches!(
            observation.state(),
            CargoTargetHolderState::Absent
        ));
    }

    #[test]
    fn positive_references_are_counted_without_identifiers_or_paths() {
        let fixture = Fixture::new();
        let target = fixture.target();
        fixture.process(123, &target);

        let observation = observe_cargo_target_holders_at(&fixture.checkout, &fixture.proc_root)
            .expect("observe holders");
        let CargoTargetHolderState::Present {
            disposition,
            counts,
            coverage,
            ..
        } = observation.state()
        else {
            panic!("target must be present");
        };
        assert_eq!(*disposition, CargoTargetHolderDisposition::HoldersObserved);
        assert_eq!(counts.cwd_processes(), 1);
        assert_eq!(counts.open_fd_processes(), 1);
        assert_eq!(counts.open_fd_references(), 1);
        assert_eq!(counts.mapped_file_processes(), 1);
        assert_eq!(counts.mount_namespaces(), 1);
        assert_eq!(counts.mount_references(), 1);
        assert_eq!(counts.holder_processes(), 1);
        assert_eq!(coverage.process_entries_completed(), 1);
        assert_eq!(coverage.process_entries_incomplete(), 0);
        assert!(!coverage.universal_absence_proven());

        let encoded = serde_json::to_string(&observation).expect("serialize observation");
        assert!(!encoded.contains("\"pid\""));
        assert!(!encoded.contains(fixture.root.to_string_lossy().as_ref()));
        assert!(!encoded.contains("artifact"));
    }

    #[test]
    fn zero_references_remain_none_observed_not_absence() {
        let fixture = Fixture::new();
        fixture.target();
        let process = fixture.proc_root.join("456");
        fs::create_dir_all(process.join("fd")).expect("create empty fd directory");
        fs::create_dir_all(process.join("ns")).expect("create namespace directory");
        fs::write(process.join("ns/mnt"), b"namespace").expect("write namespace identity");
        symlink("/", process.join("cwd")).expect("link cwd");
        symlink("/", process.join("root")).expect("link root");
        fs::write(process.join("maps"), b"00400000-00401000 r--p 0 00:00 0\n").expect("write maps");
        fs::write(
            process.join("mountinfo"),
            b"36 25 8:1 / / rw - ext4 /dev/root rw\n",
        )
        .expect("write mountinfo");

        let observation = observe_cargo_target_holders_at(&fixture.checkout, &fixture.proc_root)
            .expect("observe no references");
        let CargoTargetHolderState::Present {
            disposition,
            counts,
            coverage,
            ..
        } = observation.state()
        else {
            panic!("target must be present");
        };
        assert_eq!(*disposition, CargoTargetHolderDisposition::NoneObserved);
        assert!(!counts.any_observed());
        assert!(!coverage.universal_absence_proven());
        assert!(
            !serde_json::to_string(&observation)
                .unwrap()
                .contains("absent")
        );
    }

    #[test]
    fn missing_process_evidence_is_reported_as_incomplete() {
        let fixture = Fixture::new();
        fixture.target();
        fs::create_dir(fixture.proc_root.join("789")).expect("create incomplete process");

        let observation = observe_cargo_target_holders_at(&fixture.checkout, &fixture.proc_root)
            .expect("observe incomplete evidence");
        let CargoTargetHolderState::Present { coverage, .. } = observation.state() else {
            panic!("target must be present");
        };
        assert_eq!(coverage.process_entries_started(), 1);
        assert_eq!(coverage.process_entries_completed(), 0);
        assert_eq!(coverage.process_entries_incomplete(), 1);
        assert!(!coverage.universal_absence_proven());
    }

    #[test]
    fn bounded_proc_reader_refuses_before_accepting_excess() {
        let fixture = Fixture::new();
        let path = fixture.root.join("oversized-proc-file");
        let mut file = fs::File::create(&path).expect("create bounded fixture");
        file.write_all(b"abcd").expect("write bounded fixture");
        let mut total = 0;
        let error = read_bounded(&path, &mut total, 3).expect_err("reject excess byte");
        assert_eq!(
            error.kind(),
            CargoTargetHolderObservationErrorKind::TooLarge
        );
    }
}
