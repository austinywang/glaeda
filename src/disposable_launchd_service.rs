//! Exact plan and explicitly approved apply boundary for the macOS disposable-worker LaunchAgent.
//!
//! Planning creates no files and invokes no command. A domain-separated plan identity binds the
//! exact executable, enrollment, LaunchAgent path, property-list bytes, user domain, and ordered
//! compensation contract. The macOS apply path requires that exact approval, revalidates protected
//! inputs, serializes publication with a private lock, atomically publishes complete private bytes,
//! and uses only bounded fixed-shape `launchctl` calls. Only bounded path-free reports are public.

pub mod upgrade;

use std::fmt;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::fs::File;
#[cfg(target_os = "macos")]
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::journal::RollbackClass;
#[cfg(target_os = "macos")]
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};

#[cfg(target_os = "macos")]
use rustix::fs::{self as rustix_fs, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags};

pub const DISPOSABLE_LAUNCHD_SERVICE_PLAN_SCHEMA_VERSION: u8 = 1;
pub const DISPOSABLE_LAUNCHD_SERVICE_LABEL: &str = "io.smolrunner.disposable-worker";
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const MAX_UID: u32 = 2_147_483_647;
#[cfg(target_os = "macos")]
const MAX_PROGRAM_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(target_os = "macos")]
const MAX_ENROLLMENT_BYTES: u64 = 64 * 1024;
#[cfg(target_os = "macos")]
const MAX_PLIST_BYTES: u64 = 64 * 1024;
#[cfg(target_os = "macos")]
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(15);
// The longest bridge exchange is the official Scale Set long poll, bounded at 75 seconds. Give
// SIGTERM enough time to return from that exchange so normal Rust Drop can terminate and reap the
// secret-bearing bridge child before launchd escalates to SIGKILL.
const SERVICE_EXIT_TIMEOUT_SECONDS: u16 = 90;
#[cfg(target_os = "macos")]
const LAUNCHCTL_PROGRAM: &str = "/bin/launchctl";
const APPLY_LOCK: &str = ".io.smolrunner.disposable-worker.apply.lock";
const STAGED_PLIST_PREFIX: &str = ".io.smolrunner.disposable-worker.plist.next.";
#[cfg(target_os = "macos")]
const SYSTEM_RANDOM_SOURCE: &str = "/dev/urandom";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceDesiredState {
    Installed,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceActionKind {
    PublishConfiguration,
    BootstrapService,
    BootoutService,
    RemoveConfiguration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServiceActionReport {
    sequence: u8,
    kind: DisposableLaunchdServiceActionKind,
    summary: &'static str,
    rollback: RollbackClass,
}

impl DisposableLaunchdServiceActionReport {
    #[must_use]
    pub const fn sequence(&self) -> u8 {
        self.sequence
    }

    #[must_use]
    pub const fn kind(&self) -> DisposableLaunchdServiceActionKind {
        self.kind
    }

    #[must_use]
    pub const fn summary(&self) -> &'static str {
        self.summary
    }

    #[must_use]
    pub const fn rollback(&self) -> RollbackClass {
        self.rollback
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServicePlanReport {
    schema_version: u8,
    desired_state: DisposableLaunchdServiceDesiredState,
    service_label: &'static str,
    service_scope: &'static str,
    launchd_domain: String,
    plan_identity: Sha256Digest,
    configuration_mode: u32,
    preconditions: Vec<&'static str>,
    actions: Vec<DisposableLaunchdServiceActionReport>,
    requires_operator_approval: bool,
}

impl DisposableLaunchdServicePlanReport {
    #[must_use]
    pub const fn desired_state(&self) -> DisposableLaunchdServiceDesiredState {
        self.desired_state
    }

    #[must_use]
    pub fn launchd_domain(&self) -> &str {
        &self.launchd_domain
    }

    #[must_use]
    pub fn plan_identity(&self) -> &Sha256Digest {
        &self.plan_identity
    }

    #[must_use]
    pub fn actions(&self) -> &[DisposableLaunchdServiceActionReport] {
        &self.actions
    }

    #[must_use]
    pub fn preconditions(&self) -> &[&'static str] {
        &self.preconditions
    }
}

pub struct DisposableLaunchdServicePlan {
    report: DisposableLaunchdServicePlanReport,
    operator_uid: u32,
    program: PathBuf,
    program_digest: Sha256Digest,
    enrollment: PathBuf,
    enrollment_digest: Sha256Digest,
    launch_agent: PathBuf,
    plist: Vec<u8>,
}

impl fmt::Debug for DisposableLaunchdServicePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLaunchdServicePlan")
            .field("report", &self.report)
            .finish()
    }
}

impl DisposableLaunchdServicePlan {
    #[must_use]
    pub const fn report(&self) -> &DisposableLaunchdServicePlanReport {
        &self.report
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceApplyDisposition {
    Satisfied,
    Installed,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServiceApplyReport {
    schema_version: u8,
    desired_state: DisposableLaunchdServiceDesiredState,
    disposition: DisposableLaunchdServiceApplyDisposition,
    service_label: &'static str,
    plan_identity: Sha256Digest,
}

impl DisposableLaunchdServiceApplyReport {
    #[must_use]
    pub const fn disposition(&self) -> DisposableLaunchdServiceApplyDisposition {
        self.disposition
    }

    #[must_use]
    pub fn plan_identity(&self) -> &Sha256Digest {
        &self.plan_identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceApplyErrorKind {
    ApprovalRequired,
    UnsafeState,
    CommandFailed,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServiceApplyError {
    kind: DisposableLaunchdServiceApplyErrorKind,
    code: &'static str,
}

impl DisposableLaunchdServiceApplyError {
    #[must_use]
    pub const fn kind(self) -> DisposableLaunchdServiceApplyErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableLaunchdServiceApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLaunchdServiceApplyError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableLaunchdServiceApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("disposable-worker LaunchAgent apply was refused")
    }
}

impl std::error::Error for DisposableLaunchdServiceApplyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServicePlanErrorKind {
    InvalidConfiguration,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServicePlanError {
    kind: DisposableLaunchdServicePlanErrorKind,
    code: &'static str,
}

impl DisposableLaunchdServicePlanError {
    #[must_use]
    pub const fn kind(self) -> DisposableLaunchdServicePlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableLaunchdServicePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLaunchdServicePlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableLaunchdServicePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("disposable-worker LaunchAgent configuration is invalid")
    }
}

impl std::error::Error for DisposableLaunchdServicePlanError {}

/// Apply one exact, explicitly approved LaunchAgent plan.
///
/// Publication writes one unpredictable private stage and atomically renames it with no replacement,
/// so a crash leaves either no final property list or the complete exact bytes. Unknown abandoned
/// stages are never ownership or deletion authority. The exact property list is itself the durable
/// pre-command checkpoint: retries synchronize its directory, then observe it and the exact launchd
/// job before deciding whether another command is needed. Removal confirms the exact loaded path and
/// program, boots that job out, confirms absence, and only then unlinks an exact-byte-matching plist.
///
/// # Errors
///
/// Returns a path-free error unless approval, protected inputs, filesystem state, and bounded
/// launchctl observations all match the plan exactly.
#[cfg(target_os = "macos")]
pub fn apply_disposable_launchd_service(
    plan: &DisposableLaunchdServicePlan,
    approved_plan_identity: &Sha256Digest,
    executor: &impl TimedCommandExecutor,
) -> Result<DisposableLaunchdServiceApplyReport, DisposableLaunchdServiceApplyError> {
    use rustix::process::{getegid, geteuid};

    if approved_plan_identity != plan.report.plan_identity() {
        return Err(apply_error(
            DisposableLaunchdServiceApplyErrorKind::ApprovalRequired,
            "disposable_launchd_service_approval_mismatch",
        ));
    }
    if geteuid().as_raw() != plan.operator_uid || geteuid().is_root() {
        return Err(apply_error(
            DisposableLaunchdServiceApplyErrorKind::UnsafeState,
            "disposable_launchd_service_operator_mismatch",
        ));
    }
    let operator_gid = getegid().as_raw();
    // Installation consumes the current executable and enrollment bytes. Removal consumes only
    // the exact approved plist plus the exact loaded launchd identity: those inputs may have been
    // deleted or rotated precisely because the service is being recovered or removed.
    if plan.report.desired_state == DisposableLaunchdServiceDesiredState::Installed {
        verify_plan_inputs(plan, operator_gid)?;
    }
    let directory = open_launch_agent_directory(plan, operator_gid)?;
    preflight_configuration(plan, &directory, operator_gid)?;
    let _apply_lock = acquire_apply_lock(plan, &directory, operator_gid)?;
    // A previous process may have completed the atomic rename but failed while reporting the
    // containing-directory fsync. Close that durability window before classifying the final name.
    rustix_fs::fsync(&directory).map_err(|_| unsafe_state())?;
    let existing = read_exact_plist(plan, &directory, operator_gid)?;
    let disposition = match plan.report.desired_state {
        DisposableLaunchdServiceDesiredState::Installed => {
            if !existing {
                publish_exact_plist(plan, &directory, operator_gid)?;
            }
            match observe_launchd_service(plan, executor)? {
                LaunchdServiceObservation::Exact => {
                    verify_plan_inputs(plan, operator_gid)?;
                    verify_exact_plist(plan, &directory, operator_gid)?;
                    DisposableLaunchdServiceApplyDisposition::Satisfied
                }
                LaunchdServiceObservation::Absent => {
                    verify_plan_inputs(plan, operator_gid)?;
                    verify_exact_plist(plan, &directory, operator_gid)?;
                    let command = launchctl(
                        executor,
                        &[
                            "bootstrap",
                            plan.report.launchd_domain(),
                            private_text(&plan.launch_agent)?,
                        ],
                    )?;
                    match observe_launchd_service(plan, executor)? {
                        LaunchdServiceObservation::Exact => {
                            verify_plan_inputs(plan, operator_gid)?;
                            verify_exact_plist(plan, &directory, operator_gid)?;
                            DisposableLaunchdServiceApplyDisposition::Installed
                        }
                        LaunchdServiceObservation::Absent if !command.success => {
                            return Err(apply_error(
                                DisposableLaunchdServiceApplyErrorKind::CommandFailed,
                                "disposable_launchd_service_bootstrap_failed",
                            ));
                        }
                        LaunchdServiceObservation::Absent => {
                            return Err(apply_error(
                                DisposableLaunchdServiceApplyErrorKind::CommandFailed,
                                "disposable_launchd_service_bootstrap_not_observed",
                            ));
                        }
                    }
                }
            }
        }
        DisposableLaunchdServiceDesiredState::Removed => {
            match (existing, observe_launchd_service(plan, executor)?) {
                (false, LaunchdServiceObservation::Absent) => {
                    DisposableLaunchdServiceApplyDisposition::Satisfied
                }
                (false, LaunchdServiceObservation::Exact) => {
                    return Err(apply_error(
                        DisposableLaunchdServiceApplyErrorKind::UnsafeState,
                        "disposable_launchd_service_loaded_without_configuration",
                    ));
                }
                (true, observation) => {
                    if observation == LaunchdServiceObservation::Exact {
                        verify_exact_plist(plan, &directory, operator_gid)?;
                        let command = launchctl(executor, &["bootout", &service_target(plan)])?;
                        match observe_launchd_service(plan, executor)? {
                            LaunchdServiceObservation::Absent => {}
                            LaunchdServiceObservation::Exact if !command.success => {
                                return Err(apply_error(
                                    DisposableLaunchdServiceApplyErrorKind::CommandFailed,
                                    "disposable_launchd_service_bootout_failed",
                                ));
                            }
                            LaunchdServiceObservation::Exact => {
                                return Err(apply_error(
                                    DisposableLaunchdServiceApplyErrorKind::CommandFailed,
                                    "disposable_launchd_service_bootout_not_observed",
                                ));
                            }
                        }
                    }
                    remove_exact_plist(plan, &directory, operator_gid)?;
                    DisposableLaunchdServiceApplyDisposition::Removed
                }
            }
        }
    };
    Ok(DisposableLaunchdServiceApplyReport {
        schema_version: DISPOSABLE_LAUNCHD_SERVICE_PLAN_SCHEMA_VERSION,
        desired_state: plan.report.desired_state,
        disposition,
        service_label: DISPOSABLE_LAUNCHD_SERVICE_LABEL,
        plan_identity: plan.report.plan_identity.clone(),
    })
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtectedInputKind {
    Program,
    Enrollment,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdServiceObservation {
    Absent,
    Exact,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct LaunchdApplyLock {
    lock: OwnedFd,
}

#[cfg(target_os = "macos")]
impl Drop for LaunchdApplyLock {
    fn drop(&mut self) {
        // A concurrent fork can briefly inherit this open-file description before exec. Explicit
        // unlock prevents that duplicate from extending the apply boundary after this guard ends.
        let _ = rustix_fs::flock(&self.lock, FlockOperation::Unlock);
    }
}

#[cfg(target_os = "macos")]
fn verify_plan_inputs(
    plan: &DisposableLaunchdServicePlan,
    operator_gid: u32,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    verify_protected_input(
        &plan.program,
        &plan.program_digest,
        plan.operator_uid,
        operator_gid,
        ProtectedInputKind::Program,
    )?;
    verify_protected_input(
        &plan.enrollment,
        &plan.enrollment_digest,
        plan.operator_uid,
        operator_gid,
        ProtectedInputKind::Enrollment,
    )
}

#[cfg(target_os = "macos")]
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[cfg(target_os = "macos")]
const INPUT_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

#[cfg(target_os = "macos")]
fn verify_protected_input(
    path: &Path,
    expected_digest: &Sha256Digest,
    operator_uid: u32,
    operator_gid: u32,
    kind: ProtectedInputKind,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    let parent_path = path.parent().ok_or_else(unsafe_state)?;
    let name = path.file_name().ok_or_else(unsafe_state)?;
    let parent = open_protected_directory_chain(
        parent_path,
        operator_uid,
        kind == ProtectedInputKind::Enrollment,
    )?;
    let parent_before = rustix_fs::fstat(&parent).map_err(|_| unsafe_state())?;
    let held =
        rustix_fs::openat(&parent, name, INPUT_FLAGS, Mode::empty()).map_err(|_| unsafe_state())?;
    let mut file = File::from(held);
    let before = rustix_fs::fstat(&file).map_err(|_| unsafe_state())?;
    inspect_input(&before, operator_uid, operator_gid, kind)?;
    let path_before =
        rustix_fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| unsafe_state())?;
    if !same_file(&before, &path_before) {
        return Err(unsafe_state());
    }
    if digest_file(&mut file, input_limit(kind))? != *expected_digest {
        return Err(apply_error(
            DisposableLaunchdServiceApplyErrorKind::UnsafeState,
            "disposable_launchd_service_input_digest_mismatch",
        ));
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| unsafe_state())?;
    if digest_file(&mut file, input_limit(kind))? != *expected_digest {
        return Err(unsafe_state());
    }
    let after = rustix_fs::fstat(&file).map_err(|_| unsafe_state())?;
    let path_after =
        rustix_fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| unsafe_state())?;
    let parent_after = rustix_fs::fstat(&parent).map_err(|_| unsafe_state())?;
    let resolved_parent = rustix_fs::stat(parent_path).map_err(|_| unsafe_state())?;
    if !same_file(&before, &after)
        || !same_file(&before, &path_after)
        || !same_directory(&parent_before, &parent_after)
        || !same_directory(&parent_before, &resolved_parent)
    {
        return Err(unsafe_state());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_launch_agent_directory(
    plan: &DisposableLaunchdServicePlan,
    operator_gid: u32,
) -> Result<OwnedFd, DisposableLaunchdServiceApplyError> {
    let path = plan.launch_agent.parent().ok_or_else(unsafe_state)?;
    let directory = open_protected_directory_chain(path, plan.operator_uid, true)?;
    let stat = rustix_fs::fstat(&directory).map_err(|_| unsafe_state())?;
    if stat.st_uid != plan.operator_uid || stat.st_gid != operator_gid || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_state());
    }
    let resolved = rustix_fs::stat(path).map_err(|_| unsafe_state())?;
    if !same_directory(&stat, &resolved) {
        return Err(unsafe_state());
    }
    Ok(directory)
}

#[cfg(target_os = "macos")]
fn acquire_apply_lock(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
) -> Result<LaunchdApplyLock, DisposableLaunchdServiceApplyError> {
    let flags =
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let lock = rustix_fs::openat(directory, APPLY_LOCK, flags, Mode::from_raw_mode(0o600))
        .map_err(|_| unsafe_state())?;
    let before = rustix_fs::fstat(&lock).map_err(|_| unsafe_state())?;
    let path_before = rustix_fs::statat(directory, APPLY_LOCK, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state())?;
    if !FileType::from_raw_mode(before.st_mode).is_file()
        || before.st_nlink != 1
        || before.st_uid != plan.operator_uid
        || before.st_gid != operator_gid
        || before.st_mode & 0o7777 != 0o600
        || before.st_size != 0
        || !same_file(&before, &path_before)
    {
        return Err(unsafe_state());
    }
    rustix_fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| {
        apply_error(
            DisposableLaunchdServiceApplyErrorKind::UnsafeState,
            "disposable_launchd_service_apply_busy",
        )
    })?;
    let guard = LaunchdApplyLock { lock };
    validate_acquired_apply_lock(plan, directory, operator_gid, &before, guard)
}

#[cfg(target_os = "macos")]
fn validate_acquired_apply_lock(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
    before: &rustix_fs::Stat,
    guard: LaunchdApplyLock,
) -> Result<LaunchdApplyLock, DisposableLaunchdServiceApplyError> {
    let after = rustix_fs::fstat(&guard.lock).map_err(|_| unsafe_state())?;
    let path_after = rustix_fs::statat(directory, APPLY_LOCK, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state())?;
    if !same_file(before, &after) || !same_file(before, &path_after) {
        return Err(unsafe_state());
    }
    verify_launch_agent_directory(plan, directory, operator_gid)?;
    Ok(guard)
}

#[cfg(target_os = "macos")]
fn open_protected_directory_chain(
    path: &Path,
    operator_uid: u32,
    allow_operator_owner: bool,
) -> Result<OwnedFd, DisposableLaunchdServiceApplyError> {
    use std::path::Component;

    let mut directory =
        rustix_fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| unsafe_state())?;
    inspect_directory(
        &rustix_fs::fstat(&directory).map_err(|_| unsafe_state())?,
        operator_uid,
        allow_operator_owner,
    )?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = rustix_fs::openat(&directory, name, DIRECTORY_FLAGS, Mode::empty())
                    .map_err(|_| unsafe_state())?;
                inspect_directory(
                    &rustix_fs::fstat(&directory).map_err(|_| unsafe_state())?,
                    operator_uid,
                    allow_operator_owner,
                )?;
            }
            _ => return Err(unsafe_state()),
        }
    }
    Ok(directory)
}

#[cfg(target_os = "macos")]
fn inspect_directory(
    stat: &rustix_fs::Stat,
    operator_uid: u32,
    allow_operator_owner: bool,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid != 0 && (!allow_operator_owner || stat.st_uid != operator_uid))
        || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_state());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn inspect_input(
    stat: &rustix_fs::Stat,
    operator_uid: u32,
    operator_gid: u32,
    kind: ProtectedInputKind,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_size <= 0
        || u64::try_from(stat.st_size)
            .ok()
            .is_none_or(|size| size > input_limit(kind))
    {
        return Err(unsafe_state());
    }
    match kind {
        ProtectedInputKind::Program
            if stat.st_uid != 0 || stat.st_mode & 0o022 != 0 || stat.st_mode & 0o111 == 0 =>
        {
            Err(unsafe_state())
        }
        ProtectedInputKind::Enrollment
            if stat.st_uid != operator_uid
                || stat.st_gid != operator_gid
                || stat.st_mode & 0o7777 != 0o600 =>
        {
            Err(unsafe_state())
        }
        _ => Ok(()),
    }
}

#[cfg(target_os = "macos")]
const fn input_limit(kind: ProtectedInputKind) -> u64 {
    match kind {
        ProtectedInputKind::Program => MAX_PROGRAM_BYTES,
        ProtectedInputKind::Enrollment => MAX_ENROLLMENT_BYTES,
    }
}

#[cfg(target_os = "macos")]
fn digest_file(
    file: &mut File,
    limit: u64,
) -> Result<Sha256Digest, DisposableLaunchdServiceApplyError> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 8_192];
    loop {
        let read = file.read(&mut buffer).map_err(|_| unsafe_state())?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| unsafe_state())?)
            .ok_or_else(unsafe_state)?;
        if total > limit {
            return Err(unsafe_state());
        }
        hasher.update(&buffer[..read]);
    }
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| unsafe_state())
}

