use std::fmt;
use std::fs::File;
use std::io::Read as _;
use std::os::fd::OwnedFd;

use rustix::fs::{self as rustix_fs, AtFlags, Mode, OFlags, StatxFlags, statx};
use rustix::io::Errno;
use rustix::mount::{
    FsMountFlags, FsOpenFlags, MountAttrFlags, MoveMountFlags, fsconfig_create, fsconfig_set_fd,
    fsmount, fsopen, move_mount,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::trusted_overlay_mount_plan::{
    TrustedOverlayMountDescriptorLease, TrustedOverlayMountExecutionDescriptors,
    TrustedOverlayMountOptionPolicy, TrustedOverlayMountPlan,
};
use crate::trusted_overlay_task_view::{
    OverlaySourceAnchorGeneration, OverlaySourceAnchorId, OverlaySourceAnchorRecord,
    OverlayTaskViewLease, OverlayTaskViewRecord,
};
use crate::trusted_project_filesystem_correlation::{
    TrustedProjectFilesystemCorrelationGeneration, TrustedProjectFilesystemCorrelationProof,
};

pub const TRUSTED_OVERLAY_MOUNT_EXECUTION_SCHEMA_VERSION: u8 = 1;
const MOUNT_ID_DOMAIN: &[u8] = b"smolrunner-trusted-overlay-mount-observation-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const OVERLAY_FILESYSTEM: &str = "overlay";
const MAX_PROC_MOUNTINFO_BYTES: usize = 1_048_576;
const LOWER_KEY: &str = "lowerdir+";
const UPPER_KEY: &str = "upperdir";
const WORK_KEY: &str = "workdir";
const VISIBLE_DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedOverlayMountExecutionReceipt {
    schema_version: u8,
    task_lease: OverlayTaskViewLease,
    source_anchor_id: OverlaySourceAnchorId,
    source_anchor_generation: OverlaySourceAnchorGeneration,
    correlation_generation: TrustedProjectFilesystemCorrelationGeneration,
    option_policy: TrustedOverlayMountOptionPolicy,
    mount_identity: Sha256Digest,
}

impl TrustedOverlayMountExecutionReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn task_lease(&self) -> &OverlayTaskViewLease {
        &self.task_lease
    }

    #[must_use]
    pub const fn source_anchor_id(&self) -> &OverlaySourceAnchorId {
        &self.source_anchor_id
    }

    #[must_use]
    pub const fn source_anchor_generation(&self) -> OverlaySourceAnchorGeneration {
        self.source_anchor_generation
    }

    #[must_use]
    pub const fn correlation_generation(&self) -> TrustedProjectFilesystemCorrelationGeneration {
        self.correlation_generation
    }

    #[must_use]
    pub const fn option_policy(&self) -> TrustedOverlayMountOptionPolicy {
        self.option_policy
    }

    #[must_use]
    pub const fn mount_identity(&self) -> &Sha256Digest {
        &self.mount_identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedOverlayMountExecutionErrorKind {
    Authority,
    Correlation,
    KernelUnavailable,
    PermissionDenied,
    MountConfiguration,
    Observation,
    WorkdirResetRequired,
    RecoveryRequired,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct TrustedOverlayMountExecutionError {
    kind: TrustedOverlayMountExecutionErrorKind,
    code: &'static str,
    message: &'static str,
    recovery_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mount_identity: Option<Sha256Digest>,
}

impl TrustedOverlayMountExecutionError {
    #[must_use]
    pub const fn kind(&self) -> TrustedOverlayMountExecutionErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn recovery_required(&self) -> bool {
        self.recovery_required
    }

    #[must_use]
    pub const fn mount_identity(&self) -> Option<&Sha256Digest> {
        self.mount_identity.as_ref()
    }
}

impl fmt::Debug for TrustedOverlayMountExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedOverlayMountExecutionError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .field("recovery_required", &self.recovery_required)
            .field("mount_identity", &self.mount_identity)
            .finish()
    }
}

impl fmt::Display for TrustedOverlayMountExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedOverlayMountExecutionError {}

/// Attach one sealed trusted OverlayFS view using only retained directory descriptors.
///
/// This function is physically privileged on Linux, but normal product composition cannot call it
/// successfully until #565 can mint a `TrustedProjectFilesystemCorrelationProof`. P1 intentionally
/// provides no production proof constructor.
///
/// The second plan/descriptor/correlation confirmation occurs after the filesystem context has been
/// configured with retained FDs and immediately before `FSCONFIG_CMD_CREATE`, the first operation in
/// this transaction that may cause OverlayFS to initialize its work state.
///
/// # Errors
///
/// Returns bounded path-private errors for authority/correlation drift, unavailable/denied kernel
/// mount operations, configuration failure, or post-attach observation ambiguity. An attach-attempt or later
/// failure carries `recovery_required = true` and an opaque mount identity when one was available.
pub fn execute_trusted_overlay_mount(
    plan: &TrustedOverlayMountPlan,
    descriptor_lease: &TrustedOverlayMountDescriptorLease,
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    correlation: TrustedProjectFilesystemCorrelationProof,
) -> Result<TrustedOverlayMountExecutionReceipt, TrustedOverlayMountExecutionError> {
    execute_with_backend(
        plan,
        descriptor_lease,
        source_anchor,
        task_view,
        &correlation,
        &mut RustixOverlayMountBackend,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleMountObservation {
    mount_id: u64,
    overlay: bool,
    read_write: bool,
    read_only: bool,
    nodev: bool,
    nosuid: bool,
    noexec: bool,
}

impl VisibleMountObservation {
    const fn has_exact_v1_policy(self) -> bool {
        self.overlay
            && self.read_write
            && !self.read_only
            && self.nodev
            && self.nosuid
            && !self.noexec
    }
}

trait OverlayMountBackend {
    type Prepared;
    type Created;
    type UnattachedMount;

    fn prepare(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<Self::Prepared, BackendError>;

    fn create(&mut self, prepared: Self::Prepared) -> Result<Self::Created, BackendError>;

    fn mount(
        &mut self,
        created: Self::Created,
    ) -> Result<(Self::UnattachedMount, u64), BackendError>;

    fn attach(
        &mut self,
        mount: &Self::UnattachedMount,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<(), BackendError>;

    fn visible_mount_observation(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<VisibleMountObservation, BackendError>;
}

fn execute_with_backend<B: OverlayMountBackend>(
    plan: &TrustedOverlayMountPlan,
    descriptor_lease: &TrustedOverlayMountDescriptorLease,
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    correlation: &TrustedProjectFilesystemCorrelationProof,
    backend: &mut B,
) -> Result<TrustedOverlayMountExecutionReceipt, TrustedOverlayMountExecutionError> {
    confirm_inputs(
        plan,
        descriptor_lease,
        source_anchor,
        task_view,
        correlation,
    )?;
    let descriptors = descriptor_lease.execution_descriptors();
    let prepared = backend
        .prepare(&descriptors)
        .map_err(|error| map_backend(error, false, None))?;

    // SET_FD configuration retains kernel references but does not publish a mount. Reconfirm every
    // accepted authority immediately before CREATE enters the observed OverlayFS work-state mutation boundary.
    confirm_inputs(
        plan,
        descriptor_lease,
        source_anchor,
        task_view,
        correlation,
    )?;

    let created = backend
        .create(prepared)
        .map_err(|error| map_workdir_backend(error, None))?;
    let (mount, mount_id) = backend
        .mount(created)
        .map_err(|error| map_workdir_backend(error, None))?;
    if mount_id == 0 {
        return Err(workdir_reset_error(None));
    }
    let mount_identity =
        mount_identity(plan, correlation, mount_id).map_err(|_| workdir_reset_error(None))?;

    backend
        .attach(&mount, &descriptors)
        .map_err(|error| map_backend(error, true, Some(mount_identity.clone())))?;
    let visible = backend
        .visible_mount_observation(&descriptors)
        .map_err(|error| map_backend(error, true, Some(mount_identity.clone())))?;
    if visible.mount_id != mount_id || !visible.has_exact_v1_policy() {
        return Err(observation_error(true, Some(mount_identity)));
    }

    Ok(TrustedOverlayMountExecutionReceipt {
        schema_version: TRUSTED_OVERLAY_MOUNT_EXECUTION_SCHEMA_VERSION,
        task_lease: task_view.lease().clone(),
        source_anchor_id: source_anchor.binding().anchor_id().clone(),
        source_anchor_generation: source_anchor.binding().anchor_generation(),
        correlation_generation: correlation.summary().correlation_generation(),
        option_policy: plan.summary().option_policy(),
        mount_identity,
    })
}

fn confirm_inputs(
    plan: &TrustedOverlayMountPlan,
    descriptor_lease: &TrustedOverlayMountDescriptorLease,
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    correlation: &TrustedProjectFilesystemCorrelationProof,
) -> Result<(), TrustedOverlayMountExecutionError> {
    plan.confirm(source_anchor, task_view)
        .map_err(|_| authority_error())?;
    descriptor_lease
        .confirm(plan, source_anchor, task_view)
        .map_err(|_| authority_error())?;
    let descriptors = descriptor_lease.execution_descriptors();
    let stat = rustix_fs::fstat(descriptors.lower).map_err(|_| observation_error(false, None))?;
    correlation
        .confirm_overlay_anchor(source_anchor.binding(), stat.st_dev)
        .map_err(|_| correlation_error())?;
    Ok(())
}

struct RustixOverlayMountBackend;

impl OverlayMountBackend for RustixOverlayMountBackend {
    type Prepared = OwnedFd;
    type Created = OwnedFd;
    type UnattachedMount = OwnedFd;

    fn prepare(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<Self::Prepared, BackendError> {
        let context = fsopen(OVERLAY_FILESYSTEM, FsOpenFlags::FSOPEN_CLOEXEC)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        fsconfig_set_fd(&context, LOWER_KEY, descriptors.lower)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        fsconfig_set_fd(&context, UPPER_KEY, descriptors.upper)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        fsconfig_set_fd(&context, WORK_KEY, descriptors.work)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        Ok(context)
    }

    fn create(&mut self, prepared: Self::Prepared) -> Result<Self::Created, BackendError> {
        fsconfig_create(&prepared).map_err(|error| backend_error(error, BackendStage::Create))?;
        Ok(prepared)
    }

    fn mount(
        &mut self,
        created: Self::Created,
    ) -> Result<(Self::UnattachedMount, u64), BackendError> {
        let mount = fsmount(
            &created,
            FsMountFlags::FSMOUNT_CLOEXEC,
            MountAttrFlags::MOUNT_ATTR_NODEV | MountAttrFlags::MOUNT_ATTR_NOSUID,
        )
        .map_err(|error| backend_error(error, BackendStage::Mount))?;
        let mount_id =
            mount_id(&mount).map_err(|error| backend_error(error, BackendStage::Observe))?;
        Ok((mount, mount_id))
    }

    fn attach(
        &mut self,
        mount: &Self::UnattachedMount,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<(), BackendError> {
        move_mount(
            mount,
            "",
            descriptors.merged,
            "",
            MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
        )
        .map_err(|error| backend_error(error, BackendStage::Attach))
    }

    fn visible_mount_observation(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<VisibleMountObservation, BackendError> {
        let visible = rustix_fs::openat(
            descriptors.merged_parent,
            descriptors.merged_name,
            VISIBLE_DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| backend_error(error, BackendStage::Observe))?;
        let visible_mount_id =
            mount_id(&visible).map_err(|error| backend_error(error, BackendStage::Observe))?;
        observe_mountinfo(visible_mount_id)
    }
}

fn mount_id(fd: &impl std::os::fd::AsFd) -> rustix::io::Result<u64> {
    Ok(statx(fd, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?.stx_mnt_id)
}

fn observe_mountinfo(mount_id: u64) -> Result<VisibleMountObservation, BackendError> {
    let file = File::open("/proc/self/mountinfo").map_err(|_| observation_backend_error())?;
    let mut limited = file.take((MAX_PROC_MOUNTINFO_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|_| observation_backend_error())?;
    if bytes.len() > MAX_PROC_MOUNTINFO_BYTES {
        return Err(observation_backend_error());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| observation_backend_error())?;
    let mut found = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        if fields.len() < 7 {
            return Err(observation_backend_error());
        }
        let current_id = fields[0]
            .parse::<u64>()
            .map_err(|_| observation_backend_error())?;
        if current_id != mount_id {
            continue;
        }
        if found.is_some() {
            return Err(observation_backend_error());
        }
        let separator = fields
            .iter()
            .position(|field| *field == "-")
            .ok_or_else(observation_backend_error)?;
        if separator < 6 || separator + 1 >= fields.len() {
            return Err(observation_backend_error());
        }
        let mut read_write = false;
        let mut read_only = false;
        let mut nodev = false;
        let mut nosuid = false;
        let mut noexec = false;
        for option in fields[5].split(',') {
            match option {
                "rw" => read_write = true,
                "ro" => read_only = true,
                "nodev" => nodev = true,
                "nosuid" => nosuid = true,
                "noexec" => noexec = true,
                _ => {}
            }
        }
        found = Some(VisibleMountObservation {
            mount_id,
            overlay: fields[separator + 1] == OVERLAY_FILESYSTEM,
            read_write,
            read_only,
            nodev,
            nosuid,
            noexec,
        });
    }
    found.ok_or_else(observation_backend_error)
}

const fn observation_backend_error() -> BackendError {
    BackendError {
        stage: BackendStage::Observe,
        class: BackendErrorClass::Other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendStage {
    Prepare,
    Create,
    Mount,
    Attach,
    Observe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackendError {
    stage: BackendStage,
    class: BackendErrorClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendErrorClass {
    Unavailable,
    Permission,
    Other,
}

fn backend_error(error: Errno, stage: BackendStage) -> BackendError {
    let class = match error {
        Errno::NOSYS => BackendErrorClass::Unavailable,
        Errno::ACCESS | Errno::PERM => BackendErrorClass::Permission,
        _ => BackendErrorClass::Other,
    };
    BackendError { stage, class }
}

fn map_backend(
    backend_error: BackendError,
    recovery_required: bool,
    mount_identity: Option<Sha256Digest>,
) -> TrustedOverlayMountExecutionError {
    if recovery_required {
        return error(
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired,
            "overlay_mount_attach_requires_recovery",
            "the OverlayFS attach attempt requires exact recovery observation",
            true,
            mount_identity,
        );
    }
    let (kind, code, message) = match backend_error.class {
        BackendErrorClass::Unavailable => (
            TrustedOverlayMountExecutionErrorKind::KernelUnavailable,
            "overlay_new_mount_api_unavailable",
            "the reviewed Linux OverlayFS mount API is unavailable",
        ),
        BackendErrorClass::Permission => (
            TrustedOverlayMountExecutionErrorKind::PermissionDenied,
            "overlay_mount_permission_denied",
            "current guest authority cannot create the reviewed OverlayFS mount",
        ),
        BackendErrorClass::Other => match backend_error.stage {
            BackendStage::Observe => (
                TrustedOverlayMountExecutionErrorKind::Observation,
                "overlay_mount_observation_failed",
                "the exact OverlayFS mount identity could not be observed",
            ),
            BackendStage::Prepare
            | BackendStage::Create
            | BackendStage::Mount
            | BackendStage::Attach => (
                TrustedOverlayMountExecutionErrorKind::MountConfiguration,
                "overlay_mount_operation_failed",
                "the reviewed OverlayFS mount transaction failed",
            ),
        },
    };
    TrustedOverlayMountExecutionError {
        kind,
        code,
        message,
        recovery_required,
        mount_identity,
    }
}

fn map_workdir_backend(
    _error: BackendError,
    mount_identity: Option<Sha256Digest>,
) -> TrustedOverlayMountExecutionError {
    workdir_reset_error(mount_identity)
}

fn workdir_reset_error(mount_identity: Option<Sha256Digest>) -> TrustedOverlayMountExecutionError {
    error(
        TrustedOverlayMountExecutionErrorKind::WorkdirResetRequired,
        "overlay_workdir_revalidation_required",
        "OverlayFS work state requires exact reset and revalidation before retry",
        true,
        mount_identity,
    )
}

fn authority_error() -> TrustedOverlayMountExecutionError {
    error(
        TrustedOverlayMountExecutionErrorKind::Authority,
        "overlay_mount_authority_changed",
        "trusted OverlayFS mount authority changed before execution",
        false,
        None,
    )
}

fn correlation_error() -> TrustedOverlayMountExecutionError {
    error(
        TrustedOverlayMountExecutionErrorKind::Correlation,
        "overlay_mount_project_filesystem_unproven",
        "the exact project-disk/filesystem correlation is not proven",
        false,
        None,
    )
}

fn observation_error(
    recovery_required: bool,
    mount_identity: Option<Sha256Digest>,
) -> TrustedOverlayMountExecutionError {
    error(
        if recovery_required {
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired
        } else {
            TrustedOverlayMountExecutionErrorKind::Observation
        },
        if recovery_required {
            "overlay_mount_post_attach_unproven"
        } else {
            "overlay_mount_identity_unavailable"
        },
        if recovery_required {
            "attached OverlayFS state requires exact recovery observation"
        } else {
            "the exact OverlayFS mount identity could not be observed"
        },
        recovery_required,
        mount_identity,
    )
}

const fn error(
    kind: TrustedOverlayMountExecutionErrorKind,
    code: &'static str,
    message: &'static str,
    recovery_required: bool,
    mount_identity: Option<Sha256Digest>,
) -> TrustedOverlayMountExecutionError {
    TrustedOverlayMountExecutionError {
        kind,
        code,
        message,
        recovery_required,
        mount_identity,
    }
}

fn mount_identity(
    plan: &TrustedOverlayMountPlan,
    correlation: &TrustedProjectFilesystemCorrelationProof,
    mount_id: u64,
) -> Result<Sha256Digest, TrustedOverlayMountExecutionError> {
    let mut hasher = Sha256::new();
    hasher.update(MOUNT_ID_DOMAIN);
    hasher.update(plan.kernel().mount_namespace_device().to_be_bytes());
    hasher.update(plan.kernel().mount_namespace_inode().to_be_bytes());
    hasher.update(mount_id.to_be_bytes());
    push_token(&mut hasher, plan.task_lease().task_id().as_str())?;
    hasher.update(plan.task_lease().generation().get().to_be_bytes());
    push_token(&mut hasher, plan.source_anchor().anchor_id().as_str())?;
    hasher.update(plan.source_anchor().anchor_generation().get().to_be_bytes());
    hasher.update(
        correlation
            .summary()
            .correlation_generation()
            .get()
            .to_be_bytes(),
    );
    digest_to_sha256(hasher.finalize().as_slice())
}

fn push_token(hasher: &mut Sha256, value: &str) -> Result<(), TrustedOverlayMountExecutionError> {
    let length = u32::try_from(value.len()).map_err(|_| observation_error(false, None))?;
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
    Ok(())
}

fn digest_to_sha256(bytes: &[u8]) -> Result<Sha256Digest, TrustedOverlayMountExecutionError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| observation_error(false, None))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        BackendError, BackendErrorClass, BackendStage, OverlayMountBackend,
        TrustedOverlayMountExecutionDescriptors, TrustedOverlayMountExecutionErrorKind,
        VisibleMountObservation, execute_with_backend,
    };
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
        ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_overlay_mount_plan::{TrustedOverlayMountPaths, descriptor_test_plan};
    use crate::trusted_overlay_task_view::{
        OverlayGitProofObservation, OverlayGitWorktreeObservation, OverlayIndexObservation,
        OverlayMountObservation, OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration,
        OverlaySourceAnchorId, OverlaySourceAnchorRecord, OverlayTaskProcessObservation,
        OverlayTaskViewGeneration, OverlayTaskViewId, OverlayTaskViewLease,
        OverlayTaskViewObservation, OverlayTaskViewRecord,
    };
    use crate::trusted_project_filesystem_correlation::{
        TrustedProjectFilesystemCorrelationGeneration, TrustedProjectFilesystemCorrelationProof,
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        work: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "smolrunner-overlay-execution-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            for name in ["lower", "upper", "work", "merged"] {
                fs::create_dir_all(root.join(name)).unwrap();
            }
            Self {
                work: root.join("work"),
                root,
            }
        }

        fn paths(&self) -> TrustedOverlayMountPaths {
            TrustedOverlayMountPaths::new(
                self.root.join("lower"),
                self.root.join("upper"),
                self.root.join("work"),
                self.root.join("merged"),
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn anchor_binding() -> OverlaySourceAnchorBinding {
        OverlaySourceAnchorBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskRevision::new(4).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            ProjectDiskAttachmentGeneration::new(7).unwrap(),
            OverlaySourceAnchorId::parse("anchor-a").unwrap(),
            OverlaySourceAnchorGeneration::new(5).unwrap(),
            CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
            GitTreeId::parse("89abcdef0123456789abcdef0123456789abcdef").unwrap(),
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
    }

    fn authority() -> (OverlaySourceAnchorRecord, OverlayTaskViewRecord) {
        let binding = anchor_binding();
        let lease = OverlayTaskViewLease::new(
            OverlayTaskViewId::parse("task-a").unwrap(),
            OverlayTaskViewGeneration::new(7).unwrap(),
        );
        let anchor = OverlaySourceAnchorRecord::new_ready(binding.clone())
            .acquire_task(lease.clone())
            .unwrap();
        let task = OverlayTaskViewRecord::new_planned(lease, binding)
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

    fn correlation(
        anchor: &OverlaySourceAnchorRecord,
        device: u64,
    ) -> TrustedProjectFilesystemCorrelationProof {
        TrustedProjectFilesystemCorrelationProof::for_test(
            anchor.binding(),
            ProjectDiskAttachmentGeneration::new(9).unwrap(),
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            device,
        )
    }

    struct FakeBackend {
        mount_id: u64,
        visible_id: u64,
        visible_overlay: bool,
        visible_read_write: bool,
        visible_read_only: bool,
        visible_nodev: bool,
        visible_nosuid: bool,
        visible_noexec: bool,
        mutate_during_prepare: Option<PathBuf>,
        prepared: bool,
        create_attempted: bool,
        mount_attempted: bool,
        attached: bool,
        fail_prepare: bool,
        fail_create: bool,
        fail_mount: bool,
        fail_attach: bool,
        fail_visible: bool,
    }

    impl FakeBackend {
        fn successful() -> Self {
            Self {
                mount_id: 41,
                visible_id: 41,
                visible_overlay: true,
                visible_read_write: true,
                visible_read_only: false,
                visible_nodev: true,
                visible_nosuid: true,
                visible_noexec: false,
                mutate_during_prepare: None,
                prepared: false,
                create_attempted: false,
                mount_attempted: false,
                attached: false,
                fail_prepare: false,
                fail_create: false,
                fail_mount: false,
                fail_attach: false,
                fail_visible: false,
            }
        }
    }

    impl OverlayMountBackend for FakeBackend {
        type Prepared = ();
        type Created = ();
        type UnattachedMount = ();

        fn prepare(
            &mut self,
            _descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
        ) -> Result<Self::Prepared, BackendError> {
            if self.fail_prepare {
                return Err(BackendError {
                    stage: BackendStage::Prepare,
                    class: BackendErrorClass::Other,
                });
            }
            self.prepared = true;
            if let Some(path) = self.mutate_during_prepare.as_ref() {
                fs::write(path.join("late"), b"x").unwrap();
            }
            Ok(())
        }

        fn create(&mut self, _prepared: Self::Prepared) -> Result<Self::Created, BackendError> {
            self.create_attempted = true;
            if self.fail_create {
                return Err(BackendError {
                    stage: BackendStage::Create,
                    class: BackendErrorClass::Other,
                });
            }
            Ok(())
        }

        fn mount(
            &mut self,
            _created: Self::Created,
        ) -> Result<(Self::UnattachedMount, u64), BackendError> {
            self.mount_attempted = true;
            if self.fail_mount {
                return Err(BackendError {
                    stage: BackendStage::Mount,
                    class: BackendErrorClass::Other,
                });
            }
            Ok(((), self.mount_id))
        }

        fn attach(
            &mut self,
            _mount: &Self::UnattachedMount,
            _descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
        ) -> Result<(), BackendError> {
            if self.fail_attach {
                return Err(BackendError {
                    stage: BackendStage::Attach,
                    class: BackendErrorClass::Other,
                });
            }
            self.attached = true;
            Ok(())
        }

        fn visible_mount_observation(
            &mut self,
            _descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
        ) -> Result<VisibleMountObservation, BackendError> {
            if self.fail_visible {
                return Err(BackendError {
                    stage: BackendStage::Observe,
                    class: BackendErrorClass::Other,
                });
            }
            Ok(VisibleMountObservation {
                mount_id: self.visible_id,
                overlay: self.visible_overlay,
                read_write: self.visible_read_write,
                read_only: self.visible_read_only,
                nodev: self.visible_nodev,
                nosuid: self.visible_nosuid,
                noexec: self.visible_noexec,
            })
        }
    }

    fn inputs(
        fixture: &Fixture,
    ) -> (
        OverlaySourceAnchorRecord,
        OverlayTaskViewRecord,
        crate::trusted_overlay_mount_plan::TrustedOverlayMountPlan,
        crate::trusted_overlay_mount_plan::TrustedOverlayMountDescriptorLease,
        TrustedProjectFilesystemCorrelationProof,
    ) {
        let (anchor, task) = authority();
        let plan = descriptor_test_plan(&anchor, &task, fixture.paths()).unwrap();
        let lease = plan.open_descriptor_lease(&anchor, &task).unwrap();
        let device = fs::metadata(fixture.root.join("lower")).unwrap().dev();
        let proof = correlation(&anchor, device);
        (anchor, task, plan, lease, proof)
    }

    #[test]
    fn fake_backend_accepts_exact_descriptor_bound_transaction() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        let receipt =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap();
        assert!(backend.prepared);
        assert!(backend.create_attempted);
        assert!(backend.mount_attempted);
        assert!(backend.attached);
        assert_eq!(receipt.task_lease(), task.lease());
        assert_eq!(receipt.correlation_generation().get(), 13);
        assert!(receipt.mount_identity().as_str().starts_with("sha256:"));
    }

    #[test]
    fn second_confirmation_blocks_drift_before_create_boundary() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.mutate_during_prepare = Some(fixture.work.clone());
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::Authority
        );
        assert!(!error.recovery_required());
        assert!(backend.prepared);
        assert!(!backend.create_attempted);
        assert!(!backend.mount_attempted);
        assert!(!backend.attached);
    }

    #[test]
    fn correlation_mismatch_blocks_backend_before_prepare() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, _) = inputs(&fixture);
        let proof = correlation(&anchor, u64::MAX);
        let mut backend = FakeBackend::successful();
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::Correlation
        );
        assert!(!error.recovery_required());
        assert!(!backend.prepared);
    }

    #[test]
    fn prepare_failure_has_no_recovery_debt() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.fail_prepare = true;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::MountConfiguration
        );
        assert!(!error.recovery_required());
        assert!(error.mount_identity().is_none());
        assert!(!backend.create_attempted);
    }

    #[test]
    fn create_failure_requires_workdir_revalidation() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.fail_create = true;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::WorkdirResetRequired
        );
        assert!(error.recovery_required());
        assert!(error.mount_identity().is_none());
        assert!(backend.create_attempted);
        assert!(!backend.mount_attempted);
        assert!(!backend.attached);
    }

    #[test]
    fn mount_failure_after_create_requires_workdir_revalidation() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.fail_mount = true;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::WorkdirResetRequired
        );
        assert!(error.recovery_required());
        assert!(backend.create_attempted);
        assert!(backend.mount_attempted);
        assert!(!backend.attached);
    }

    #[test]
    fn missing_unattached_mount_id_requires_workdir_revalidation() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.mount_id = 0;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::WorkdirResetRequired
        );
        assert!(error.recovery_required());
        assert!(error.mount_identity().is_none());
        assert!(!backend.attached);
    }

    #[test]
    fn attach_failure_is_mount_recovery_debt_with_identity() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.fail_attach = true;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired
        );
        assert!(error.recovery_required());
        assert!(error.mount_identity().is_some());
    }

    #[test]
    fn visible_observation_failure_is_mount_recovery_debt() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.fail_visible = true;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired
        );
        assert!(error.recovery_required());
        assert!(error.mount_identity().is_some());
        assert!(backend.attached);
    }

    #[test]
    fn visible_mount_id_mismatch_is_mount_recovery_debt() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.visible_id = 42;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired
        );
        assert!(error.recovery_required());
        assert!(backend.attached);
    }

    #[test]
    fn visible_non_overlay_mount_is_mount_recovery_debt() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.visible_overlay = false;
        let error =
            execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired
        );
        assert!(error.recovery_required());
    }

    #[test]
    fn visible_policy_drift_is_mount_recovery_debt() {
        for case in 0..5 {
            let fixture = Fixture::new();
            let (anchor, task, plan, lease, proof) = inputs(&fixture);
            let mut backend = FakeBackend::successful();
            match case {
                0 => backend.visible_read_write = false,
                1 => backend.visible_read_only = true,
                2 => backend.visible_nodev = false,
                3 => backend.visible_nosuid = false,
                4 => backend.visible_noexec = true,
                _ => unreachable!(),
            }
            let error = execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend)
                .unwrap_err();
            assert_eq!(
                error.kind(),
                TrustedOverlayMountExecutionErrorKind::RecoveryRequired
            );
            assert!(error.recovery_required());
            assert!(backend.attached);
        }
    }
}
