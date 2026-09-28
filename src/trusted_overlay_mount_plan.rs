use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read as _, Take};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::geteuid;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::descriptor_bound_launcher::ReviewedFilesystemIdentity;
use crate::trusted_overlay_task_view::{
    OverlaySourceAnchorBinding, OverlaySourceAnchorRecord, OverlayTaskViewLease,
    OverlayTaskViewRecord, OverlayTaskViewState,
};

pub const TRUSTED_OVERLAY_MOUNT_PLAN_SCHEMA_VERSION: u8 = 1;
const MAX_PROC_FILESYSTEMS_BYTES: usize = 65_536;
const MAX_PROC_MOUNTINFO_BYTES: usize = 1_048_576;
const OVERLAY_FILESYSTEM_NAME: &[u8] = b"overlay";
const PROC_SUPER_MAGIC: u64 = 0x9fa0;
const PROC_DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedOverlayMountOptionPolicy {
    SingleLowerNodevNosuidV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedOverlayMountPlanSummary {
    schema_version: u8,
    option_policy: TrustedOverlayMountOptionPolicy,
    role_count: u8,
    single_filesystem_device: bool,
}

impl TrustedOverlayMountPlanSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn option_policy(&self) -> TrustedOverlayMountOptionPolicy {
        self.option_policy
    }

    #[must_use]
    pub const fn role_count(&self) -> u8 {
        self.role_count
    }

    #[must_use]
    pub const fn single_filesystem_device(&self) -> bool {
        self.single_filesystem_device
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TrustedOverlayMountPaths {
    lower: PathBuf,
    upper: PathBuf,
    work: PathBuf,
    merged: PathBuf,
}

impl TrustedOverlayMountPaths {
    /// Bind the four private path roles used by one future OverlayFS mount.
    ///
    /// This constructor performs no filesystem I/O.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless every path is normalized, absolute, UTF-8, non-root, and
    /// lexically disjoint from every other role; no role may contain another.
    pub fn new(
        lower: impl Into<PathBuf>,
        upper: impl Into<PathBuf>,
        work: impl Into<PathBuf>,
        merged: impl Into<PathBuf>,
    ) -> Result<Self, TrustedOverlayMountPlanError> {
        let lower = validate_absolute_path(lower.into())?;
        let upper = validate_absolute_path(upper.into())?;
        let work = validate_absolute_path(work.into())?;
        let merged = validate_absolute_path(merged.into())?;
        let paths = [&lower, &upper, &work, &merged];
        for (index, left) in paths.iter().enumerate() {
            for right in paths.iter().skip(index + 1) {
                if *left == *right
                    || left.starts_with(right.as_path())
                    || right.starts_with(left.as_path())
                {
                    return Err(role_conflict());
                }
            }
        }
        Ok(Self {
            lower,
            upper,
            work,
            merged,
        })
    }
}

impl fmt::Debug for TrustedOverlayMountPaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private trusted overlay mount paths>")
    }
}

#[derive(Clone, PartialEq, Eq)]
struct TrustedOverlayDirectory {
    path: PathBuf,
    identity: ReviewedFilesystemIdentity,
    snapshot: DirectorySnapshot,
}

impl fmt::Debug for TrustedOverlayDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private exact overlay directory>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TrustedOverlayKernelCapability {
    mount_namespace_device: u64,
    mount_namespace_inode: u64,
    filesystems_digest: Sha256Digest,
}

impl TrustedOverlayKernelCapability {
    #[must_use]
    pub const fn mount_namespace_device(&self) -> u64 {
        self.mount_namespace_device
    }

    #[must_use]
    pub const fn mount_namespace_inode(&self) -> u64 {
        self.mount_namespace_inode
    }

    #[must_use]
    pub const fn filesystems_digest(&self) -> &Sha256Digest {
        &self.filesystems_digest
    }
}

impl fmt::Debug for TrustedOverlayKernelCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedOverlayKernelCapability")
            .field("mount_namespace", &"<private exact mount namespace>")
            .field("filesystems_digest", &self.filesystems_digest)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TrustedOverlayMountPlan {
    summary: TrustedOverlayMountPlanSummary,
    source_anchor: OverlaySourceAnchorBinding,
    task_lease: OverlayTaskViewLease,
    kernel: TrustedOverlayKernelCapability,
    lower: TrustedOverlayDirectory,
    upper: TrustedOverlayDirectory,
    work: TrustedOverlayDirectory,
    merged: TrustedOverlayDirectory,
}

impl TrustedOverlayMountPlan {
    #[must_use]
    pub const fn summary(&self) -> &TrustedOverlayMountPlanSummary {
        &self.summary
    }

    #[must_use]
    pub const fn source_anchor(&self) -> &OverlaySourceAnchorBinding {
        &self.source_anchor
    }

    #[must_use]
    pub const fn task_lease(&self) -> &OverlayTaskViewLease {
        &self.task_lease
    }

    #[must_use]
    pub const fn kernel(&self) -> &TrustedOverlayKernelCapability {
        &self.kernel
    }

    /// Reopen and retain every private directory role through no-follow descriptor traversal.
    ///
    /// The plan is confirmed before acquisition and again after all four descriptors are held, so
    /// concurrent path replacement prevents publication while already-opened descriptors remain
    /// private diagnostic objects. This function performs no mount or filesystem mutation.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error when current plan authority/evidence fails, a path
    /// contains an alias/non-directory component, or a held descriptor differs from its sealed role.
    pub fn open_descriptor_lease(
        &self,
        source_anchor: &OverlaySourceAnchorRecord,
        task_view: &OverlayTaskViewRecord,
    ) -> Result<TrustedOverlayMountDescriptorLease, TrustedOverlayMountPlanError> {
        self.confirm(source_anchor, task_view)?;
        let lower = open_held_directory(&self.lower)?;
        let upper = open_held_directory(&self.upper)?;
        let work = open_held_directory(&self.work)?;
        let merged = open_held_directory(&self.merged)?;
        let (merged_parent, merged_name) = open_held_parent(&self.merged)?;
        let lease = TrustedOverlayMountDescriptorLease {
            summary: TrustedOverlayMountDescriptorLeaseSummary {
                schema_version: TRUSTED_OVERLAY_MOUNT_PLAN_SCHEMA_VERSION,
                role_count: 4,
                single_filesystem_device: true,
            },
            source_anchor: self.source_anchor.clone(),
            task_lease: self.task_lease.clone(),
            lower,
            upper,
            work,
            merged,
            merged_parent,
            merged_name,
        };
        lease.confirm(self, source_anchor, task_view)?;
        Ok(lease)
    }