#[cfg(target_os = "macos")]
fn read_exact_plist(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
) -> Result<bool, DisposableLaunchdServiceApplyError> {
    read_matching_plist(plan, directory, operator_gid, 1)
}

#[cfg(target_os = "macos")]
fn read_matching_plist(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
    expected_links: u16,
) -> Result<bool, DisposableLaunchdServiceApplyError> {
    let name = plan.launch_agent.file_name().ok_or_else(unsafe_state)?;
    let held = match rustix_fs::openat(directory, name, INPUT_FLAGS, Mode::empty()) {
        Ok(held) => held,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(_) => return Err(unsafe_state()),
    };
    let mut file = File::from(held);
    let before = rustix_fs::fstat(&file).map_err(|_| unsafe_state())?;
    inspect_plist(
        &before,
        plan.operator_uid,
        operator_gid,
        plan.plist.len(),
        expected_links,
    )?;
    let path_before = rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state())?;
    if !same_file(&before, &path_before) {
        return Err(unsafe_state());
    }
    let bytes = read_bounded(&mut file, MAX_PLIST_BYTES)?;
    file.seek(SeekFrom::Start(0)).map_err(|_| unsafe_state())?;
    let confirmation = read_bounded(&mut file, MAX_PLIST_BYTES)?;
    let after = rustix_fs::fstat(&file).map_err(|_| unsafe_state())?;
    let path_after = rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state())?;
    if bytes != plan.plist
        || confirmation != bytes
        || !same_file(&before, &after)
        || !same_file(&before, &path_after)
    {
        return Err(apply_error(
            DisposableLaunchdServiceApplyErrorKind::UnsafeState,
            "disposable_launchd_service_configuration_mismatch",
        ));
    }
    verify_launch_agent_directory(plan, directory, operator_gid)?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn read_bounded(
    file: &mut File,
    limit: u64,
) -> Result<Vec<u8>, DisposableLaunchdServiceApplyError> {
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_state())?;
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|size| size > limit)
    {
        return Err(unsafe_state());
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn inspect_plist(
    stat: &rustix_fs::Stat,
    operator_uid: u32,
    operator_gid: u32,
    expected_len: usize,
    expected_links: u16,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != expected_links
        || stat.st_uid != operator_uid
        || stat.st_gid != operator_gid
        || stat.st_mode & 0o7777 != 0o600
        || usize::try_from(stat.st_size).ok() != Some(expected_len)
    {
        return Err(unsafe_state());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn preflight_configuration(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    let _ = read_exact_plist(plan, directory, operator_gid)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn create_staged_plist(
    directory: &OwnedFd,
    flags: OFlags,
) -> Result<(String, OwnedFd), DisposableLaunchdServiceApplyError> {
    let mut random_source = File::open(SYSTEM_RANDOM_SOURCE).map_err(|_| unsafe_state())?;
    for _ in 0..4 {
        let mut random = [0_u8; 16];
        random_source
            .read_exact(&mut random)
            .map_err(|_| unsafe_state())?;
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let name = format!("{STAGED_PLIST_PREFIX}{suffix}");
        match rustix_fs::openat(directory, &name, flags, Mode::from_raw_mode(0o600)) {
            Ok(file) => return Ok((name, file)),
            Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(unsafe_state()),
        }
    }
    Err(unsafe_state())
}

#[cfg(target_os = "macos")]
fn publish_exact_plist(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    let final_name = plan.launch_agent.file_name().ok_or_else(unsafe_state)?;
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let (stage_name, held) = create_staged_plist(directory, flags)?;
    let mut file = File::from(held);
    let result = (|| {
        file.write_all(&plan.plist).map_err(|_| unsafe_state())?;
        file.sync_all().map_err(|_| unsafe_state())?;
        file.seek(SeekFrom::Start(0)).map_err(|_| unsafe_state())?;
        let bytes = read_bounded(&mut file, MAX_PLIST_BYTES)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| unsafe_state())?;
        if bytes != plan.plist || read_bounded(&mut file, MAX_PLIST_BYTES)? != bytes {
            return Err(unsafe_state());
        }
        let stat = rustix_fs::fstat(&file).map_err(|_| unsafe_state())?;
        if stat.st_uid != plan.operator_uid
            || stat.st_gid != operator_gid
            || stat.st_mode & 0o7777 != 0o600
            || usize::try_from(stat.st_size).ok() != Some(plan.plist.len())
        {
            return Err(unsafe_state());
        }
        let path = rustix_fs::statat(directory, &stage_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unsafe_state())?;
        if !same_file(&stat, &path) {
            return Err(unsafe_state());
        }
        rustix_fs::renameat_with(
            directory,
            &stage_name,
            directory,
            final_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|_| unsafe_state())?;
        rustix_fs::fsync(directory).map_err(|_| unsafe_state())?;
        Ok(())
    })();
    if result.is_err()
        && rustix_fs::fstat(&file)
            .ok()
            .zip(rustix_fs::statat(directory, &stage_name, AtFlags::SYMLINK_NOFOLLOW).ok())
            .is_some_and(|(held, path)| same_file(&held, &path))
    {
        let _ = rustix_fs::unlinkat(directory, &stage_name, AtFlags::empty());
        let _ = rustix_fs::fsync(directory);
    }
    result?;
    verify_exact_plist(plan, directory, operator_gid)
}

#[cfg(target_os = "macos")]
fn verify_exact_plist(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    if !read_exact_plist(plan, directory, operator_gid)? {
        return Err(unsafe_state());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_exact_plist(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    verify_exact_plist(plan, directory, operator_gid)?;
    let name = plan.launch_agent.file_name().ok_or_else(unsafe_state)?;
    rustix_fs::unlinkat(directory, name, AtFlags::empty()).map_err(|_| unsafe_state())?;
    rustix_fs::fsync(directory).map_err(|_| unsafe_state())?;
    match rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        _ => Err(unsafe_state()),
    }
}

#[cfg(target_os = "macos")]
fn verify_launch_agent_directory(
    plan: &DisposableLaunchdServicePlan,
    directory: &OwnedFd,
    operator_gid: u32,
) -> Result<(), DisposableLaunchdServiceApplyError> {
    let held = rustix_fs::fstat(directory).map_err(|_| unsafe_state())?;
    if held.st_uid != plan.operator_uid || held.st_gid != operator_gid || held.st_mode & 0o022 != 0
    {
        return Err(unsafe_state());
    }
    let path = plan.launch_agent.parent().ok_or_else(unsafe_state)?;
    let resolved = rustix_fs::stat(path).map_err(|_| unsafe_state())?;
    if !same_directory(&held, &resolved) {
        return Err(unsafe_state());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn observe_launchd_service(
    plan: &DisposableLaunchdServicePlan,
    executor: &impl TimedCommandExecutor,
) -> Result<LaunchdServiceObservation, DisposableLaunchdServiceApplyError> {
    let target = service_target(plan);
    let record = launchctl(executor, &["print", &target])?;
    if record.success {
        let expected_header = format!("{target} = {{\n");
        let expected_path = format!("\n\tpath = {}\n", private_text(&plan.launch_agent)?);
        let expected_program = format!("\n\tprogram = {}\n", private_text(&plan.program)?);
        let expected_arguments = format!(
            "\n\targuments = {{\n\t\t{}\n\t\tworker\n\t\tserve\n\t\t--program-digest\n\t\t{}\n\t\t--enrollment\n\t\t{}\n\t\t--enrollment-digest\n\t\t{}\n\t}}\n",
            private_text(&plan.program)?,
            plan.program_digest.as_str(),
            private_text(&plan.enrollment)?,
            plan.enrollment_digest.as_str(),
        );
        if record.stdout.starts_with(&expected_header)
            && record.stdout.contains(&expected_path)
            && record.stdout.contains("\n\ttype = LaunchAgent\n")
            && record.stdout.contains(&expected_program)
            && record.stdout.contains(&expected_arguments)
        {
            return Ok(LaunchdServiceObservation::Exact);
        }
        return Err(apply_error(
            DisposableLaunchdServiceApplyErrorKind::UnsafeState,
            "disposable_launchd_service_identity_mismatch",
        ));
    }
    if record.status == Some(113) {
        return Ok(LaunchdServiceObservation::Absent);
    }
    Err(apply_error(
        DisposableLaunchdServiceApplyErrorKind::CommandFailed,
        "disposable_launchd_service_observation_failed",
    ))
}

#[cfg(target_os = "macos")]
fn launchctl(
    executor: &impl TimedCommandExecutor,
    arguments: &[&str],
) -> Result<ExecutionRecord, DisposableLaunchdServiceApplyError> {
    let spec = arguments
        .iter()
        .fold(CommandSpec::new(LAUNCHCTL_PROGRAM), |spec, argument| {
            spec.argument(*argument)
        });
    executor
        .execute_with_timeout(&spec, LAUNCHCTL_TIMEOUT)
        .map_err(|_| {
            apply_error(
                DisposableLaunchdServiceApplyErrorKind::CommandFailed,
                "disposable_launchd_service_command_failed",
            )
        })
}

#[cfg(target_os = "macos")]
fn service_target(plan: &DisposableLaunchdServicePlan) -> String {
    format!(
        "{}/{}",
        plan.report.launchd_domain(),
        DISPOSABLE_LAUNCHD_SERVICE_LABEL
    )
}

#[cfg(target_os = "macos")]
fn private_text(path: &Path) -> Result<&str, DisposableLaunchdServiceApplyError> {
    path.to_str().ok_or_else(unsafe_state)
}

#[cfg(target_os = "macos")]
fn same_file(left: &rustix_fs::Stat, right: &rustix_fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

#[cfg(target_os = "macos")]
fn same_directory(left: &rustix_fs::Stat, right: &rustix_fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
}

const fn unsafe_state() -> DisposableLaunchdServiceApplyError {
    apply_error(
        DisposableLaunchdServiceApplyErrorKind::UnsafeState,
        "disposable_launchd_service_unsafe_state",
    )
}

const fn apply_error(
    kind: DisposableLaunchdServiceApplyErrorKind,
    code: &'static str,
) -> DisposableLaunchdServiceApplyError {
    DisposableLaunchdServiceApplyError { kind, code }
}

/// Build one non-mutating exact LaunchAgent installation or removal plan.
///
/// Apply refuses an existing nonmatching property list, publishes atomically with mode `0600`,
/// treats the exact durable property list as its pre-command checkpoint, and retries by observing
/// the exact loaded job. Removal boots out the exact loaded path/program/argv before deleting an
/// exact-byte-matching property list.
///
/// # Errors
///
/// Returns a path-free error unless every private path is an explicit normalized absolute path and
/// the operator UID is a positive non-root user identity.
pub fn plan_disposable_launchd_service(
    desired_state: DisposableLaunchdServiceDesiredState,
    operator_uid: u32,
    operator_home: &Path,
    program: &Path,
    program_digest: &Sha256Digest,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
) -> Result<DisposableLaunchdServicePlan, DisposableLaunchdServicePlanError> {
    if operator_uid == 0
        || operator_uid > MAX_UID
        || !valid_private_path(operator_home)
        || !valid_private_path(program)
        || !valid_private_path(enrollment)
    {
        return Err(invalid_configuration());
    }
    let launch_agent = operator_home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{DISPOSABLE_LAUNCHD_SERVICE_LABEL}.plist"));
    if !valid_private_path(&launch_agent) {
        return Err(invalid_configuration());
    }
    let launch_agent_directory = launch_agent.parent().ok_or_else(invalid_configuration)?;
    for input in [program, enrollment] {
        if input == launch_agent.as_path()
            || input == launch_agent_directory.join(APPLY_LOCK)
            || (input.parent() == Some(launch_agent_directory)
                && input
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(STAGED_PLIST_PREFIX)))
        {
            return Err(invalid_configuration());
        }
    }
    let plist = canonical_plist(program, program_digest, enrollment, enrollment_digest)?;
    let plan_identity = plan_identity(
        desired_state,
        operator_uid,
        &launch_agent,
        &plist,
        program_digest,
        enrollment_digest,
    )?;
    let actions = match desired_state {
        DisposableLaunchdServiceDesiredState::Installed => vec![
            DisposableLaunchdServiceActionReport {
                sequence: 1,
                kind: DisposableLaunchdServiceActionKind::PublishConfiguration,
                summary: "atomically publish the exact private LaunchAgent property list",
                rollback: RollbackClass::Reversible,
            },
            DisposableLaunchdServiceActionReport {
                sequence: 2,
                kind: DisposableLaunchdServiceActionKind::BootstrapService,
                summary: "bootstrap the exact user LaunchAgent in its GUI domain",
                rollback: RollbackClass::Compensating,
            },
        ],
        DisposableLaunchdServiceDesiredState::Removed => vec![
            DisposableLaunchdServiceActionReport {
                sequence: 1,
                kind: DisposableLaunchdServiceActionKind::BootoutService,
                summary: "boot out the exact user LaunchAgent and confirm it is no longer owned",
                rollback: RollbackClass::Compensating,
            },
            DisposableLaunchdServiceActionReport {
                sequence: 2,
                kind: DisposableLaunchdServiceActionKind::RemoveConfiguration,
                summary: "remove only the exact matching private LaunchAgent property list",
                rollback: RollbackClass::Compensating,
            },
        ],
    };
    Ok(DisposableLaunchdServicePlan {
        report: DisposableLaunchdServicePlanReport {
            schema_version: DISPOSABLE_LAUNCHD_SERVICE_PLAN_SCHEMA_VERSION,
            desired_state,
            service_label: DISPOSABLE_LAUNCHD_SERVICE_LABEL,
            service_scope: "user_launch_agent",
            launchd_domain: format!("gui/{operator_uid}"),
            plan_identity,
            configuration_mode: 0o600,
            preconditions: vec![
                "explicit operator approval names the exact plan identity",
                "the current user and GUI domain match the planned operator identity",
                "the executable is root-owned and immutable and enrollment is exact and private",
                "a foreign or nonmatching LaunchAgent configuration blocks the entire operation",
                "every completed action is durably checkpointed before the next mutation",
            ],
            actions,
            requires_operator_approval: true,
        },
        operator_uid,
        program: program.to_path_buf(),
        program_digest: program_digest.clone(),
        enrollment: enrollment.to_path_buf(),
        enrollment_digest: enrollment_digest.clone(),
        launch_agent,
        plist,
    })
}

fn plan_identity(
    desired_state: DisposableLaunchdServiceDesiredState,
    operator_uid: u32,
    launch_agent: &Path,
    plist: &[u8],
    program_digest: &Sha256Digest,
    enrollment_digest: &Sha256Digest,
) -> Result<Sha256Digest, DisposableLaunchdServicePlanError> {
    let target = launch_agent
        .to_str()
        .ok_or_else(invalid_configuration)?
        .as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(b"smolrunner.disposable-launchd-service-plan.v1\0");
    hasher.update([match desired_state {
        DisposableLaunchdServiceDesiredState::Installed => 1,
        DisposableLaunchdServiceDesiredState::Removed => 2,
    }]);
    hasher.update(operator_uid.to_be_bytes());
    hasher.update((target.len() as u64).to_be_bytes());
    hasher.update(target);
    hasher.update((plist.len() as u64).to_be_bytes());
    hasher.update(plist);
    hasher.update((program_digest.as_str().len() as u64).to_be_bytes());
    hasher.update(program_digest.as_str().as_bytes());
    hasher.update((enrollment_digest.as_str().len() as u64).to_be_bytes());
    hasher.update(enrollment_digest.as_str().as_bytes());
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| invalid_configuration())
}

fn canonical_plist(
    program: &Path,
    program_digest: &Sha256Digest,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
) -> Result<Vec<u8>, DisposableLaunchdServicePlanError> {
    let program = program.to_str().ok_or_else(invalid_configuration)?;
    let enrollment = enrollment.to_str().ok_or_else(invalid_configuration)?;
    let program = xml_text(program);
    let program_digest = xml_text(program_digest.as_str());
    let enrollment = xml_text(enrollment);
    let enrollment_digest = xml_text(enrollment_digest.as_str());
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
  <key>Label</key>\n\
  <string>{DISPOSABLE_LAUNCHD_SERVICE_LABEL}</string>\n\
  <key>ProgramArguments</key>\n\
  <array>\n\
    <string>{program}</string>\n\
    <string>worker</string>\n\
    <string>serve</string>\n\
    <string>--program-digest</string>\n\
    <string>{program_digest}</string>\n\
    <string>--enrollment</string>\n\
    <string>{enrollment}</string>\n\
    <string>--enrollment-digest</string>\n\
    <string>{enrollment_digest}</string>\n\
  </array>\n\
  <key>RunAtLoad</key>\n\
  <true/>\n\
  <key>KeepAlive</key>\n\
  <true/>\n\
  <key>ProcessType</key>\n\
  <string>Background</string>\n\
  <key>ThrottleInterval</key>\n\
  <integer>10</integer>\n\
  <key>ExitTimeOut</key>\n\
  <integer>{SERVICE_EXIT_TIMEOUT_SECONDS}</integer>\n\
  <key>Umask</key>\n\
  <integer>63</integer>\n\
  <key>WorkingDirectory</key>\n\
  <string>/var/empty</string>\n\
  <key>StandardOutPath</key>\n\
  <string>/dev/null</string>\n\
  <key>StandardErrorPath</key>\n\
  <string>/dev/null</string>\n\
</dict>\n\
</plist>\n"
    )
    .into_bytes())
}

fn valid_private_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if text.len() > MAX_PRIVATE_PATH_BYTES
        || text == "/"
        || !text.starts_with('/')
        || text.ends_with('/')
        || text.chars().any(char::is_control)
    {
        return false;
    }
    text[1..]
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

const fn invalid_configuration() -> DisposableLaunchdServicePlanError {
    DisposableLaunchdServicePlanError {
        kind: DisposableLaunchdServicePlanErrorKind::InvalidConfiguration,
        code: "disposable_launchd_service_configuration_invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    use std::cell::{Cell, RefCell};
    #[cfg(target_os = "macos")]
    use std::io;
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::PermissionsExt as _;
    #[cfg(target_os = "macos")]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(target_os = "macos")]
    use crate::process::CommandExecutor;
    #[cfg(target_os = "macos")]
    use rustix::io::dup;

    fn install_plan() -> DisposableLaunchdServicePlan {
        plan_disposable_launchd_service(
            DisposableLaunchdServiceDesiredState::Installed,
            501,
            Path::new("/Users/operator"),
            Path::new("/opt/smolrunner/bin/smolrunner"),
            &digest('a'),
            Path::new("/Users/operator/.config/smolrunner/enrollment.json"),
            &digest('b'),
        )
        .unwrap()
    }

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn install_plan_binds_exact_private_inputs_and_path_free_report() {
        let plan = install_plan();
        let launch_agent =
            Path::new("/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist");
        let plist = canonical_plist(
            Path::new("/opt/smolrunner/bin/smolrunner"),
            &digest('a'),
            Path::new("/Users/operator/.config/smolrunner/enrollment.json"),
            &digest('b'),
        )
        .unwrap();
        assert_eq!(
            launch_agent,
            Path::new("/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist")
        );
        assert_eq!(
            plan.report().plan_identity(),
            &plan_identity(
                DisposableLaunchdServiceDesiredState::Installed,
                501,
                launch_agent,
                &plist,
                &digest('a'),
                &digest('b'),
            )
            .unwrap()
        );
        let plist = std::str::from_utf8(&plist).unwrap();
        assert!(plist.contains("<string>/opt/smolrunner/bin/smolrunner</string>"));
        assert!(plist.contains(digest('a').as_str()));
        assert!(plist.contains("<string>--enrollment</string>"));
        assert!(plist.contains("<string>--enrollment-digest</string>"));
        assert!(plist.contains(digest('b').as_str()));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>ExitTimeOut</key>"));
        assert!(plist.contains("<integer>90</integer>"));
        assert!(!plist.contains("EnvironmentVariables"));

        let report = serde_json::to_string(plan.report()).unwrap();
        assert!(!report.contains("/Users/"));
        assert!(!report.contains("/opt/"));
        assert!(report.contains("requires_operator_approval"));
        assert_eq!(plan.report().launchd_domain(), "gui/501");
        assert_eq!(plan.report().preconditions().len(), 5);
        assert_eq!(plan.report().actions().len(), 2);

        let different_program = canonical_plist(
            Path::new("/opt/smolrunner/bin/smolrunner"),
            &digest('f'),
            Path::new("/Users/operator/.config/smolrunner/enrollment.json"),
            &digest('b'),
        )
        .unwrap();
        assert_ne!(different_program, plan.plist);
    }

    #[test]
    fn removal_is_ordered_bootout_then_exact_configuration_removal() {
        let plan = plan_disposable_launchd_service(
            DisposableLaunchdServiceDesiredState::Removed,
            501,
            Path::new("/Users/operator"),
            Path::new("/opt/smolrunner/bin/smolrunner"),
            &digest('a'),
            Path::new("/Users/operator/.config/smolrunner/enrollment.json"),
            &digest('b'),
        )
        .unwrap();
        assert_eq!(
            plan.report().desired_state(),
            DisposableLaunchdServiceDesiredState::Removed
        );
        assert_eq!(
            plan.report().actions[0].kind,
            DisposableLaunchdServiceActionKind::BootoutService
        );
        assert_eq!(
            plan.report().actions[1].kind,
            DisposableLaunchdServiceActionKind::RemoveConfiguration
        );
        assert_ne!(
            plan.report().plan_identity(),
            install_plan().report().plan_identity()
        );
    }

    #[test]
    fn aliases_controls_root_and_root_uid_are_refused() {
        for path in [
            "/Users/operator/../other",
            "/Users/operator/./config",
            "/Users/operator//config",
            "/Users/operator/config/",
            "/Users/operator/\nconfig",
            "/",
        ] {
            assert!(
                plan_disposable_launchd_service(
                    DisposableLaunchdServiceDesiredState::Installed,
                    501,
                    Path::new("/Users/operator"),
                    Path::new("/opt/smolrunner/bin/smolrunner"),
                    &digest('a'),
                    Path::new(path),
                    &digest('b'),
                )
                .is_err(),
                "accepted {path:?}"
            );
        }
        assert!(
            plan_disposable_launchd_service(
                DisposableLaunchdServiceDesiredState::Installed,
                0,
                Path::new("/Users/operator"),
                Path::new("/opt/smolrunner/bin/smolrunner"),
                &digest('a'),
                Path::new("/Users/operator/enrollment.json"),
                &digest('b'),
            )
            .is_err()
        );
        for reserved in [
            "/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist",
            "/Users/operator/Library/LaunchAgents/.io.smolrunner.disposable-worker.apply.lock",
            "/Users/operator/Library/LaunchAgents/.io.smolrunner.disposable-worker.plist.next.0123456789abcdef",
        ] {
            assert!(
                plan_disposable_launchd_service(
                    DisposableLaunchdServiceDesiredState::Installed,
                    501,
                    Path::new("/Users/operator"),
                    Path::new("/opt/smolrunner/bin/smolrunner"),
                    &digest('a'),
                    Path::new(reserved),
                    &digest('b'),
                )
                .is_err(),
                "accepted reserved apply path {reserved:?}"
            );
        }
    }

    #[test]
    fn plist_escapes_private_path_metacharacters_without_changing_argv_shape() {
        let plist = canonical_plist(
            Path::new("/opt/smolrunner/bin/smol&runner"),
            &digest('a'),
            Path::new("/Users/operator/config<one>.json"),
            &digest('b'),
        )
        .unwrap();
        let plist = std::str::from_utf8(&plist).unwrap();
        assert!(plist.contains("smol&amp;runner"));
        assert!(plist.contains("config&lt;one&gt;.json"));
    }

    #[cfg(target_os = "macos")]
    struct ApplyFixture {
        root: PathBuf,
        program: PathBuf,
        enrollment: PathBuf,
        program_digest: Sha256Digest,
        enrollment_digest: Sha256Digest,
    }

    #[cfg(target_os = "macos")]
    impl ApplyFixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let root = std::env::temp_dir().join(format!(
                "smolrunner-launchd-apply-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let root = root.canonicalize().unwrap();
            for directory in [
                root.join("config"),
                root.join("Library"),
                root.join("Library/LaunchAgents"),
            ] {
                std::fs::create_dir(&directory).unwrap();
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            let program = PathBuf::from("/usr/bin/true");
            let enrollment = root.join("config/enrollment.json");
            let program_bytes = std::fs::read(&program).unwrap();
            let enrollment_bytes = b"{\"schema_version\":1}\n";
            std::fs::write(&enrollment, enrollment_bytes).unwrap();
            std::fs::set_permissions(&enrollment, std::fs::Permissions::from_mode(0o600)).unwrap();
            Self {
                root,
                program,
                enrollment,
                program_digest: digest_bytes(&program_bytes),
                enrollment_digest: digest_bytes(enrollment_bytes),
            }
        }

        fn plan(
            &self,
            desired: DisposableLaunchdServiceDesiredState,
        ) -> DisposableLaunchdServicePlan {
            plan_disposable_launchd_service(
                desired,
                rustix::process::geteuid().as_raw(),
                &self.root,
                &self.program,
                &self.program_digest,
                &self.enrollment,
                &self.enrollment_digest,
            )
            .unwrap()
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for ApplyFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(target_os = "macos")]
    fn digest_bytes(bytes: &[u8]) -> Sha256Digest {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).unwrap()
    }

    #[cfg(target_os = "macos")]
    struct FakeLaunchctl {
        loaded: Cell<bool>,
        path: PathBuf,
        program: PathBuf,
        program_digest: String,
        enrollment: PathBuf,
        enrollment_digest: String,
        calls: RefCell<Vec<Vec<String>>>,
    }

    #[cfg(target_os = "macos")]
    impl FakeLaunchctl {
        fn new(plan: &DisposableLaunchdServicePlan) -> Self {
            Self {
                loaded: Cell::new(false),
                path: plan.launch_agent.clone(),
                program: plan.program.clone(),
                program_digest: plan.program_digest.as_str().to_owned(),
                enrollment: plan.enrollment.clone(),
                enrollment_digest: plan.enrollment_digest.as_str().to_owned(),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn execute_inner(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            let argv = spec.displayed_argv();
            self.calls.borrow_mut().push(argv.clone());
            let operation = argv.get(1).map(String::as_str).unwrap_or_default();
            match operation {
                "print" if self.loaded.get() => Ok(ExecutionRecord {
                    argv,
                    environment_keys: Vec::new(),
                    status: Some(0),
                    success: true,
                    stdout: format!(
                        "gui/{}/{} = {{\n\tpath = {}\n\ttype = LaunchAgent\n\tprogram = {}\n\targuments = {{\n\t\t{}\n\t\tworker\n\t\tserve\n\t\t--program-digest\n\t\t{}\n\t\t--enrollment\n\t\t{}\n\t\t--enrollment-digest\n\t\t{}\n\t}}\n}}\n",
                        rustix::process::geteuid().as_raw(),
                        DISPOSABLE_LAUNCHD_SERVICE_LABEL,
                        self.path.display(),
                        self.program.display(),
                        self.program.display(),
                        self.program_digest,
                        self.enrollment.display(),
                        self.enrollment_digest,
                    ),
                    stderr: String::new(),
                }),
                "print" => Ok(ExecutionRecord {
                    argv,
                    environment_keys: Vec::new(),
                    status: Some(113),
                    success: false,
                    stdout: String::new(),
                    stderr: "not found".to_owned(),
                }),
                "bootstrap" => {
                    self.loaded.set(true);
                    Ok(success(argv))
                }
                "bootout" => {
                    self.loaded.set(false);
                    Ok(success(argv))
                }
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unexpected fake launchctl operation",
                )),
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl CommandExecutor for FakeLaunchctl {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.execute_inner(spec)
        }
    }

    #[cfg(target_os = "macos")]
    impl TimedCommandExecutor for FakeLaunchctl {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            _timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            self.execute_inner(spec)
        }
    }

    #[cfg(target_os = "macos")]
    fn success(argv: Vec<String>) -> ExecutionRecord {
        ExecutionRecord {
            argv,
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exact_approved_apply_is_idempotent_and_removal_boots_out_before_unlink() {
        let fixture = ApplyFixture::new();
        let install = fixture.plan(DisposableLaunchdServiceDesiredState::Installed);
        let executor = FakeLaunchctl::new(&install);
        let approved = install.report().plan_identity().clone();
        let installed = apply_disposable_launchd_service(&install, &approved, &executor).unwrap();
        assert_eq!(
            installed.disposition(),
            DisposableLaunchdServiceApplyDisposition::Installed
        );
        let plist = fixture
            .root
            .join("Library/LaunchAgents/io.smolrunner.disposable-worker.plist");
        assert_eq!(std::fs::read(&plist).unwrap(), install.plist);
        assert_eq!(
            std::fs::metadata(&plist).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_eq!(
            apply_disposable_launchd_service(&install, &approved, &executor)
                .unwrap()
                .disposition(),
            DisposableLaunchdServiceApplyDisposition::Satisfied
        );
        assert_eq!(
            executor
                .calls
                .borrow()
                .iter()
                .filter(|argv| argv.get(1).is_some_and(|value| value == "bootstrap"))
                .count(),
            1
        );

        let removal = fixture.plan(DisposableLaunchdServiceDesiredState::Removed);
        std::fs::remove_file(&fixture.enrollment).unwrap();
        let removed =
            apply_disposable_launchd_service(&removal, removal.report().plan_identity(), &executor)
                .unwrap();
        assert_eq!(
            removed.disposition(),
            DisposableLaunchdServiceApplyDisposition::Removed
        );
        assert!(!plist.exists());
        assert_eq!(
            apply_disposable_launchd_service(
                &removal,
                removal.report().plan_identity(),
                &executor,
            )
            .unwrap()
            .disposition(),
            DisposableLaunchdServiceApplyDisposition::Satisfied
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn approval_and_foreign_configuration_fail_before_launchctl() {
        let fixture = ApplyFixture::new();
        let plan = fixture.plan(DisposableLaunchdServiceDesiredState::Installed);
        let executor = FakeLaunchctl::new(&plan);
        let mismatch =
            apply_disposable_launchd_service(&plan, &digest('f'), &executor).unwrap_err();
        assert_eq!(
            mismatch.kind(),
            DisposableLaunchdServiceApplyErrorKind::ApprovalRequired
        );
        assert!(executor.calls.borrow().is_empty());

        let plist = fixture
            .root
            .join("Library/LaunchAgents/io.smolrunner.disposable-worker.plist");
        std::fs::write(&plist, b"foreign\n").unwrap();
        std::fs::set_permissions(&plist, std::fs::Permissions::from_mode(0o600)).unwrap();
        let foreign =
            apply_disposable_launchd_service(&plan, plan.report().plan_identity(), &executor)
                .unwrap_err();
        assert_eq!(
            foreign.kind(),
            DisposableLaunchdServiceApplyErrorKind::UnsafeState
        );
        assert!(executor.calls.borrow().is_empty());
        assert_eq!(std::fs::read(&plist).unwrap(), b"foreign\n");
        assert!(
            !fixture
                .root
                .join("Library/LaunchAgents")
                .join(APPLY_LOCK)
                .exists()
        );

        std::fs::remove_file(plist).unwrap();
        let mutable_program = fixture.root.join("mutable-program");
        std::fs::write(&mutable_program, b"operator-owned").unwrap();
        std::fs::set_permissions(&mutable_program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mutable_plan = plan_disposable_launchd_service(
            DisposableLaunchdServiceDesiredState::Installed,
            rustix::process::geteuid().as_raw(),
            &fixture.root,
            &mutable_program,
            &digest_bytes(b"operator-owned"),
            &fixture.enrollment,
            &fixture.enrollment_digest,
        )
        .unwrap();
        let mutable_executor = FakeLaunchctl::new(&mutable_plan);
        assert_eq!(
            apply_disposable_launchd_service(
                &mutable_plan,
                mutable_plan.report().plan_identity(),
                &mutable_executor,
            )
            .unwrap_err()
            .kind(),
            DisposableLaunchdServiceApplyErrorKind::UnsafeState
        );
        assert!(mutable_executor.calls.borrow().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn foreign_stage_is_preserved_and_apply_lock_explicitly_unlocks() {
        let fixture = ApplyFixture::new();
        let plan = fixture.plan(DisposableLaunchdServiceDesiredState::Installed);
        let executor = FakeLaunchctl::new(&plan);
        let launch_agents = fixture.root.join("Library/LaunchAgents");
        let stage = launch_agents
            .join(".io.smolrunner.disposable-worker.plist.next.ffffffffffffffffffffffffffffffff");
        std::fs::write(&stage, b"partial").unwrap();
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            apply_disposable_launchd_service(&plan, plan.report().plan_identity(), &executor,)
                .unwrap()
                .disposition(),
            DisposableLaunchdServiceApplyDisposition::Installed
        );
        assert_eq!(std::fs::read(&stage).unwrap(), b"partial");

        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(launch_agents.join(APPLY_LOCK))
            .unwrap();
        rustix_fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
        let busy =
            apply_disposable_launchd_service(&plan, plan.report().plan_identity(), &executor)
                .unwrap_err();
        assert_eq!(busy.code(), "disposable_launchd_service_apply_busy");
        rustix_fs::flock(&lock, FlockOperation::Unlock).unwrap();

        let directory = open_launch_agent_directory(&plan, rustix::process::getegid().as_raw())
            .expect("open launch agent directory");
        let guard = acquire_apply_lock(&plan, &directory, rustix::process::getegid().as_raw())
            .expect("acquire apply guard");
        let inherited = dup(&guard.lock).expect("duplicate inherited lock descriptor");
        drop(guard);
        let reacquired = acquire_apply_lock(&plan, &directory, rustix::process::getegid().as_raw())
            .expect("explicit unlock must release inherited open-file description");
        drop(reacquired);
        drop(inherited);

        let guard = acquire_apply_lock(&plan, &directory, rustix::process::getegid().as_raw())
            .expect("acquire guard for error path");
        let before = rustix_fs::fstat(&guard.lock).unwrap();
        let inherited = dup(&guard.lock).expect("duplicate error-path lock descriptor");
        rustix_fs::fchmod(&guard.lock, Mode::from_raw_mode(0o640)).unwrap();
        assert!(
            validate_acquired_apply_lock(
                &plan,
                &directory,
                rustix::process::getegid().as_raw(),
                &before,
                guard,
            )
            .is_err()
        );
        rustix_fs::fchmod(&inherited, Mode::from_raw_mode(0o600)).unwrap();
        let reacquired = acquire_apply_lock(&plan, &directory, rustix::process::getegid().as_raw())
            .expect("error-path drop must explicitly unlock inherited description");
        drop(reacquired);
        drop(inherited);
    }
}
