//! Descriptor-bound observation and publication of the canonical user-local `glaeda` launcher.
//!
//! Only fixed platform location classes and freshly verified local-generation-store targets are
//! accepted. The adapter never edits shell configuration, searches arbitrary writable `PATH`
//! entries, invokes elevation, builds binaries, or adopts an unmarked executable. Public output
//! contains symbolic locations and generation identities, never private paths or PATH contents.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};
use rustix::io::{Errno, fcntl_dupfd_cloexec};
use rustix::process::{getegid, geteuid};
use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::local_install_generation_store::{
    LocalInstallGenerationStoreError, LocalInstallGenerationStoreErrorKind,
    LocalInstallLauncherTarget, UnixLocalInstallGenerationStore,
};
use crate::local_install_plan::{
    LauncherDirectoryDisposition, LauncherEntryDisposition, LauncherLocationClass,
    LauncherLocationObservation, LauncherSwitchPlan, LocalInstallGenerationIdentity,
    LocalInstallPlatform,
};

const LAUNCHER_NAME: &str = "glaeda";
const STAGED_LAUNCHER_NAME: &str = ".glaeda.launcher.next";
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallLauncherAuthority {
    ApprovedCurrentUserLauncherOnly,
}

/// Current platform/home/PATH identity retained privately for one operation.
pub struct LocalInstallLauncherContext {
    platform: LocalInstallPlatform,
    home: PathBuf,
    path: OsString,
    owner: (u32, u32),
}

impl fmt::Debug for LocalInstallLauncherContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalInstallLauncherContext")
            .field("platform", &self.platform)
            .field("home", &"<private-home-path>")
            .field("path", &"<private-path-value>")
            .finish_non_exhaustive()
    }
}