    /// Reconfirm this sealed plan against current authority and read-only host evidence.
    ///
    /// The source-anchor record revision may advance while sibling leases change. Exact child
    /// lease membership and task lifecycle state are rechecked instead of freezing an old anchor
    /// revision. This function performs no filesystem mutation.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error when current task/anchor authority, directory identity,
    /// workdir emptiness, mount namespace, OverlayFS capability, or merged-target absence no longer
    /// matches the sealed plan.
    pub fn confirm(
        &self,
        source_anchor: &OverlaySourceAnchorRecord,
        task_view: &OverlayTaskViewRecord,
    ) -> Result<(), TrustedOverlayMountPlanError> {
        require_mount_authority(source_anchor, task_view)?;
        if source_anchor.binding() != &self.source_anchor || task_view.lease() != &self.task_lease {
            return Err(plan_authority_mismatch());
        }
        require_procfs()?;
        let filesystems = read_bounded(Path::new("/proc/filesystems"), MAX_PROC_FILESYSTEMS_BYTES)?;
        if sha256_digest(&filesystems)? != self.kernel.filesystems_digest {
            return Err(changed_during_observation());
        }
        let mount_namespace = fs::metadata("/proc/self/ns/mnt").map_err(|_| io_error())?;
        if (mount_namespace.dev(), mount_namespace.ino())
            != (
                self.kernel.mount_namespace_device,
                self.kernel.mount_namespace_inode,
            )
        {
            return Err(changed_during_observation());
        }
        require_empty_directory(&self.work.path)?;
        for directory in [&self.lower, &self.upper, &self.work, &self.merged] {
            revalidate_directory(directory)?;
        }
        validate_directory_roles([&self.lower, &self.upper, &self.work, &self.merged])?;
        let mountinfo = read_bounded(Path::new("/proc/self/mountinfo"), MAX_PROC_MOUNTINFO_BYTES)?;
        if mountinfo_has_mountpoint(&mountinfo, &self.merged.path)? {
            return Err(already_mounted());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedOverlayMountDescriptorLeaseSummary {
    schema_version: u8,
    role_count: u8,
    single_filesystem_device: bool,
}

impl TrustedOverlayMountDescriptorLeaseSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn role_count(self) -> u8 {
        self.role_count
    }

    #[must_use]
    pub const fn single_filesystem_device(self) -> bool {
        self.single_filesystem_device
    }
}

pub struct TrustedOverlayMountDescriptorLease {
    summary: TrustedOverlayMountDescriptorLeaseSummary,
    source_anchor: OverlaySourceAnchorBinding,
    task_lease: OverlayTaskViewLease,
    lower: OwnedFd,
    upper: OwnedFd,
    work: OwnedFd,
    merged: OwnedFd,
    merged_parent: OwnedFd,
    merged_name: OsString,
}

impl TrustedOverlayMountDescriptorLease {
    #[must_use]
    pub const fn summary(&self) -> TrustedOverlayMountDescriptorLeaseSummary {
        self.summary
    }

    pub(crate) fn execution_descriptors(&self) -> TrustedOverlayMountExecutionDescriptors<'_> {
        TrustedOverlayMountExecutionDescriptors {
            lower: self.lower.as_fd(),
            upper: self.upper.as_fd(),
            work: self.work.as_fd(),
            merged: self.merged.as_fd(),
            merged_parent: self.merged_parent.as_fd(),
            merged_name: &self.merged_name,
        }
    }

    /// Reconfirm the held role descriptors against this exact sealed plan and current task authority.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error when the supplied plan is for another logical task,
    /// current plan evidence fails, or any held descriptor no longer matches its sealed role.
    pub fn confirm(
        &self,
        plan: &TrustedOverlayMountPlan,
        source_anchor: &OverlaySourceAnchorRecord,
        task_view: &OverlayTaskViewRecord,
    ) -> Result<(), TrustedOverlayMountPlanError> {
        if plan.source_anchor != self.source_anchor || plan.task_lease != self.task_lease {
            return Err(plan_authority_mismatch());
        }
        plan.confirm(source_anchor, task_view)?;
        require_held_directory(&self.lower, &plan.lower)?;
        require_held_directory(&self.upper, &plan.upper)?;
        require_held_directory(&self.work, &plan.work)?;
        require_held_directory(&self.merged, &plan.merged)?;
        require_merged_parent_binding(&self.merged_parent, &self.merged_name, &plan.merged)?;
        validate_held_roles([&self.lower, &self.upper, &self.work, &self.merged])?;
        Ok(())
    }
}

pub(crate) struct TrustedOverlayMountExecutionDescriptors<'a> {
    pub(crate) lower: BorrowedFd<'a>,
    pub(crate) upper: BorrowedFd<'a>,
    pub(crate) work: BorrowedFd<'a>,
    pub(crate) merged: BorrowedFd<'a>,
    pub(crate) merged_parent: BorrowedFd<'a>,
    pub(crate) merged_name: &'a OsStr,
}

impl fmt::Debug for TrustedOverlayMountDescriptorLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = (
            &self.lower,
            &self.upper,
            &self.work,
            &self.merged,
            &self.merged_parent,
            &self.merged_name,
        );
        formatter
            .debug_struct("TrustedOverlayMountDescriptorLease")
            .field("summary", &self.summary)
            .field("source_anchor", &self.source_anchor)
            .field("task_lease", &self.task_lease)
            .field("descriptors", &"<private exact overlay role descriptors>")
            .finish()
    }
}

impl fmt::Debug for TrustedOverlayMountPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let private_roles = [&self.lower, &self.upper, &self.work, &self.merged];
        formatter
            .debug_struct("TrustedOverlayMountPlan")
            .field("summary", &self.summary)
            .field("source_anchor", &self.source_anchor)
            .field("task_lease", &self.task_lease)
            .field("kernel", &self.kernel)
            .field("directory_role_count", &private_roles.len())
            .field("directories", &"<private exact overlay directories>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedOverlayMountPlanErrorKind {
    InvalidPath,
    UnsafeFilesystem,
    IdentityConflict,
    FilesystemMismatch,
    WorkdirNotEmpty,
    OverlayUnavailable,
    AlreadyMounted,
    ChangedDuringObservation,
    InvalidProcEvidence,
    AuthorityMismatch,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct TrustedOverlayMountPlanError {
    kind: TrustedOverlayMountPlanErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedOverlayMountPlanError {
    #[must_use]
    pub const fn kind(&self) -> TrustedOverlayMountPlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedOverlayMountPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedOverlayMountPlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedOverlayMountPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedOverlayMountPlanError {}

/// Observe and seal one task-specific OverlayFS mount plan without mutating host state.
///
/// The observer requires exact real-directory identities for the immutable lower source, upper,
/// work, and merged target. Every role must be owned by root or the observing identity and carry no
/// group or world write bit; `upperdir` and `workdir` must additionally grant full owner access. All
/// four roles must live on one exact filesystem device in v1, work must be empty, the merged target
/// must be absent from the current mount table, and the current kernel filesystem list must expose
/// OverlayFS. Every directory and capability input is revalidated before the plan is returned.
///
/// # Errors
///
/// Returns a bounded path-private error for aliasing, an untrusted-writable or foreign-owned role,
/// identity drift, filesystem mismatch, a dirty workdir, unavailable OverlayFS, an already-mounted
/// target, malformed proc evidence, or I/O failure.
pub fn observe_trusted_overlay_mount_plan(
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    paths: TrustedOverlayMountPaths,
) -> Result<TrustedOverlayMountPlan, TrustedOverlayMountPlanError> {
    require_mount_authority(source_anchor, task_view)?;
    require_procfs()?;
    let filesystems = read_bounded(Path::new("/proc/filesystems"), MAX_PROC_FILESYSTEMS_BYTES)?;
    let mountinfo = read_bounded(Path::new("/proc/self/mountinfo"), MAX_PROC_MOUNTINFO_BYTES)?;
    let mount_namespace = fs::metadata("/proc/self/ns/mnt").map_err(|_| io_error())?;
    let mount_namespace_identity = (mount_namespace.dev(), mount_namespace.ino());
    let merged_path = paths.merged.clone();
    let plan = observe_with_evidence(
        source_anchor.binding().clone(),
        task_view.lease().clone(),
        paths,
        &filesystems,
        &mountinfo,
        mount_namespace_identity,
        geteuid().as_raw(),
        || {},
    )?;

    require_procfs()?;
    let filesystems_after =
        read_bounded(Path::new("/proc/filesystems"), MAX_PROC_FILESYSTEMS_BYTES)?;
    let mountinfo_after =
        read_bounded(Path::new("/proc/self/mountinfo"), MAX_PROC_MOUNTINFO_BYTES)?;
    let mount_namespace_after = fs::metadata("/proc/self/ns/mnt").map_err(|_| io_error())?;
    if filesystems_after != filesystems
        || mountinfo_has_mountpoint(&mountinfo_after, &merged_path)?
        || (mount_namespace_after.dev(), mount_namespace_after.ino()) != mount_namespace_identity
    {
        return Err(changed_during_observation());
    }
    Ok(plan)
}

fn require_mount_authority(
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
) -> Result<(), TrustedOverlayMountPlanError> {
    if task_view.state() != OverlayTaskViewState::WorktreeRegistered {
        return Err(task_state_invalid());
    }
    if task_view.source_anchor() != source_anchor.binding()
        || !source_anchor.active_tasks().contains(task_view.lease())
    {
        return Err(anchor_task_unproven());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_with_evidence<F>(
    source_anchor: OverlaySourceAnchorBinding,
    task_lease: OverlayTaskViewLease,
    paths: TrustedOverlayMountPaths,
    filesystems: &[u8],
    mountinfo: &[u8],
    mount_namespace: (u64, u64),
    observer_euid: u32,
    before_revalidation: F,
) -> Result<TrustedOverlayMountPlan, TrustedOverlayMountPlanError>
where
    F: FnOnce(),
{
    if !filesystems_expose_overlay(filesystems)? {
        return Err(overlay_unavailable());
    }
    if mountinfo_has_mountpoint(mountinfo, &paths.merged)? {
        return Err(already_mounted());
    }
    if mount_namespace.1 == 0 {
        return Err(invalid_proc_evidence());
    }

    let lower = observe_directory(
        paths.lower.clone(),
        TrustedOverlayRole::Lower,
        observer_euid,
    )?;
    let upper = observe_directory(
        paths.upper.clone(),
        TrustedOverlayRole::Upper,
        observer_euid,
    )?;
    let work = observe_directory(paths.work.clone(), TrustedOverlayRole::Work, observer_euid)?;
    let merged = observe_directory(
        paths.merged.clone(),
        TrustedOverlayRole::Merged,
        observer_euid,
    )?;
    validate_directory_roles([&lower, &upper, &work, &merged])?;
    require_empty_directory(&work.path)?;

    let capability = TrustedOverlayKernelCapability {
        mount_namespace_device: mount_namespace.0,
        mount_namespace_inode: mount_namespace.1,
        filesystems_digest: sha256_digest(filesystems)?,
    };

    before_revalidation();

    for directory in [&lower, &upper, &work, &merged] {
        revalidate_directory(directory)?;
    }
    require_empty_directory(&work.path)?;
    if !filesystems_expose_overlay(filesystems)? {
        return Err(changed_during_observation());
    }
    if mountinfo_has_mountpoint(mountinfo, &merged.path)? {
        return Err(changed_during_observation());
    }

    Ok(TrustedOverlayMountPlan {
        summary: TrustedOverlayMountPlanSummary {
            schema_version: TRUSTED_OVERLAY_MOUNT_PLAN_SCHEMA_VERSION,
            option_policy: TrustedOverlayMountOptionPolicy::SingleLowerNodevNosuidV1,
            role_count: 4,
            single_filesystem_device: true,
        },
        source_anchor,
        task_lease,
        kernel: capability,
        lower,
        upper,
        work,
        merged,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DirectorySnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

/// The trust question one OverlayFS role answers.
///
/// `Lower` is the read-only base every task read and execution resolves against, `Upper` receives
/// the task's writes and shadows `Lower` entry by entry, `Work` is OverlayFS's private staging area
/// on the upper filesystem, and `Merged` is the visible mount target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustedOverlayRole {
    Lower,
    Upper,
    Work,
    Merged,
}

impl TrustedOverlayRole {
    /// Roles OverlayFS must itself read, write, and traverse as the mounting identity.
    const fn requires_owner_access(self) -> bool {
        matches!(self, Self::Upper | Self::Work)
    }
}

/// Refuse any overlay role directory an untrusted identity owns or can write.
///
/// Every role must be owned by root or the observing identity and carry no group or world write
/// bit. Group ownership needs no
/// separate rule here, because a foreign group only gains authority through the group write bit
/// this already refuses.
///
/// `Upper` and `Work` are runtime-private scratch roles the mount must write through, so they
/// additionally require full owner access. `Lower` deliberately
/// carries no owner-write requirement: a read-only source anchor is a valid immutable base, and
/// this observer never writes to it.
fn require_role_trust(
    role: TrustedOverlayRole,
    snapshot: &DirectorySnapshot,
    observer_euid: u32,
) -> Result<(), TrustedOverlayMountPlanError> {
    if snapshot.uid != 0 && snapshot.uid != observer_euid {
        return Err(role_owner_foreign());
    }
    let permissions = snapshot.mode & 0o7777;
    if permissions & 0o022 != 0 {
        return Err(role_untrusted_writable());
    }
    if role.requires_owner_access() && permissions & 0o700 != 0o700 {
        return Err(role_owner_access_missing());
    }
    Ok(())
}

fn observe_directory(
    path: PathBuf,
    role: TrustedOverlayRole,
    observer_euid: u32,
) -> Result<TrustedOverlayDirectory, TrustedOverlayMountPlanError> {
    let snapshot = snapshot_directory(&path)?;
    require_role_trust(role, &snapshot, observer_euid)?;
    let identity = ReviewedFilesystemIdentity::new(
        snapshot.device,
        snapshot.inode,
        snapshot.uid,
        snapshot.gid,
        snapshot.mode & 0o7777,
    )
    .map_err(|_| unsafe_filesystem())?;
    Ok(TrustedOverlayDirectory {
        path,
        identity,
        snapshot,
    })
}

fn snapshot_directory(path: &Path) -> Result<DirectorySnapshot, TrustedOverlayMountPlanError> {
    let symlink_metadata = fs::symlink_metadata(path).map_err(|_| io_error())?;
    if symlink_metadata.file_type().is_symlink() || !symlink_metadata.is_dir() {
        return Err(unsafe_filesystem());
    }
    let canonical = fs::canonicalize(path).map_err(|_| io_error())?;
    if canonical.as_path() != path {
        return Err(unsafe_filesystem());
    }
    let metadata = fs::metadata(path).map_err(|_| io_error())?;
    if !metadata.is_dir() {
        return Err(unsafe_filesystem());
    }
    Ok(DirectorySnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.mode(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

fn open_directory_path(path: &Path) -> Result<OwnedFd, TrustedOverlayMountPlanError> {
    let mut current = rustix_fs::open(Path::new("/"), PROC_DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| io_error())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current =
                    rustix_fs::openat(current.as_fd(), name, PROC_DIRECTORY_FLAGS, Mode::empty())
                        .map_err(|cause| match cause {
                        Errno::LOOP | Errno::NOTDIR => unsafe_filesystem(),
                        _ => io_error(),
                    })?;
            }
            _ => return Err(invalid_path()),
        }
    }
    Ok(current)
}

fn open_held_directory(
    directory: &TrustedOverlayDirectory,
) -> Result<OwnedFd, TrustedOverlayMountPlanError> {
    let current = open_directory_path(&directory.path)?;
    require_held_directory(&current, directory)?;
    Ok(current)
}

fn open_held_parent(
    merged: &TrustedOverlayDirectory,
) -> Result<(OwnedFd, OsString), TrustedOverlayMountPlanError> {
    let parent_path = merged.path.parent().ok_or_else(invalid_path)?;
    let name = merged
        .path
        .file_name()
        .ok_or_else(invalid_path)?
        .to_os_string();
    let parent = open_directory_path(parent_path)?;
    require_merged_parent_binding(&parent, &name, merged)?;
    Ok((parent, name))
}

fn require_merged_parent_binding(
    parent: &OwnedFd,
    name: &OsStr,
    merged: &TrustedOverlayDirectory,
) -> Result<(), TrustedOverlayMountPlanError> {
    let reopened = rustix_fs::openat(parent.as_fd(), name, PROC_DIRECTORY_FLAGS, Mode::empty())
        .map_err(|cause| match cause {
            Errno::LOOP | Errno::NOTDIR => unsafe_filesystem(),
            _ => io_error(),
        })?;
    require_held_directory(&reopened, merged)
}

fn snapshot_held_directory(
    descriptor: &OwnedFd,
) -> Result<DirectorySnapshot, TrustedOverlayMountPlanError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| io_error())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_filesystem());
    }
    Ok(DirectorySnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_filesystem())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_filesystem())?,
    })
}