impl LocalInstallLauncherContext {
    /// Capture the current user's reviewed platform, HOME and lexical PATH observation.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when HOME is unavailable/noncanonical or the platform is outside
    /// the reviewed Linux/macOS contract.
    pub fn current_user() -> Result<Self, LocalInstallLauncherError> {
        #[cfg(target_os = "linux")]
        let platform = LocalInstallPlatform::Linux;
        #[cfg(target_os = "macos")]
        let platform = LocalInstallPlatform::Macos;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(error(
            LocalInstallLauncherErrorKind::UnsupportedPlatform,
            "the current platform has no reviewed local launcher contract",
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                error(
                    LocalInstallLauncherErrorKind::InvalidContext,
                    "the operator home directory is unavailable",
                )
            })?;
            Self::new(
                platform,
                PathBuf::from(home),
                std::env::var_os("PATH").unwrap_or_default(),
                (geteuid().as_raw(), getegid().as_raw()),
            )
        }
    }

    fn new(
        platform: LocalInstallPlatform,
        home: PathBuf,
        path: OsString,
        owner: (u32, u32),
    ) -> Result<Self, LocalInstallLauncherError> {
        if !canonical_absolute_path(&home) {
            return Err(error(
                LocalInstallLauncherErrorKind::InvalidContext,
                "the operator home directory is not a canonical absolute path",
            ));
        }
        Ok(Self {
            platform,
            home,
            path,
            owner,
        })
    }

    #[cfg(test)]
    fn for_test(home: PathBuf, path: OsString) -> Result<Self, LocalInstallLauncherError> {
        Self::new(
            LocalInstallPlatform::Linux,
            home,
            path,
            (geteuid().as_raw(), getegid().as_raw()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallLauncherObservationReceipt {
    authority: LocalInstallLauncherAuthority,
    platform: LocalInstallPlatform,
    locations: Vec<LauncherLocationObservation>,
}

impl LocalInstallLauncherObservationReceipt {
    #[must_use]
    pub const fn authority(&self) -> LocalInstallLauncherAuthority {
        self.authority
    }

    #[must_use]
    pub const fn platform(&self) -> LocalInstallPlatform {
        self.platform
    }

    #[must_use]
    pub fn locations(&self) -> &[LauncherLocationObservation] {
        &self.locations
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallLauncherPublishDisposition {
    Published,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallLauncherPublishReceipt {
    authority: LocalInstallLauncherAuthority,
    disposition: LocalInstallLauncherPublishDisposition,
    location: LauncherLocationClass,
    generation: LocalInstallGenerationIdentity,
    observation: LauncherLocationObservation,
}

impl LocalInstallLauncherPublishReceipt {
    #[must_use]
    pub const fn authority(&self) -> LocalInstallLauncherAuthority {
        self.authority
    }

    #[must_use]
    pub const fn disposition(&self) -> LocalInstallLauncherPublishDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn location(&self) -> LauncherLocationClass {
        self.location
    }

    #[must_use]
    pub const fn generation(&self) -> &LocalInstallGenerationIdentity {
        &self.generation
    }

    #[must_use]
    pub const fn observation(&self) -> &LauncherLocationObservation {
        &self.observation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallLauncherErrorKind {
    UnsupportedPlatform,
    InvalidContext,
    GenerationStoreUnavailable,
    MissingAcceptedGeneration,
    LocationNotInPath,
    LocationUnavailable,
    UnsafeDirectory,
    NeedsElevation,
    ForeignEntry,
    UnknownEntry,
    Busy,
    Conflict,
    RecoveryRequired,
    Io,
    InjectedFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallLauncherError {
    kind: LocalInstallLauncherErrorKind,
    message: &'static str,
}

impl LocalInstallLauncherError {
    #[must_use]
    pub const fn kind(&self) -> LocalInstallLauncherErrorKind {
        self.kind
    }
}

impl fmt::Display for LocalInstallLauncherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for LocalInstallLauncherError {}

/// Observe all fixed approved launcher classes from fresh store and descriptor evidence.
///
/// # Errors
///
/// Returns a bounded error when the generation store or approved directory evidence cannot be
/// inspected consistently.
pub fn observe_local_install_launchers(
    store: &UnixLocalInstallGenerationStore,
    context: &LocalInstallLauncherContext,
) -> Result<LocalInstallLauncherObservationReceipt, LocalInstallLauncherError> {
    let targets = store.launcher_targets().map_err(map_store_error)?;
    let locations = candidates(context)?
        .into_iter()
        .map(|candidate| observe_candidate(context, &candidate, targets.as_slice()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LocalInstallLauncherObservationReceipt {
        authority: LocalInstallLauncherAuthority::ApprovedCurrentUserLauncherOnly,
        platform: context.platform,
        locations,
    })
}

/// Publish or replay one exact planner-selected canonical launcher.
///
/// The target must be an accepted or retained verified generation. The selected directory is
/// re-observed under a nonblocking directory lock. An absent launcher is created atomically with
/// no-replace symlink semantics. Existing stages and stale launchers are recovery debt: POSIX has
/// no compare-and-unlink or compare-and-exchange primitive that can prove a pathname still names
/// the previously observed inode against a non-cooperating same-user writer, so this adapter never
/// removes a stage or replaces an existing launcher.
///
/// # Errors
///
/// Returns a bounded error for stale/missing target evidence, unsafe/elevated locations,
/// foreign/unknown entries, contention, recovery ambiguity or I/O failure.
pub fn publish_local_install_launcher(
    store: &UnixLocalInstallGenerationStore,
    context: &LocalInstallLauncherContext,
    plan: &LauncherSwitchPlan,
) -> Result<LocalInstallLauncherPublishReceipt, LocalInstallLauncherError> {
    publish_inner(store, context, plan, None)
}

fn publish_inner(
    store: &UnixLocalInstallGenerationStore,
    context: &LocalInstallLauncherContext,
    plan: &LauncherSwitchPlan,
    fault: Option<FaultBoundary>,
) -> Result<LocalInstallLauncherPublishReceipt, LocalInstallLauncherError> {
    let targets = store.launcher_targets().map_err(map_store_error)?;
    let target = targets
        .as_slice()
        .iter()
        .find(|value| value.generation.identity == plan.target_generation)
        .ok_or_else(|| {
            error(
                LocalInstallLauncherErrorKind::MissingAcceptedGeneration,
                "the launcher target is not an accepted or retained generation",
            )
        })?;
    let candidate = candidates(context)?
        .into_iter()
        .find(|value| value.class == plan.location)
        .ok_or_else(|| {
            error(
                LocalInstallLauncherErrorKind::InvalidContext,
                "the requested launcher class is unavailable on this platform",
            )
        })?;
    let rank = path_rank(&context.path, &candidate.path)?;
    if rank.is_none() {
        return Err(error(
            LocalInstallLauncherErrorKind::LocationNotInPath,
            "the requested approved launcher location is not present in PATH",
        ));
    }
    let (directory, directory_stat) = match open_directory(context, &candidate)? {
        DirectoryProbe::Ready { directory, stat } => {
            match directory_disposition(context, &candidate, &stat) {
                LauncherDirectoryDisposition::ReadyUserOwned => (directory, stat),
                LauncherDirectoryDisposition::NeedsElevation => {
                    return Err(error(
                        LocalInstallLauncherErrorKind::NeedsElevation,
                        "the launcher location requires a separate elevation plan",
                    ));
                }
                LauncherDirectoryDisposition::Unsafe => {
                    return Err(error(
                        LocalInstallLauncherErrorKind::UnsafeDirectory,
                        "the launcher directory is unsafe",
                    ));
                }
                LauncherDirectoryDisposition::Unavailable => unreachable!(),
            }
        }
        DirectoryProbe::Unavailable => {
            return Err(error(
                LocalInstallLauncherErrorKind::LocationUnavailable,
                "the launcher directory is unavailable",
            ));
        }
        DirectoryProbe::Unsafe => {
            return Err(error(
                LocalInstallLauncherErrorKind::UnsafeDirectory,
                "the launcher directory is unsafe",
            ));
        }
    };

    let entry_owner = launcher_entry_owner(context, &directory_stat);
    let before = observe_entry(&directory, LAUNCHER_NAME, entry_owner, targets.as_slice())?;
    reject_protected(&before)?;
    let _lock = DirectoryLock::acquire(&directory)?;
    let locked = observe_entry(&directory, LAUNCHER_NAME, entry_owner, targets.as_slice())?;
    if locked != before {
        return Err(error(
            LocalInstallLauncherErrorKind::Conflict,
            "the launcher changed before publication",
        ));
    }
    reject_protected(&locked)?;
    require_stage_absent(&directory)?;
    if owned_digest(&locked) == Some(&plan.target_generation.digest) {
        // Replaying an exact launcher also closes the durability gap when a previous creator lost
        // its receipt before the directory fsync completed.
        sync_directory(&directory)?;
        return receipt(
            plan,
            rank,
            locked,
            LocalInstallLauncherPublishDisposition::Replayed,
        );
    }

    if matches!(locked, LauncherEntryDisposition::Owned { .. }) {
        return Err(error(
            LocalInstallLauncherErrorKind::Conflict,
            "a stale owned launcher requires explicit operator retirement",
        ));
    }

    inject_foreign_launcher_before_create(&directory, fault)?;
    fs::symlinkat(&target.path, &directory, LAUNCHER_NAME).map_err(|errno| {
        if errno == Errno::EXIST {
            error(
                LocalInstallLauncherErrorKind::Conflict,
                "the launcher changed at the publication boundary",
            )
        } else {
            error(
                LocalInstallLauncherErrorKind::Io,
                "the launcher could not be created atomically",
            )
        }
    })?;
    sync_directory(&directory)?;
    maybe_fail(fault, FaultBoundary::LauncherSynchronized)?;
    let published = observe_entry(&directory, LAUNCHER_NAME, entry_owner, targets.as_slice())?;
    if owned_digest(&published) != Some(&plan.target_generation.digest) {
        return Err(error(
            LocalInstallLauncherErrorKind::RecoveryRequired,
            "the published launcher does not name the requested generation",
        ));
    }
    receipt(
        plan,
        rank,
        published,
        LocalInstallLauncherPublishDisposition::Published,
    )
}

fn receipt(
    plan: &LauncherSwitchPlan,
    rank: Option<u16>,
    entry: LauncherEntryDisposition,
    disposition: LocalInstallLauncherPublishDisposition,
) -> Result<LocalInstallLauncherPublishReceipt, LocalInstallLauncherError> {
    let observation = observation(
        plan.location,
        rank,
        LauncherDirectoryDisposition::ReadyUserOwned,
        entry,
    )?;
    Ok(LocalInstallLauncherPublishReceipt {
        authority: LocalInstallLauncherAuthority::ApprovedCurrentUserLauncherOnly,
        disposition,
        location: plan.location,
        generation: plan.target_generation.clone(),
        observation,
    })
}

struct Candidate {
    class: LauncherLocationClass,
    path: PathBuf,
    system: bool,
}

fn candidates(
    context: &LocalInstallLauncherContext,
) -> Result<Vec<Candidate>, LocalInstallLauncherError> {
    let mut values = vec![
        Candidate {
            class: LauncherLocationClass::HomeLocalBin,
            path: context.home.join(".local/bin"),
            system: false,
        },
        Candidate {
            class: LauncherLocationClass::HomeBin,
            path: context.home.join("bin"),
            system: false,
        },
    ];
    if context.platform == LocalInstallPlatform::Macos {
        values.push(Candidate {
            class: LauncherLocationClass::HomebrewBin,
            path: PathBuf::from("/opt/homebrew/bin"),
            system: true,
        });
    }
    values.push(Candidate {
        class: LauncherLocationClass::UsrLocalBin,
        path: PathBuf::from("/usr/local/bin"),
        system: true,
    });
    if values
        .iter()
        .any(|value| !canonical_absolute_path(&value.path))
    {
        return Err(error(
            LocalInstallLauncherErrorKind::InvalidContext,
            "an approved launcher path is not canonical",
        ));
    }
    Ok(values)
}

fn observe_candidate(
    context: &LocalInstallLauncherContext,
    candidate: &Candidate,
    targets: &[LocalInstallLauncherTarget],
) -> Result<LauncherLocationObservation, LocalInstallLauncherError> {
    let rank = path_rank(&context.path, &candidate.path)?;
    if rank.is_none() {
        let directory = match open_directory(context, candidate)? {
            DirectoryProbe::Unavailable => LauncherDirectoryDisposition::Unavailable,
            DirectoryProbe::Unsafe => LauncherDirectoryDisposition::Unsafe,
            DirectoryProbe::Ready { stat, .. } => directory_disposition(context, candidate, &stat),
        };
        return observation(
            candidate.class,
            None,
            directory,
            LauncherEntryDisposition::Absent,
        );
    }
    let (directory, entry) = match open_directory(context, candidate)? {
        DirectoryProbe::Unavailable => (
            LauncherDirectoryDisposition::Unavailable,
            LauncherEntryDisposition::Absent,
        ),
        DirectoryProbe::Unsafe => (
            LauncherDirectoryDisposition::Unsafe,
            LauncherEntryDisposition::Unknown,
        ),
        DirectoryProbe::Ready { directory, stat } => {
            let entry_owner = launcher_entry_owner(context, &stat);
            (
                directory_disposition(context, candidate, &stat),
                observe_entry(&directory, LAUNCHER_NAME, entry_owner, targets)?,
            )
        }
    };
    observation(candidate.class, rank, directory, entry)
}

fn observation(
    location: LauncherLocationClass,
    rank: Option<u16>,
    directory: LauncherDirectoryDisposition,
    entry: LauncherEntryDisposition,
) -> Result<LauncherLocationObservation, LocalInstallLauncherError> {
    LauncherLocationObservation::new(location, rank.is_some(), rank, directory, entry).map_err(
        |_| {
            error(
                LocalInstallLauncherErrorKind::InvalidContext,
                "the launcher observation is internally inconsistent",
            )
        },
    )
}

enum DirectoryProbe {
    Ready {
        directory: OwnedFd,
        stat: rustix::fs::Stat,
    },
    Unavailable,
    Unsafe,
}

fn open_directory(
    context: &LocalInstallLauncherContext,
    candidate: &Candidate,
) -> Result<DirectoryProbe, LocalInstallLauncherError> {
    let mut current = fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
        error(
            LocalInstallLauncherErrorKind::Io,
            "the filesystem root could not be opened",
        )
    })?;
    let mut current_path = PathBuf::from("/");
    for component in candidate.path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current = match fs::openat(&current, component, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(directory) => directory,
            Err(Errno::NOENT) => return Ok(DirectoryProbe::Unavailable),
            Err(Errno::LOOP | Errno::NOTDIR) => return Ok(DirectoryProbe::Unsafe),
            Err(_) => {
                return Err(error(
                    LocalInstallLauncherErrorKind::Io,
                    "an approved launcher path could not be inspected",
                ));
            }
        };
        current_path.push(component);
        if (candidate.system || current_path.starts_with(&context.home))
            && !safe_directory_ancestor(context, candidate, &current)?
        {
            return Ok(DirectoryProbe::Unsafe);
        }
    }
    let stat = fs::fstat(&current).map_err(|_| {
        error(
            LocalInstallLauncherErrorKind::Io,
            "an approved launcher directory could not be inspected",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (!candidate.system && !candidate.path.starts_with(&context.home))
    {
        return Ok(DirectoryProbe::Unsafe);
    }
    Ok(DirectoryProbe::Ready {
        directory: current,
        stat,
    })
}

fn safe_directory_ancestor(
    context: &LocalInstallLauncherContext,
    candidate: &Candidate,
    directory: &OwnedFd,
) -> Result<bool, LocalInstallLauncherError> {
    let stat = fs::fstat(directory).map_err(|_| {
        error(
            LocalInstallLauncherErrorKind::Io,
            "an approved launcher path ancestor could not be inspected",
        )
    })?;
    Ok(FileType::from_raw_mode(stat.st_mode).is_dir()
        && stat.st_mode & 0o022 == 0
        && if candidate.system {
            stat.st_uid == 0 || stat.st_uid == context.owner.0
        } else {
            stat.st_uid == context.owner.0
        })
}

fn directory_disposition(
    context: &LocalInstallLauncherContext,
    candidate: &Candidate,
    stat: &rustix::fs::Stat,
) -> LauncherDirectoryDisposition {
    if stat.st_mode & 0o022 != 0 {
        LauncherDirectoryDisposition::Unsafe
    } else if stat.st_uid == context.owner.0 {
        LauncherDirectoryDisposition::ReadyUserOwned
    } else if candidate.system && stat.st_uid == 0 {
        LauncherDirectoryDisposition::NeedsElevation
    } else {
        LauncherDirectoryDisposition::Unsafe
    }
}

fn launcher_entry_owner(
    context: &LocalInstallLauncherContext,
    directory: &rustix::fs::Stat,
) -> (u32, u32) {
    let group = match context.platform {
        // Linux uses the effective group except when the containing directory requests setgid
        // inheritance. macOS reports a symlink's owner/group attributes from its containing
        // directory, so its exact ownership predicate must use that retained directory evidence.
        LocalInstallPlatform::Linux if directory.st_mode & 0o2000 == 0 => context.owner.1,
        LocalInstallPlatform::Linux | LocalInstallPlatform::Macos => directory.st_gid,
    };
    (context.owner.0, group)
}

fn observe_entry(
    directory: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    targets: &[LocalInstallLauncherTarget],
) -> Result<LauncherEntryDisposition, LocalInstallLauncherError> {
    let stat = match fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(LauncherEntryDisposition::Absent),
        Err(_) => return Ok(LauncherEntryDisposition::Unknown),
    };
    if !FileType::from_raw_mode(stat.st_mode).is_symlink()
        || stat.st_uid != owner.0
        || stat.st_gid != owner.1
        || stat.st_nlink != 1
    {
        return Ok(LauncherEntryDisposition::Foreign);
    }
    let target = match fs::readlinkat(directory, name, Vec::new()) {
        Ok(target) => target,
        Err(_) => return Ok(LauncherEntryDisposition::Unknown),
    };
    let Some(owned) = targets
        .iter()
        .find(|candidate| target.as_bytes() == candidate.path.as_os_str().as_bytes())
    else {
        return Ok(LauncherEntryDisposition::Foreign);
    };
    owned.verify_resolved_path().map_err(map_store_error)?;
    let current = match fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(_) => return Ok(LauncherEntryDisposition::Unknown),
    };
    if !same_entry_snapshot(&stat, &current) {
        return Ok(LauncherEntryDisposition::Unknown);
    }
    Ok(LauncherEntryDisposition::Owned {
        generation_digest: owned.generation.identity.digest.clone(),
    })
}

fn same_entry_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn reject_protected(entry: &LauncherEntryDisposition) -> Result<(), LocalInstallLauncherError> {
    match entry {
        LauncherEntryDisposition::Foreign => Err(error(
            LocalInstallLauncherErrorKind::ForeignEntry,
            "a foreign canonical glaeda launcher is protected",
        )),
        LauncherEntryDisposition::Unknown => Err(error(
            LocalInstallLauncherErrorKind::UnknownEntry,
            "an unclassified canonical glaeda launcher is protected",
        )),
        LauncherEntryDisposition::Absent | LauncherEntryDisposition::Owned { .. } => Ok(()),
    }
}

fn owned_digest(entry: &LauncherEntryDisposition) -> Option<&Sha256Digest> {
    if let LauncherEntryDisposition::Owned { generation_digest } = entry {
        Some(generation_digest)
    } else {
        None
    }
}

fn require_stage_absent(directory: &OwnedFd) -> Result<(), LocalInstallLauncherError> {
    match fs::statat(directory, STAGED_LAUNCHER_NAME, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) => Err(error(
            LocalInstallLauncherErrorKind::RecoveryRequired,
            "a staged launcher requires explicit operator recovery",
        )),
        Err(_) => Err(error(
            LocalInstallLauncherErrorKind::RecoveryRequired,
            "the staged launcher could not be classified without mutation",
        )),
    }
}

struct DirectoryLock(OwnedFd);

impl DirectoryLock {
    fn acquire(directory: &OwnedFd) -> Result<Self, LocalInstallLauncherError> {
        let retained = fcntl_dupfd_cloexec(directory, 0).map_err(|_| {
            error(
                LocalInstallLauncherErrorKind::Io,
                "the launcher directory lock could not be retained",
            )
        })?;
        match fs::flock(&retained, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Self(retained)),
            Err(Errno::AGAIN) => Err(error(
                LocalInstallLauncherErrorKind::Busy,
                "another launcher publisher holds the directory lock",
            )),
            Err(_) => Err(error(
                LocalInstallLauncherErrorKind::Io,
                "the launcher directory could not be locked",
            )),
        }
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        let _ = fs::flock(&self.0, FlockOperation::Unlock);
    }
}

fn sync_directory(directory: impl AsFd) -> Result<(), LocalInstallLauncherError> {
    fs::fsync(directory).map_err(|_| {
        error(
            LocalInstallLauncherErrorKind::Io,
            "the launcher directory could not be synchronized",
        )
    })
}

fn inject_foreign_launcher_before_create(
    _directory: &OwnedFd,
    _fault: Option<FaultBoundary>,
) -> Result<(), LocalInstallLauncherError> {
    #[cfg(test)]
    if _fault == Some(FaultBoundary::ForeignLauncherBeforeCreate) {
        fs::openat(
            _directory,
            LAUNCHER_NAME,
            OFlags::WRONLY
                .union(OFlags::CREATE)
                .union(OFlags::EXCL)
                .union(OFlags::CLOEXEC),
            Mode::RUSR.union(Mode::WUSR),
        )
        .map_err(|_| {
            error(
                LocalInstallLauncherErrorKind::InjectedFailure,
                "the foreign-launcher race fixture could not be installed",
            )
        })?;
    }
    Ok(())
}

fn path_rank(path: &OsStr, candidate: &Path) -> Result<Option<u16>, LocalInstallLauncherError> {
    for (index, entry) in std::env::split_paths(path).enumerate() {
        if entry == candidate {
            return u16::try_from(index).map(Some).map_err(|_| {
                error(
                    LocalInstallLauncherErrorKind::InvalidContext,
                    "PATH has too many components for bounded observation",
                )
            });
        }
    }
    Ok(None)
}

fn canonical_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path.as_os_str().as_bytes().is_empty()
        && path
            .components()
            .all(|value| matches!(value, Component::RootDir | Component::Normal(_)))
}

fn map_store_error(value: LocalInstallGenerationStoreError) -> LocalInstallLauncherError {
    match value.kind() {
        LocalInstallGenerationStoreErrorKind::Busy => error(
            LocalInstallLauncherErrorKind::Busy,
            "another local-install store operation is active",
        ),
        LocalInstallGenerationStoreErrorKind::Conflict => error(
            LocalInstallLauncherErrorKind::Conflict,
            "the local-install generation evidence conflicts",
        ),
        LocalInstallGenerationStoreErrorKind::RecoveryRequired => error(
            LocalInstallLauncherErrorKind::RecoveryRequired,
            "the local-install generation store requires recovery",
        ),
        LocalInstallGenerationStoreErrorKind::UnsupportedPlatform
        | LocalInstallGenerationStoreErrorKind::InvalidLocation
        | LocalInstallGenerationStoreErrorKind::InvalidRequest
        | LocalInstallGenerationStoreErrorKind::CorruptState
        | LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        | LocalInstallGenerationStoreErrorKind::Io
        | LocalInstallGenerationStoreErrorKind::InjectedFailure => error(
            LocalInstallLauncherErrorKind::GenerationStoreUnavailable,
            "the generation store could not supply verified launcher targets",
        ),
    }
}

const fn error(
    kind: LocalInstallLauncherErrorKind,
    message: &'static str,
) -> LocalInstallLauncherError {
    LocalInstallLauncherError { kind, message }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultBoundary {
    LauncherSynchronized,
    #[cfg(test)]
    ForeignLauncherBeforeCreate,
}

fn maybe_fail(
    selected: Option<FaultBoundary>,
    boundary: FaultBoundary,
) -> Result<(), LocalInstallLauncherError> {
    if selected == Some(boundary) {
        Err(error(
            LocalInstallLauncherErrorKind::InjectedFailure,
            "an injected launcher publication boundary failed",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self as std_fs, File, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::artifact::{CommitId, GitTreeId};
    use crate::local_install_generation_store::{
        LocalInstallGenerationPublishRequest, LocalInstallStoreOperationId,
    };
    use crate::local_install_plan::{
        BuiltLocalBinaryEvidence, InstalledLocalBinaryGeneration, LocalInstallBuildPlan,
        LocalInstallSourceIdentity, LocalInstallToolchainIdentity, complete_local_install_build,
    };

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct World {
        root: PathBuf,
        home: PathBuf,
        store: UnixLocalInstallGenerationStore,
    }

    impl World {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
            let temporary_root = std_fs::canonicalize(std::env::temp_dir())
                .expect("canonicalize test temporary directory");
            let root = temporary_root.join(format!(
                "glaeda-local-launcher-{label}-{}-{sequence}",
                std::process::id()
            ));
            let home = root.join("home");
            let data = root.join("data");
            for path in [
                root.clone(),
                home.clone(),
                data.clone(),
                home.join(".local"),
                home.join(".local/bin"),
            ] {
                std_fs::create_dir(&path).expect("create exact test directory");
                std_fs::set_permissions(&path, std_fs::Permissions::from_mode(0o700))
                    .expect("set exact test mode");
            }
            let store = UnixLocalInstallGenerationStore::open_for_test(&data)
                .expect("open test generation store");
            Self { root, home, store }
        }

        fn launcher_dir(&self) -> PathBuf {
            self.home.join(".local/bin")
        }

        fn context(&self, entries: &[&Path]) -> LocalInstallLauncherContext {
            LocalInstallLauncherContext::for_test(
                self.home.clone(),
                std::env::join_paths(entries).expect("join PATH"),
            )
            .expect("test context")
        }

        fn context_with_platform_owner(
            &self,
            entries: &[&Path],
            platform: LocalInstallPlatform,
            owner: (u32, u32),
        ) -> LocalInstallLauncherContext {
            LocalInstallLauncherContext::new(
                platform,
                self.home.clone(),
                std::env::join_paths(entries).expect("join PATH"),
                owner,
            )
            .expect("test context")
        }

        fn binary(&self, label: &str, bytes: &[u8]) -> PathBuf {
            let path = self.root.join(format!("candidate-{label}"));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o500)
                .open(&path)
                .expect("create candidate binary");
            file.write_all(bytes).expect("write candidate binary");
            file.sync_all().expect("sync candidate binary");
            std_fs::set_permissions(&path, std_fs::Permissions::from_mode(0o500))
                .expect("set candidate mode");
            path
        }

        fn publish_generation(
            &self,
            predecessor: Option<LocalInstallGenerationIdentity>,
            marker: char,
        ) -> InstalledLocalBinaryGeneration {
            let bytes = format!("glaeda-generation-{marker}").into_bytes();
            let binary = self.binary(&marker.to_string(), &bytes);
            let candidate = generation(predecessor.clone(), marker, &bytes);
            let request = LocalInstallGenerationPublishRequest::new(
                operation(marker),
                predecessor,
                candidate.clone(),
            )
            .expect("publish request");
            self.store
                .publish(&request, binary)
                .expect("publish generation");
            candidate
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            std_fs::remove_dir_all(&self.root).expect("remove exact test root");
        }
    }

    fn digest(bytes: &[u8]) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes))).expect("digest")
    }

    fn generation(
        predecessor: Option<LocalInstallGenerationIdentity>,
        marker: char,
        bytes: &[u8],
    ) -> InstalledLocalBinaryGeneration {
        let source = LocalInstallSourceIdentity::new(
            CommitId::parse(&marker.to_string().repeat(40)).expect("commit"),
            GitTreeId::parse(&((marker as u8 + 1) as char).to_string().repeat(40)).expect("tree"),
            Sha256Digest::parse(&format!("sha256:{}", marker.to_string().repeat(64)))
                .expect("lock digest"),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-x86_64-unknown-linux-gnu")
                .expect("toolchain"),
        )
        .expect("source");
        let build = LocalInstallBuildPlan {
            target_generation: predecessor.as_ref().map_or(1, |value| value.number + 1),
            expected_predecessor: predecessor.clone(),
            source: source.clone(),
        };
        complete_local_install_build(
            &build,
            BuiltLocalBinaryEvidence::new(
                source.digest().clone(),
                predecessor,
                digest(bytes),
                format!("glaeda 0.1.{marker}"),
            )
            .expect("build evidence"),
        )
        .expect("generation")
    }

    fn operation(marker: char) -> LocalInstallStoreOperationId {
        LocalInstallStoreOperationId::parse(&format!("sha256:{}", marker.to_string().repeat(64)))
            .expect("operation")
    }

    fn plan(generation: &InstalledLocalBinaryGeneration) -> LauncherSwitchPlan {
        LauncherSwitchPlan {
            target_generation: generation.identity.clone(),
            location: LauncherLocationClass::HomeLocalBin,
        }
    }

    fn home(receipt: &LocalInstallLauncherObservationReceipt) -> &LauncherLocationObservation {
        receipt
            .locations()
            .iter()
            .find(|value| value.location == LauncherLocationClass::HomeLocalBin)
            .expect("home-local-bin observation")
    }

    #[test]
    fn first_exact_path_rank_wins_and_arbitrary_path_is_ignored() {
        let world = World::new("path-rank");
        let arbitrary = world.root.join("arbitrary");
        std_fs::create_dir(&arbitrary).expect("arbitrary PATH directory");
        let launcher = world.launcher_dir();
        let context = world.context(&[&arbitrary, &launcher, &launcher]);
        let receipt = observe_local_install_launchers(&world.store, &context).expect("observe");
        assert_eq!(home(&receipt).path_rank, Some(1));
        assert_eq!(home(&receipt).entry, LauncherEntryDisposition::Absent);
        assert_eq!(receipt.locations().len(), 3);
    }

    #[test]
    fn absent_publish_and_exact_replay_converge() {
        let world = World::new("publish-replay");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect("publish")
                .disposition(),
            LocalInstallLauncherPublishDisposition::Published
        );
        assert!(
            std_fs::read_link(launcher.join(LAUNCHER_NAME))
                .expect("launcher target")
                .is_absolute()
        );
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect("replay")
                .disposition(),
            LocalInstallLauncherPublishDisposition::Replayed
        );
    }

    #[test]
    fn stale_owned_launcher_is_never_replaced_by_basename() {
        let world = World::new("owned-stale");
        let first = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        publish_local_install_launcher(&world.store, &context, &plan(&first))
            .expect("publish first launcher");
        let old = std_fs::read_link(launcher.join(LAUNCHER_NAME)).expect("old target");
        let second = world.publish_generation(Some(first.identity.clone()), 'b');
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&second))
                .expect_err("stale launcher blocked")
                .kind(),
            LocalInstallLauncherErrorKind::Conflict
        );
        assert_eq!(
            std_fs::read_link(launcher.join(LAUNCHER_NAME)).expect("retained old target"),
            old
        );
        assert!(!launcher.join(STAGED_LAUNCHER_NAME).exists());
    }

    #[test]
    fn foreign_entry_is_protected_before_publication() {
        let world = World::new("foreign");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        std_fs::write(launcher.join(LAUNCHER_NAME), b"foreign").expect("foreign launcher");
        let context = world.context(&[&launcher]);
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("foreign blocked")
                .kind(),
            LocalInstallLauncherErrorKind::ForeignEntry
        );
        assert_eq!(
            std_fs::read(launcher.join(LAUNCHER_NAME)).expect("foreign retained"),
            b"foreign"
        );
        assert!(!launcher.join(STAGED_LAUNCHER_NAME).exists());
    }

    #[test]
    fn symlinked_approved_directory_is_unsafe() {
        let world = World::new("symlink-directory");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let moved = world.root.join("moved-bin");
        std_fs::rename(&launcher, &moved).expect("move launcher directory");
        symlink(&moved, &launcher).expect("symlink launcher directory");
        let context = world.context(&[&launcher]);
        let receipt = observe_local_install_launchers(&world.store, &context).expect("observe");
        assert_eq!(
            home(&receipt).directory,
            LauncherDirectoryDisposition::Unsafe
        );
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("unsafe blocked")
                .kind(),
            LocalInstallLauncherErrorKind::UnsafeDirectory
        );
    }

    #[test]
    fn writable_home_ancestor_is_unsafe() {
        let world = World::new("writable-home-ancestor");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        std_fs::set_permissions(
            world.home.join(".local"),
            std_fs::Permissions::from_mode(0o770),
        )
        .expect("make launcher ancestor writable");
        let context = world.context(&[&launcher]);
        let receipt = observe_local_install_launchers(&world.store, &context).expect("observe");
        assert_eq!(
            home(&receipt).directory,
            LauncherDirectoryDisposition::Unsafe
        );
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("unsafe ancestor blocked")
                .kind(),
            LocalInstallLauncherErrorKind::UnsafeDirectory
        );
    }

    #[test]
    fn linux_setgid_directory_group_is_the_created_launcher_group() {
        let world = World::new("setgid-directory-group");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        if let Err(error) =
            std_fs::set_permissions(&launcher, std_fs::Permissions::from_mode(0o2700))
        {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("set setgid launcher mode: {error}");
        }
        let actual_group = fs::fstat(
            fs::open(&launcher, DIRECTORY_FLAGS, Mode::empty()).expect("open launcher directory"),
        )
        .expect("launcher directory stat")
        .st_gid;
        let captured_effective_group = actual_group ^ 1;
        let context = world.context_with_platform_owner(
            &[&launcher],
            LocalInstallPlatform::Linux,
            (geteuid().as_raw(), captured_effective_group),
        );

        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect("publish with Linux setgid inheritance")
                .disposition(),
            LocalInstallLauncherPublishDisposition::Published
        );
        let link = fs::statat(
            fs::open(&launcher, DIRECTORY_FLAGS, Mode::empty()).expect("open launcher directory"),
            LAUNCHER_NAME,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .expect("launcher stat");
        assert_eq!(link.st_gid, actual_group);
        let receipt = observe_local_install_launchers(&world.store, &context).expect("observe");
        assert_eq!(
            home(&receipt).entry,
            LauncherEntryDisposition::Owned {
                generation_digest: generation.identity.digest
            }
        );
    }

    #[test]
    fn linux_non_setgid_directory_uses_the_effective_group() {
        let world = World::new("non-setgid-directory-group");
        let launcher = world.launcher_dir();
        let mut directory_stat = fs::fstat(
            fs::open(&launcher, DIRECTORY_FLAGS, Mode::empty()).expect("open launcher directory"),
        )
        .expect("launcher directory stat");
        assert_eq!(directory_stat.st_mode & 0o2000, 0);
        directory_stat.st_gid ^= 1;
        let effective_group = directory_stat.st_gid ^ 2;
        let context = world.context_with_platform_owner(
            &[&launcher],
            LocalInstallPlatform::Linux,
            (geteuid().as_raw(), effective_group),
        );

        assert_eq!(
            launcher_entry_owner(&context, &directory_stat),
            (geteuid().as_raw(), effective_group)
        );
    }

    #[test]
    fn macos_directory_group_is_the_created_launcher_group() {
        let world = World::new("macos-directory-group");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let actual_group = fs::fstat(
            fs::open(&launcher, DIRECTORY_FLAGS, Mode::empty()).expect("open launcher directory"),
        )
        .expect("launcher directory stat")
        .st_gid;
        let captured_effective_group = actual_group ^ 1;
        let context = world.context_with_platform_owner(
            &[&launcher],
            LocalInstallPlatform::Macos,
            (geteuid().as_raw(), captured_effective_group),
        );

        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect("publish with macOS directory-group ownership")
                .disposition(),
            LocalInstallLauncherPublishDisposition::Published
        );
        let receipt = observe_local_install_launchers(&world.store, &context).expect("observe");
        assert_eq!(
            home(&receipt).entry,
            LauncherEntryDisposition::Owned {
                generation_digest: generation.identity.digest
            }
        );
    }

    #[test]
    fn unaccepted_generation_never_mutates_launcher() {
        let world = World::new("missing-generation");
        let generation = generation(None, 'a', b"not-published");
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("missing generation")
                .kind(),
            LocalInstallLauncherErrorKind::MissingAcceptedGeneration
        );
        assert!(!launcher.join(LAUNCHER_NAME).exists());
    }

    #[test]
    fn retained_target_guard_blocks_generation_retirement_during_launcher_work() {
        let world = World::new("store-lock");
        let first = world.publish_generation(None, 'a');
        let locked_targets = world
            .store
            .launcher_targets()
            .expect("hold shared store lock");
        let bytes = b"glaeda-generation-b";
        let binary = world.binary("b", bytes);
        let second = generation(Some(first.identity.clone()), 'b', bytes);
        let request =
            LocalInstallGenerationPublishRequest::new(operation('b'), Some(first.identity), second)
                .expect("successor request");
        assert_eq!(
            world
                .store
                .publish(&request, binary)
                .expect_err("exclusive store writer blocked")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Busy
        );
        assert_eq!(locked_targets.as_slice().len(), 1);
    }

    #[test]
    fn competing_launcher_writer_fails_fast_without_staging() {
        let world = World::new("directory-lock");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        let candidate = candidates(&context)
            .expect("candidates")
            .into_iter()
            .find(|value| value.class == LauncherLocationClass::HomeLocalBin)
            .expect("home candidate");
        let directory = match open_directory(&context, &candidate).expect("open directory") {
            DirectoryProbe::Ready { directory, .. } => directory,
            DirectoryProbe::Unavailable | DirectoryProbe::Unsafe => panic!("ready directory"),
        };
        let _held = DirectoryLock::acquire(&directory).expect("hold directory lock");
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("competing writer blocked")
                .kind(),
            LocalInstallLauncherErrorKind::Busy
        );
        assert!(!launcher.join(STAGED_LAUNCHER_NAME).exists());
    }

    #[test]
    fn receipt_crash_converges_by_exact_replay_without_a_stage() {
        let world = World::new("receipt-crash");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        assert_eq!(
            publish_inner(
                &world.store,
                &context,
                &plan(&generation),
                Some(FaultBoundary::LauncherSynchronized),
            )
            .expect_err("injected failure")
            .kind(),
            LocalInstallLauncherErrorKind::InjectedFailure
        );
        assert!(launcher.join(LAUNCHER_NAME).is_symlink());
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect("replay after lost receipt")
                .disposition(),
            LocalInstallLauncherPublishDisposition::Replayed
        );
        assert!(!launcher.join(STAGED_LAUNCHER_NAME).exists());
    }

    #[test]
    fn final_create_race_never_replaces_foreign_launcher() {
        let world = World::new("final-create-race");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        assert_eq!(
            publish_inner(
                &world.store,
                &context,
                &plan(&generation),
                Some(FaultBoundary::ForeignLauncherBeforeCreate),
            )
            .expect_err("foreign final-race winner blocked")
            .kind(),
            LocalInstallLauncherErrorKind::Conflict
        );
        let metadata = std_fs::symlink_metadata(launcher.join(LAUNCHER_NAME))
            .expect("foreign launcher retained");
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.len(), 0);
        assert!(!launcher.join(STAGED_LAUNCHER_NAME).exists());
    }

    #[test]
    fn foreign_stage_is_not_deleted_by_basename() {
        let world = World::new("foreign-stage");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        symlink("/foreign/target", launcher.join(STAGED_LAUNCHER_NAME)).expect("foreign stage");
        let context = world.context(&[&launcher]);
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("foreign stage blocked")
                .kind(),
            LocalInstallLauncherErrorKind::RecoveryRequired
        );
        assert_eq!(
            std_fs::read_link(launcher.join(STAGED_LAUNCHER_NAME)).expect("stage retained"),
            PathBuf::from("/foreign/target")
        );
    }

    #[test]
    fn apparently_owned_stage_is_retained_for_explicit_recovery() {
        let world = World::new("owned-stage");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let target = world
            .store
            .launcher_targets()
            .expect("verified target")
            .as_slice()[0]
            .path
            .clone();
        symlink(&target, launcher.join(STAGED_LAUNCHER_NAME)).expect("owned-looking stage");
        let context = world.context(&[&launcher]);
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("stage requires recovery")
                .kind(),
            LocalInstallLauncherErrorKind::RecoveryRequired
        );
        assert_eq!(
            std_fs::read_link(launcher.join(STAGED_LAUNCHER_NAME)).expect("stage retained"),
            target
        );
    }

    #[test]
    fn store_ancestor_rebind_cannot_publish_lexical_target() {
        let world = World::new("store-ancestor-rebind");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        publish_local_install_launcher(&world.store, &context, &plan(&generation))
            .expect("publish before rebind");
        let target = std_fs::read_link(launcher.join(LAUNCHER_NAME)).expect("original target");
        let original = world.root.join("data-original");
        std_fs::rename(world.root.join("data"), &original).expect("move retained data root");
        std_fs::create_dir(world.root.join("data")).expect("replace data ancestor");
        std_fs::set_permissions(
            world.root.join("data"),
            std_fs::Permissions::from_mode(0o700),
        )
        .expect("replacement mode");

        assert_eq!(
            observe_local_install_launchers(&world.store, &context)
                .expect_err("rebound launcher is never reported owned")
                .kind(),
            LocalInstallLauncherErrorKind::GenerationStoreUnavailable
        );
        assert_eq!(
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect_err("rebound absolute target blocked")
                .kind(),
            LocalInstallLauncherErrorKind::GenerationStoreUnavailable
        );
        assert_eq!(
            std_fs::read_link(launcher.join(LAUNCHER_NAME)).expect("launcher left untouched"),
            target
        );
    }

    #[test]
    fn public_output_and_debug_never_contain_private_paths() {
        let world = World::new("privacy");
        let generation = world.publish_generation(None, 'a');
        let launcher = world.launcher_dir();
        let context = world.context(&[&launcher]);
        let publish = publish_local_install_launcher(&world.store, &context, &plan(&generation))
            .expect("publish");
        let observe = observe_local_install_launchers(&world.store, &context).expect("observe");
        for value in [
            serde_json::to_string(&publish).expect("publish JSON"),
            serde_json::to_string(&observe).expect("observe JSON"),
            format!("{context:?}"),
        ] {
            assert!(!value.contains(world.root.to_str().expect("utf8 test path")));
            assert!(!value.contains("candidate-a"));
        }
    }

    #[test]
    #[ignore = "physical launcher control matrix; run explicitly at an exact head"]
    fn physical_launcher_control_matrix() {
        const SAMPLES: usize = 20;
        const BINARY_BYTES: usize = 8 * 1024 * 1024;

        fn summary(mut values: Vec<Duration>) -> serde_json::Value {
            values.sort_unstable();
            let p90 = (values.len() * 9).div_ceil(10).saturating_sub(1);
            serde_json::json!({
                "minimum_ns": values[0].as_nanos(),
                "p50_ns": values[values.len() / 2].as_nanos(),
                "p90_ns": values[p90].as_nanos(),
                "maximum_ns": values[values.len() - 1].as_nanos(),
            })
        }

        fn sync(path: &Path) {
            File::open(path)
                .expect("open directory")
                .sync_all()
                .expect("sync directory");
        }

        let world = World::new("physical");
        let mut binary_bytes = vec![0_u8; BINARY_BYTES];
        for (index, byte) in binary_bytes.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let binary = world.binary("physical", &binary_bytes);
        let generation = generation(None, 'a', &binary_bytes);
        world
            .store
            .publish(
                &LocalInstallGenerationPublishRequest::new(
                    operation('a'),
                    None,
                    generation.clone(),
                )
                .expect("physical publish request"),
                binary,
            )
            .expect("seed physical generation");
        drop(binary_bytes);
        let locked_targets = world.store.launcher_targets().expect("verified target");
        let exact_target = locked_targets
            .as_slice()
            .first()
            .expect("accepted target")
            .path
            .clone();
        let mut naive = Vec::new();
        let mut atomic = Vec::new();
        let mut exact_publish = Vec::new();
        let mut exact_replay = Vec::new();
        let mut exact_observe = Vec::new();

        for index in 0..SAMPLES {
            let directory = world.root.join(format!("naive-{index}"));
            std_fs::create_dir(&directory).expect("naive directory");
            let started = Instant::now();
            symlink(&exact_target, directory.join(LAUNCHER_NAME)).expect("naive symlink");
            sync(&directory);
            assert_eq!(
                std_fs::read_link(directory.join(LAUNCHER_NAME)).expect("naive target"),
                exact_target
            );
            naive.push(started.elapsed());

            let directory = world.root.join(format!("atomic-{index}"));
            std_fs::create_dir(&directory).expect("atomic directory");
            let started = Instant::now();
            symlink(&exact_target, directory.join("glaeda.next")).expect("atomic stage");
            sync(&directory);
            std_fs::rename(directory.join("glaeda.next"), directory.join(LAUNCHER_NAME))
                .expect("atomic rename");
            sync(&directory);
            assert_eq!(
                std_fs::read_link(directory.join(LAUNCHER_NAME)).expect("atomic target"),
                exact_target
            );
            atomic.push(started.elapsed());

            let home = world.root.join(format!("exact-{index}"));
            let directory = home.join(".local/bin");
            std_fs::create_dir_all(&directory).expect("exact directory");
            for path in [&home, &home.join(".local"), &directory] {
                std_fs::set_permissions(path, std_fs::Permissions::from_mode(0o700))
                    .expect("exact mode");
            }
            let context = LocalInstallLauncherContext::for_test(
                home,
                std::env::join_paths([&directory]).expect("exact PATH"),
            )
            .expect("exact context");
            let started = Instant::now();
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect("exact publish");
            exact_publish.push(started.elapsed());
            let started = Instant::now();
            publish_local_install_launcher(&world.store, &context, &plan(&generation))
                .expect("exact replay");
            exact_replay.push(started.elapsed());
            let started = Instant::now();
            observe_local_install_launchers(&world.store, &context).expect("exact observe");
            exact_observe.push(started.elapsed());
        }

        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "authority": "performance_observation_only",
                "samples_per_arm": SAMPLES,
                "binary_bytes": BINARY_BYTES,
                "semantic_validator": "every accepted launcher resolved to the exact descriptor-verified generation target; Glaeda also validated the store, absolute target resolution, approved PATH class, directory ownership/mode, symlink ownership and no-replace publication shape",
                "controls": {
                    "naive_direct_symlink_directory_fsync_validate": summary(naive),
                    "typical_stage_rename_directory_fsync_validate": summary(atomic),
                },
                "glaeda": {
                    "exact_publish_validate": summary(exact_publish),
                    "exact_replay_validate": summary(exact_replay),
                    "exact_observe": summary(exact_observe),
                },
            })
        );
    }
}