fn require_held_directory(
    descriptor: &OwnedFd,
    planned: &TrustedOverlayDirectory,
) -> Result<(), TrustedOverlayMountPlanError> {
    if snapshot_held_directory(descriptor)? != planned.snapshot {
        return Err(changed_during_observation());
    }
    Ok(())
}

fn validate_held_roles(descriptors: [&OwnedFd; 4]) -> Result<(), TrustedOverlayMountPlanError> {
    let snapshots = descriptors
        .iter()
        .map(|descriptor| snapshot_held_directory(descriptor))
        .collect::<Result<Vec<_>, _>>()?;
    let identities = snapshots
        .iter()
        .map(|snapshot| (snapshot.device, snapshot.inode))
        .collect::<BTreeSet<_>>();
    if identities.len() != 4 {
        return Err(role_conflict());
    }
    let expected_device = snapshots[0].device;
    if snapshots
        .iter()
        .any(|snapshot| snapshot.device != expected_device)
    {
        return Err(filesystem_mismatch());
    }
    Ok(())
}

/// Confirm one already-admitted role directory is still the exact same object.
///
/// This needs no second `require_role_trust` call: the recorded snapshot only exists because it
/// passed that gate in `observe_directory`, and any later ownership or mode change fails the exact
/// snapshot comparison below as observation drift. The same holds for the descriptor-lease checks
/// in `require_held_directory`.
fn revalidate_directory(
    directory: &TrustedOverlayDirectory,
) -> Result<(), TrustedOverlayMountPlanError> {
    let current = snapshot_directory(&directory.path)?;
    let current_identity = ReviewedFilesystemIdentity::new(
        current.device,
        current.inode,
        current.uid,
        current.gid,
        current.mode & 0o7777,
    )
    .map_err(|_| unsafe_filesystem())?;
    if current != directory.snapshot || current_identity != directory.identity {
        return Err(changed_during_observation());
    }
    Ok(())
}

fn validate_directory_roles(
    directories: [&TrustedOverlayDirectory; 4],
) -> Result<(), TrustedOverlayMountPlanError> {
    let identities = directories
        .iter()
        .map(|directory| (directory.snapshot.device, directory.snapshot.inode))
        .collect::<BTreeSet<_>>();
    if identities.len() != directories.len() {
        return Err(role_conflict());
    }
    let expected_device = directories[0].snapshot.device;
    if directories
        .iter()
        .any(|directory| directory.snapshot.device != expected_device)
    {
        return Err(filesystem_mismatch());
    }
    Ok(())
}

fn require_empty_directory(path: &Path) -> Result<(), TrustedOverlayMountPlanError> {
    let mut entries = fs::read_dir(path).map_err(|_| io_error())?;
    if entries
        .next()
        .transpose()
        .map_err(|_| io_error())?
        .is_some()
    {
        return Err(workdir_not_empty());
    }
    Ok(())
}

fn require_procfs() -> Result<(), TrustedOverlayMountPlanError> {
    let proc = rustix_fs::open(Path::new("/proc"), PROC_DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| io_error())?;
    let stat = rustix_fs::fstatfs(proc.as_fd()).map_err(|_| io_error())?;
    if stat.f_type as u64 != PROC_SUPER_MAGIC {
        return Err(invalid_proc_evidence());
    }
    Ok(())
}

fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, TrustedOverlayMountPlanError> {
    let file = File::open(path).map_err(|_| io_error())?;
    let limit = u64::try_from(max_bytes)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(invalid_proc_evidence)?;
    let mut reader: Take<File> = file.take(limit);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| io_error())?;
    if bytes.len() > max_bytes {
        return Err(invalid_proc_evidence());
    }
    Ok(bytes)
}

fn filesystems_expose_overlay(bytes: &[u8]) -> Result<bool, TrustedOverlayMountPlanError> {
    if bytes.contains(&0) || !bytes.is_ascii() {
        return Err(invalid_proc_evidence());
    }
    Ok(bytes.lines().any(|line| {
        line.split(|byte| byte.is_ascii_whitespace())
            .rfind(|field| !field.is_empty())
            == Some(OVERLAY_FILESYSTEM_NAME)
    }))
}

trait ByteLines {
    fn lines(&self) -> std::slice::Split<'_, u8, fn(&u8) -> bool>;
}

impl ByteLines for [u8] {
    fn lines(&self) -> std::slice::Split<'_, u8, fn(&u8) -> bool> {
        fn newline(byte: &u8) -> bool {
            *byte == b'\n'
        }
        self.split(newline)
    }
}

fn mountinfo_has_mountpoint(
    bytes: &[u8],
    mountpoint: &Path,
) -> Result<bool, TrustedOverlayMountPlanError> {
    if bytes.contains(&0) {
        return Err(invalid_proc_evidence());
    }
    let expected = mountpoint.as_os_str().as_bytes();
    for line in bytes.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(|byte| *byte == b' ');
        let Some(_mount_id) = fields.next() else {
            return Err(invalid_proc_evidence());
        };
        let Some(_parent_id) = fields.next() else {
            return Err(invalid_proc_evidence());
        };
        let Some(_device) = fields.next() else {
            return Err(invalid_proc_evidence());
        };
        let Some(_root) = fields.next() else {
            return Err(invalid_proc_evidence());
        };
        let Some(raw_mountpoint) = fields.next() else {
            return Err(invalid_proc_evidence());
        };
        if decode_mountinfo_field(raw_mountpoint)? == expected {
            return Ok(true);
        }
    }
    Ok(false)
}

fn decode_mountinfo_field(field: &[u8]) -> Result<Vec<u8>, TrustedOverlayMountPlanError> {
    let mut result = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] != b'\\' {
            result.push(field[index]);
            index += 1;
            continue;
        }
        let Some(octal) = field.get(index + 1..index + 4) else {
            return Err(invalid_proc_evidence());
        };
        if !octal.iter().all(u8::is_ascii_digit) || octal.iter().any(|byte| *byte > b'7') {
            return Err(invalid_proc_evidence());
        }
        let value = u16::from(octal[0] - b'0') * 64
            + u16::from(octal[1] - b'0') * 8
            + u16::from(octal[2] - b'0');
        let decoded = u8::try_from(value).map_err(|_| invalid_proc_evidence())?;
        if !matches!(decoded, b'\t' | b'\n' | b' ' | b'\\') {
            return Err(invalid_proc_evidence());
        }
        result.push(decoded);
        index += 4;
    }
    Ok(result)
}

fn validate_absolute_path(path: PathBuf) -> Result<PathBuf, TrustedOverlayMountPlanError> {
    if path.to_str().is_none() || path.parent().is_none() || path.as_os_str() == OsStr::new("/") {
        return Err(invalid_path());
    }
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_path());
    }
    Ok(path)
}

fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, TrustedOverlayMountPlanError> {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| invalid_proc_evidence())
}

const fn error(
    kind: TrustedOverlayMountPlanErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedOverlayMountPlanError {
    TrustedOverlayMountPlanError {
        kind,
        code,
        message,
    }
}

const fn invalid_path() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::InvalidPath,
        "overlay_mount_path_invalid",
        "trusted overlay mount path is outside the reviewed contract",
    )
}

const fn unsafe_filesystem() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::UnsafeFilesystem,
        "overlay_mount_filesystem_unsafe",
        "trusted overlay mount directory is not an exact reviewed real directory",
    )
}

const fn role_owner_foreign() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::UnsafeFilesystem,
        "overlay_mount_role_owner_foreign",
        "trusted overlay mount role is not owned by root or the observing identity",
    )
}

const fn role_untrusted_writable() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::UnsafeFilesystem,
        "overlay_mount_role_untrusted_writable",
        "trusted overlay mount role is writable by an untrusted identity",
    )
}

const fn role_owner_access_missing() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::UnsafeFilesystem,
        "overlay_mount_role_owner_access_missing",
        "trusted overlay writable role must grant full owner access",
    )
}

const fn role_conflict() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::IdentityConflict,
        "overlay_mount_role_conflict",
        "trusted overlay mount roles must resolve to distinct directory identities",
    )
}

const fn filesystem_mismatch() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::FilesystemMismatch,
        "overlay_mount_filesystem_mismatch",
        "trusted overlay mount roles must share one exact filesystem device in v1",
    )
}

const fn workdir_not_empty() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::WorkdirNotEmpty,
        "overlay_mount_workdir_not_empty",
        "trusted overlay work directory must be empty before mount",
    )
}

const fn overlay_unavailable() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::OverlayUnavailable,
        "overlay_mount_kernel_unavailable",
        "running kernel does not expose the reviewed OverlayFS capability",
    )
}

const fn already_mounted() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::AlreadyMounted,
        "overlay_mount_target_already_mounted",
        "trusted overlay merged target is already a mountpoint",
    )
}

const fn changed_during_observation() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::ChangedDuringObservation,
        "overlay_mount_observation_changed",
        "trusted overlay mount evidence changed during observation",
    )
}

const fn invalid_proc_evidence() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::InvalidProcEvidence,
        "overlay_mount_proc_evidence_invalid",
        "trusted overlay kernel evidence is malformed or outside the reviewed bound",
    )
}

const fn plan_authority_mismatch() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::AuthorityMismatch,
        "overlay_mount_plan_authority_mismatch",
        "current trusted overlay authority does not match the sealed mount plan",
    )
}

const fn task_state_invalid() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::AuthorityMismatch,
        "overlay_mount_task_state_invalid",
        "trusted overlay mount planning requires registered-worktree task state",
    )
}

const fn anchor_task_unproven() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::AuthorityMismatch,
        "overlay_mount_anchor_task_unproven",
        "trusted overlay task lease is not active on the exact source anchor",
    )
}

const fn io_error() -> TrustedOverlayMountPlanError {
    error(
        TrustedOverlayMountPlanErrorKind::Io,
        "overlay_mount_observation_io",
        "trusted overlay mount evidence is unavailable",
    )
}

/// Build evidence for descriptor/fake-backend unit tests without requiring the
/// operator to activate OverlayFS. No actual mount backend may consume this plan.
/// Production observation and confirmation are unchanged and this constructor is
/// absent from production builds.
#[cfg(test)]
pub(crate) fn descriptor_test_plan(
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    paths: TrustedOverlayMountPaths,
) -> Result<TrustedOverlayMountPlan, TrustedOverlayMountPlanError> {
    require_mount_authority(source_anchor, task_view)?;
    require_procfs()?;
    let filesystems = read_bounded(Path::new("/proc/filesystems"), MAX_PROC_FILESYSTEMS_BYTES)?;
    let mountinfo = read_bounded(Path::new("/proc/self/mountinfo"), MAX_PROC_MOUNTINFO_BYTES)?;
    let namespace = fs::metadata("/proc/self/ns/mnt").map_err(|_| io_error())?;
    let mut plan = observe_with_evidence(
        source_anchor.binding().clone(),
        task_view.lease().clone(),
        paths,
        b"nodev\toverlay\n",
        &mountinfo,
        (namespace.dev(), namespace.ino()),
        geteuid().as_raw(),
        || {},
    )?;
    // Only the initial OverlayFS capability is synthetic. Subsequent production
    // confirmation must still detect changes to the real kernel evidence,
    // namespace, directory identities, workdir, target, and task authority.
    plan.kernel.filesystems_digest = sha256_digest(&filesystems)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::process::geteuid;

    use super::{
        DirectorySnapshot, TrustedOverlayMountOptionPolicy, TrustedOverlayMountPaths,
        TrustedOverlayMountPlanErrorKind, TrustedOverlayRole, filesystems_expose_overlay,
        mountinfo_has_mountpoint, observe_with_evidence, require_role_trust,
        validate_directory_roles,
    };
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::descriptor_bound_launcher::ReviewedFilesystemIdentity;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
        ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_overlay_task_view::{
        OverlayGitProofObservation, OverlayGitWorktreeObservation, OverlayIndexObservation,
        OverlayMountObservation, OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration,
        OverlaySourceAnchorId, OverlaySourceAnchorRecord, OverlayTaskProcessObservation,
        OverlayTaskViewGeneration, OverlayTaskViewId, OverlayTaskViewLease,
        OverlayTaskViewObservation, OverlayTaskViewRecord,
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        paths: TrustedOverlayMountPaths,
    }

    impl Fixture {
        fn new() -> Self {
            let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "smolrunner-overlay-plan-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            for name in ["lower", "upper", "work", "merged"] {
                let role = root.join(name);
                fs::create_dir(&role).unwrap();
                // Pin the fixture modes instead of inheriting the ambient umask, so a role-trust
                // refusal in a test is the test's own doing.
                fs::set_permissions(&role, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let paths = TrustedOverlayMountPaths::new(
                root.join("lower"),
                root.join("upper"),
                root.join("work"),
                root.join("merged"),
            )
            .unwrap();
            Self { root, paths }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn binding() -> OverlaySourceAnchorBinding {
        OverlaySourceAnchorBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("project-disk").unwrap(),
            ProjectDiskGeneration::new(1).unwrap(),
            ProjectDiskRevision::new(2).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(1).unwrap(),
            ProjectDiskAttachmentGeneration::new(4).unwrap(),
            OverlaySourceAnchorId::parse("anchor-a").unwrap(),
            OverlaySourceAnchorGeneration::new(1).unwrap(),
            CommitId::parse("1111111111111111111111111111111111111111").unwrap(),
            GitTreeId::parse("2222222222222222222222222222222222222222").unwrap(),
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
    }

    fn lease() -> OverlayTaskViewLease {
        OverlayTaskViewLease::new(
            OverlayTaskViewId::parse("task-a").unwrap(),
            OverlayTaskViewGeneration::new(1).unwrap(),
        )
    }

    fn registered_authority() -> (OverlaySourceAnchorRecord, OverlayTaskViewRecord) {
        let task_lease = lease();
        let anchor = OverlaySourceAnchorRecord::new_ready(binding())
            .acquire_task(task_lease.clone())
            .unwrap();
        let task = OverlayTaskViewRecord::new_planned(task_lease, binding())
            .record_worktree_registered(OverlayTaskViewObservation::new(
                OverlayGitWorktreeObservation::Exact,
                OverlayMountObservation::Absent,
                OverlayIndexObservation::Absent,
                OverlayGitProofObservation::NotRun,
                OverlayTaskProcessObservation::Absent,
            ))
            .unwrap();
        (anchor, task)
    }

    /// The identity production observation passes to `observe_with_evidence`.
    fn observer_euid() -> u32 {
        geteuid().as_raw()
    }

    fn filesystems() -> &'static [u8] {
        b"nodev\tsysfs\nnodev\tproc\nnodev\toverlay\n\text4\n\txfs\n"
    }

    fn mountinfo() -> &'static [u8] {
        b"36 25 0:32 / / rw,relatime - ext4 /dev/root rw\n"
    }

    #[test]
    fn mount_authority_requires_registered_task_held_by_exact_anchor() {
        let (anchor, task) = registered_authority();
        super::require_mount_authority(&anchor, &task).unwrap();

        let planned = OverlayTaskViewRecord::new_planned(lease(), binding());
        assert_eq!(
            super::require_mount_authority(&anchor, &planned)
                .unwrap_err()
                .kind(),
            TrustedOverlayMountPlanErrorKind::AuthorityMismatch
        );

        let other_anchor = OverlaySourceAnchorRecord::new_ready(binding());
        assert_eq!(
            super::require_mount_authority(&other_anchor, &task)
                .unwrap_err()
                .code(),
            "overlay_mount_anchor_task_unproven"
        );
    }

    #[test]
    fn descriptor_fixture_preserves_live_production_capability_refusal() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let filesystems = super::read_bounded(
            std::path::Path::new("/proc/filesystems"),
            super::MAX_PROC_FILESYSTEMS_BYTES,
        )
        .unwrap();
        let result =
            super::observe_trusted_overlay_mount_plan(&anchor, &task, fixture.paths.clone());
        if super::filesystems_expose_overlay(&filesystems).unwrap() {
            assert!(result.is_ok());
        } else {
            assert_eq!(
                result.unwrap_err().code(),
                "overlay_mount_kernel_unavailable"
            );
        }
    }

    #[test]
    fn descriptor_fixture_still_detects_kernel_evidence_drift() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let mut plan = super::descriptor_test_plan(&anchor, &task, fixture.paths.clone()).unwrap();
        plan.kernel.filesystems_digest = super::sha256_digest(b"different evidence").unwrap();
        assert_eq!(
            plan.confirm(&anchor, &task).unwrap_err().code(),
            super::changed_during_observation().code()
        );
    }

    #[test]
    fn descriptor_lease_reopens_all_exact_roles_without_exposing_paths() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let plan = super::descriptor_test_plan(&anchor, &task, fixture.paths.clone()).unwrap();
        let lease = plan.open_descriptor_lease(&anchor, &task).unwrap();
        assert_eq!(lease.summary().schema_version(), 1);
        assert_eq!(lease.summary().role_count(), 4);
        assert!(lease.summary().single_filesystem_device());
        lease.confirm(&plan, &anchor, &task).unwrap();
        assert!(!format!("{lease:?}").contains(fixture.root.to_str().unwrap()));
    }

    #[test]
    fn descriptor_lease_retains_exact_merged_parent_and_private_basename() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let plan = super::descriptor_test_plan(&anchor, &task, fixture.paths.clone()).unwrap();
        let lease = plan.open_descriptor_lease(&anchor, &task).unwrap();
        super::require_merged_parent_binding(
            &lease.merged_parent,
            &lease.merged_name,
            &plan.merged,
        )
        .unwrap();
        let debug = format!("{lease:?}");
        assert!(!debug.contains(fixture.root.to_str().unwrap()));
        assert!(!debug.contains("merged"));
    }

    #[test]
    fn descriptor_lease_retains_old_object_while_path_replacement_fails_confirmation() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let plan = super::descriptor_test_plan(&anchor, &task, fixture.paths.clone()).unwrap();
        let lease = plan.open_descriptor_lease(&anchor, &task).unwrap();
        let held_before = super::snapshot_held_directory(&lease.upper).unwrap();
        fs::rename(fixture.root.join("upper"), fixture.root.join("upper-old")).unwrap();
        fs::create_dir(fixture.root.join("upper")).unwrap();
        let held_after = super::snapshot_held_directory(&lease.upper).unwrap();
        assert_eq!(
            (held_after.device, held_after.inode),
            (held_before.device, held_before.inode)
        );
        assert!(matches!(
            lease.confirm(&plan, &anchor, &task).unwrap_err().kind(),
            TrustedOverlayMountPlanErrorKind::ChangedDuringObservation
                | TrustedOverlayMountPlanErrorKind::Io
        ));
    }

    #[test]
    fn descriptor_acquisition_refuses_intermediate_alias_after_plan_sealing() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let plan = super::descriptor_test_plan(&anchor, &task, fixture.paths.clone()).unwrap();
        let parent = fixture.root.parent().unwrap().to_path_buf();
        let original_name = fixture.root.file_name().unwrap().to_owned();
        let moved = parent.join(format!("{}-moved", original_name.to_string_lossy()));
        fs::rename(&fixture.root, &moved).unwrap();
        symlink(&moved, &fixture.root).unwrap();
        assert!(matches!(
            plan.open_descriptor_lease(&anchor, &task)
                .unwrap_err()
                .kind(),
            TrustedOverlayMountPlanErrorKind::UnsafeFilesystem
                | TrustedOverlayMountPlanErrorKind::ChangedDuringObservation
                | TrustedOverlayMountPlanErrorKind::Io
        ));
        fs::remove_file(&fixture.root).unwrap();
        fs::rename(&moved, &fixture.root).unwrap();
    }

    #[test]
    fn sealed_plan_reconfirms_exact_child_lease_across_anchor_revision() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let plan = super::descriptor_test_plan(&anchor, &task, fixture.paths.clone()).unwrap();
        plan.confirm(&anchor, &task).unwrap();
        let draining = anchor.request_draining().unwrap();
        plan.confirm(&draining, &task)
            .expect("an already-leased child remains authorized while its anchor drains");
        let released = draining.release_task(task.lease()).unwrap();
        assert_eq!(
            plan.confirm(&released, &task).unwrap_err().code(),
            "overlay_mount_anchor_task_unproven"
        );
    }

    #[test]
    fn sealed_plan_confirmation_detects_late_workdir_and_directory_drift() {
        let fixture = Fixture::new();
        let (anchor, task) = registered_authority();
        let plan = super::descriptor_test_plan(&anchor, &task, fixture.paths.clone()).unwrap();
        fs::write(fixture.root.join("work/late"), b"x").unwrap();
        assert_eq!(
            plan.confirm(&anchor, &task).unwrap_err().kind(),
            TrustedOverlayMountPlanErrorKind::WorkdirNotEmpty
        );
        fs::remove_file(fixture.root.join("work/late")).unwrap();
        fs::rename(fixture.root.join("upper"), fixture.root.join("upper-old")).unwrap();
        fs::create_dir(fixture.root.join("upper")).unwrap();
        assert!(matches!(
            plan.confirm(&anchor, &task).unwrap_err().kind(),
            TrustedOverlayMountPlanErrorKind::ChangedDuringObservation
                | TrustedOverlayMountPlanErrorKind::Io
        ));
    }

    #[test]
    fn seals_exact_mount_plan_from_read_only_observation() {
        let fixture = Fixture::new();
        let plan = observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mountinfo(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap();
        assert_eq!(
            plan.summary().option_policy(),
            TrustedOverlayMountOptionPolicy::SingleLowerNodevNosuidV1
        );
        assert_eq!(plan.summary().role_count(), 4);
        assert!(plan.summary().single_filesystem_device());
        assert_eq!(plan.source_anchor(), &binding());
        assert_eq!(plan.task_lease(), &lease());
        assert_eq!(plan.kernel().mount_namespace_inode(), 9);
        assert!(!format!("{plan:?}").contains(fixture.root.to_str().unwrap()));
    }

    #[test]
    fn refuses_nonempty_workdir_and_aliases() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("work/occupied"), b"x").unwrap();
        let error = observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mountinfo(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountPlanErrorKind::WorkdirNotEmpty
        );

        let alias = fixture.root.join("lower-alias");
        symlink(fixture.root.join("lower"), &alias).unwrap();
        let alias_paths = TrustedOverlayMountPaths::new(
            alias,
            fixture.root.join("upper"),
            fixture.root.join("work"),
            fixture.root.join("merged"),
        )
        .unwrap();
        let error = observe_with_evidence(
            binding(),
            lease(),
            alias_paths,
            filesystems(),
            mountinfo(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountPlanErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn refuses_kernel_without_overlay_and_existing_mountpoint() {
        let fixture = Fixture::new();
        let error = observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            b"nodev\tproc\n\text4\n",
            mountinfo(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountPlanErrorKind::OverlayUnavailable
        );

        let merged = fixture.root.join("merged");
        let mounted = format!(
            "36 25 0:32 / {} rw,relatime - overlay overlay rw\n",
            merged.display()
        );
        let error = observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mounted.as_bytes(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountPlanErrorKind::AlreadyMounted
        );
    }

    #[test]
    fn detects_directory_drift_before_plan_publication() {
        let fixture = Fixture::new();
        let root = fixture.root.clone();
        let error = observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mountinfo(),
            (4, 9),
            observer_euid(),
            move || {
                fs::rename(root.join("lower"), root.join("lower-old")).unwrap();
                fs::create_dir(root.join("lower")).unwrap();
            },
        )
        .unwrap_err();
        assert!(matches!(
            error.kind(),
            TrustedOverlayMountPlanErrorKind::ChangedDuringObservation
                | TrustedOverlayMountPlanErrorKind::Io
        ));
    }

    #[test]
    fn refuses_mode_drift_after_role_trust_was_observed() {
        for role in ["lower", "upper", "work", "merged"] {
            let fixture = Fixture::new();
            let directory = fixture.root.join(role);
            let error = observe_with_evidence(
                binding(),
                lease(),
                fixture.paths.clone(),
                filesystems(),
                mountinfo(),
                (4, 9),
                observer_euid(),
                || {
                    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
                },
            )
            .unwrap_err();
            assert_eq!(
                error.kind(),
                TrustedOverlayMountPlanErrorKind::ChangedDuringObservation,
                "{role} mode drift must invalidate the admitted snapshot"
            );
        }
    }

    #[test]
    fn path_roles_must_be_lexically_disjoint() {
        let fixture = Fixture::new();
        let nested = TrustedOverlayMountPaths::new(
            fixture.root.join("lower"),
            fixture.root.join("upper"),
            fixture.root.join("upper/work"),
            fixture.root.join("merged"),
        )
        .expect_err("nested role is refused");
        assert_eq!(
            nested.kind(),
            TrustedOverlayMountPlanErrorKind::IdentityConflict
        );
    }

    #[test]
    fn pure_role_validation_refuses_cross_device_and_duplicate_identity() {
        fn directory(device: u64, inode: u64) -> super::TrustedOverlayDirectory {
            let snapshot = DirectorySnapshot {
                device,
                inode,
                uid: 1000,
                gid: 1000,
                mode: 0o40700,
                mtime: 1,
                mtime_nsec: 0,
                ctime: 1,
                ctime_nsec: 0,
            };
            super::TrustedOverlayDirectory {
                path: PathBuf::from(format!("/private/{device}-{inode}")),
                identity: ReviewedFilesystemIdentity::new(device, inode, 1000, 1000, 0o700)
                    .unwrap(),
                snapshot,
            }
        }

        let a = directory(1, 1);
        let b = directory(1, 2);
        let c = directory(1, 3);
        let d = directory(2, 4);
        assert_eq!(
            validate_directory_roles([&a, &b, &c, &d])
                .unwrap_err()
                .kind(),
            TrustedOverlayMountPlanErrorKind::FilesystemMismatch
        );
        assert_eq!(
            validate_directory_roles([&a, &a, &b, &c])
                .unwrap_err()
                .kind(),
            TrustedOverlayMountPlanErrorKind::IdentityConflict
        );
    }

    #[test]
    fn refuses_group_or_world_writable_role_directories() {
        for role in ["lower", "upper", "work", "merged"] {
            for mode in [0o775, 0o757, 0o777] {
                let fixture = Fixture::new();
                let directory = fixture.root.join(role);
                fs::set_permissions(&directory, fs::Permissions::from_mode(mode)).unwrap();
                let error = observe_with_evidence(
                    binding(),
                    lease(),
                    fixture.paths.clone(),
                    filesystems(),
                    mountinfo(),
                    (4, 9),
                    observer_euid(),
                    || {},
                )
                .unwrap_err();
                assert_eq!(error.code(), "overlay_mount_role_untrusted_writable");
                assert_eq!(
                    error.kind(),
                    TrustedOverlayMountPlanErrorKind::UnsafeFilesystem
                );
            }
        }
    }

    #[test]
    fn refuses_a_foreign_owned_role_directory() {
        let fixture = Fixture::new();
        let owner = fs::metadata(fixture.root.join("lower")).unwrap().uid();
        // The fixture roles belong to whoever runs the tests. Observing them as a different
        // identity is exactly what a foreign-owned lower base looks like to production.
        let foreign_euid = owner.checked_add(1).expect("uid has headroom");
        let result = observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mountinfo(),
            (4, 9),
            foreign_euid,
            || {},
        );
        if owner == 0 {
            // Running as root: a root-owned role stays trusted under any observing identity.
            assert!(result.is_ok());
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code(), "overlay_mount_role_owner_foreign");
            assert_eq!(
                error.kind(),
                TrustedOverlayMountPlanErrorKind::UnsafeFilesystem
            );
        }

        // The same roles are admitted for the identity that actually owns them.
        observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mountinfo(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap();
    }

    #[test]
    fn writable_roles_require_owner_access_but_a_read_only_lower_base_does_not() {
        let fixture = Fixture::new();
        let upper = fixture.root.join("upper");
        fs::set_permissions(&upper, fs::Permissions::from_mode(0o355)).unwrap();
        let error = observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mountinfo(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap_err();
        assert_eq!(error.code(), "overlay_mount_role_owner_access_missing");
        fs::set_permissions(&upper, fs::Permissions::from_mode(0o755)).unwrap();

        // An immutable read-only source anchor is a valid lower base.
        fs::set_permissions(
            fixture.root.join("lower"),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        observe_with_evidence(
            binding(),
            lease(),
            fixture.paths.clone(),
            filesystems(),
            mountinfo(),
            (4, 9),
            observer_euid(),
            || {},
        )
        .unwrap();
    }

    #[test]
    fn pure_role_trust_separates_the_read_only_base_from_the_writable_roles() {
        fn snapshot(uid: u32, mode: u32) -> DirectorySnapshot {
            DirectorySnapshot {
                device: 1,
                inode: 2,
                uid,
                gid: 1000,
                mode: 0o40000 | mode,
                mtime: 1,
                mtime_nsec: 0,
                ctime: 1,
                ctime_nsec: 0,
            }
        }

        let roles = [
            TrustedOverlayRole::Lower,
            TrustedOverlayRole::Upper,
            TrustedOverlayRole::Work,
            TrustedOverlayRole::Merged,
        ];
        for role in roles {
            require_role_trust(role, &snapshot(1000, 0o755), 1000).unwrap();
            require_role_trust(role, &snapshot(0, 0o755), 1000).unwrap();
            assert_eq!(
                require_role_trust(role, &snapshot(1001, 0o755), 1000)
                    .unwrap_err()
                    .code(),
                "overlay_mount_role_owner_foreign"
            );
            for mode in [0o775, 0o757, 0o777] {
                assert_eq!(
                    require_role_trust(role, &snapshot(1000, mode), 1000)
                        .unwrap_err()
                        .code(),
                    "overlay_mount_role_untrusted_writable"
                );
            }
        }

        // Only the writable roles demand full owner access.
        require_role_trust(TrustedOverlayRole::Lower, &snapshot(1000, 0o555), 1000).unwrap();
        require_role_trust(TrustedOverlayRole::Merged, &snapshot(1000, 0o555), 1000).unwrap();
        for role in [TrustedOverlayRole::Upper, TrustedOverlayRole::Work] {
            assert_eq!(
                require_role_trust(role, &snapshot(1000, 0o555), 1000)
                    .unwrap_err()
                    .code(),
                "overlay_mount_role_owner_access_missing"
            );
        }
    }

    #[test]
    fn proc_parsers_handle_overlay_and_mountinfo_escapes() {
        assert!(filesystems_expose_overlay(filesystems()).unwrap());
        assert!(!filesystems_expose_overlay(b"nodev\tproc\n\text4\n").unwrap());
        assert!(
            mountinfo_has_mountpoint(
                b"1 0 0:1 / /tmp/a\\040b rw - overlay overlay rw\n",
                Path::new("/tmp/a b")
            )
            .unwrap()
        );
        assert!(!mountinfo_has_mountpoint(mountinfo(), Path::new("/private/target")).unwrap());
        assert!(super::decode_mountinfo_field(b"\\777").is_err());
        assert!(super::decode_mountinfo_field(b"\\041").is_err());
    }
}
