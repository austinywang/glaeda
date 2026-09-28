use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::time::Duration;

use rustix::fs::{self, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{getegid, geteuid};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::local_install_build_command::directory_preflight::{
    LocalInstallDirectoryPreflightReceipt, observe_local_install_directory_preflight,
};
use crate::local_install_build_command::toolchain_preflight::{
    LocalInstallToolchainPreflightReceipt, observe_local_install_toolchain_preflight,
};
use crate::local_install_build_command::{
    LocalInstallBuildCommandContext, LocalInstallBuildCommandError,
    LocalInstallBuildCommandIdentity, plan_local_install_build_command,
};
use crate::local_install_cargo_config_preflight::{
    LocalInstallCargoConfigPreflightContext, LocalInstallCargoConfigPreflightReceipt,
    observe_local_install_cargo_config_preflight,
};
use crate::local_install_plan::{
    BuiltLocalBinaryEvidence, LocalInstallBuildPlan, LocalInstallIdentityGeneration,
    LocalInstallPlatform,
};
use crate::local_install_source_preflight::{
    LocalInstallSourcePreflightReceipt, observe_local_install_source_preflight,
};
use crate::process::{CommandSpec, ExecutionRecord, TimedWorkingDirectoryCommandExecutor};
use crate::project_checkout_observation::ProjectCheckoutObserver;

pub const LOCAL_INSTALL_BUILD_EXECUTION_SCHEMA_VERSION: u8 = 1;
pub const LOCAL_INSTALL_ARTIFACT_VERSION_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_LOCAL_INSTALL_BINARY_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_LOCAL_INSTALL_BINARY_VERSION_BYTES: usize = 128;
pub const MAX_REVIEWED_CARGO_ARTIFACT_LINKS: u64 = 2;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const SHA256_PREFIX: &str = "sha256:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallBuildExecutionOutcome {
    Refused,
    Failed,
    Succeeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallBuildExecutionCode {
    DirectoryNotReady,
    CargoConfigNotReady,
    ToolchainNotReady,
    SourceNotReady,
    BuildTimeout,
    BuildOutputLimit,
    DependencyMaterialMissing,
    BuildFailed,
    BuildUnavailable,
    SourceChanged,
    ArtifactMissing,
    ArtifactUnsafe,
    ArtifactUnknown,
    ArtifactChanged,
    ArtifactVersionInvalid,
}

/// Path- and output-private result of one exact bounded local self-build attempt.
///
/// The embedded preflight receipts retain their detailed fixed blocker vocabularies. Raw paths,
/// compiler output, filesystem identities, and operating-system errors are never retained here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallBuildExecutionReceipt {
    schema_version: u8,
    identity_generation: LocalInstallIdentityGeneration,
    command_identity: LocalInstallBuildCommandIdentity,
    directory_preflight: LocalInstallDirectoryPreflightReceipt,
    cargo_config_preflight: LocalInstallCargoConfigPreflightReceipt,
    toolchain_preflight: LocalInstallToolchainPreflightReceipt,
    source_before: LocalInstallSourcePreflightReceipt,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_after: Option<LocalInstallSourcePreflightReceipt>,
    outcome: LocalInstallBuildExecutionOutcome,
    codes: Vec<LocalInstallBuildExecutionCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<BuiltLocalBinaryEvidence>,
}

impl LocalInstallBuildExecutionReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn outcome(&self) -> LocalInstallBuildExecutionOutcome {
        self.outcome
    }

    #[must_use]
    pub fn codes(&self) -> &[LocalInstallBuildExecutionCode] {
        &self.codes
    }

    #[must_use]
    pub const fn evidence(&self) -> Option<&BuiltLocalBinaryEvidence> {
        self.evidence.as_ref()
    }

    #[must_use]
    pub const fn directory_preflight(&self) -> &LocalInstallDirectoryPreflightReceipt {
        &self.directory_preflight
    }

    #[must_use]
    pub const fn cargo_config_preflight(&self) -> &LocalInstallCargoConfigPreflightReceipt {
        &self.cargo_config_preflight
    }

    #[must_use]
    pub const fn toolchain_preflight(&self) -> &LocalInstallToolchainPreflightReceipt {
        &self.toolchain_preflight
    }

    #[must_use]
    pub const fn source_before(&self) -> &LocalInstallSourcePreflightReceipt {
        &self.source_before
    }

    #[must_use]
    pub const fn source_after(&self) -> Option<&LocalInstallSourcePreflightReceipt> {
        self.source_after.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallBuildExecutionErrorKind {
    InvalidCommand,
    InvalidRunnerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallBuildExecutionError {
    pub kind: LocalInstallBuildExecutionErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for LocalInstallBuildExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LocalInstallBuildExecutionError {}

/// Execute the sole reviewed offline build command and bind its exact output to source evidence.
///
/// This function creates no directories and publishes no generation. The caller must prepare the
/// private build root and four derived directories first; their exact state is then re-observed
/// here before Cargo starts.
///
/// # Errors
///
/// Returns an error only for invalid caller input or an unsupported root runner identity. Runtime
/// refusals and process/artifact failures are returned as bounded receipts.
pub fn execute_local_install_build(
    plan: &LocalInstallBuildPlan,
    platform: LocalInstallPlatform,
    context: &LocalInstallBuildCommandContext,
    jobs: u8,
    checkout_observer: &ProjectCheckoutObserver,
    executor: &impl TimedWorkingDirectoryCommandExecutor,
) -> Result<LocalInstallBuildExecutionReceipt, LocalInstallBuildExecutionError> {
    let command =
        plan_local_install_build_command(plan, platform, context, jobs).map_err(invalid_command)?;
    let runner_uid = geteuid().as_raw();
    let runner_gid = getegid().as_raw();
    let cargo_context = LocalInstallCargoConfigPreflightContext::new(
        context.build_root().to_path_buf(),
        runner_uid,
        runner_gid,
    )
    .map_err(|_| LocalInstallBuildExecutionError {
        kind: LocalInstallBuildExecutionErrorKind::InvalidRunnerIdentity,
        code: "invalid_runner_identity",
        problem: "local self-build requires one reviewed non-root runner identity",
    })?;

    let directory_preflight = observe_local_install_directory_preflight(context);
    let cargo_config_preflight = observe_local_install_cargo_config_preflight(&cargo_context);
    let toolchain_preflight =
        observe_local_install_toolchain_preflight(plan.source.toolchain(), context, executor);
    let source_before = observe_local_install_source_preflight(
        &plan.source,
        context.source_root(),
        checkout_observer,
        executor,
    );
    let mut codes = BTreeSet::new();
    if !directory_preflight.ready() {
        codes.insert(LocalInstallBuildExecutionCode::DirectoryNotReady);
    }
    if !cargo_config_preflight.ready() {
        codes.insert(LocalInstallBuildExecutionCode::CargoConfigNotReady);
    }
    if !toolchain_preflight.ready() {
        codes.insert(LocalInstallBuildExecutionCode::ToolchainNotReady);
    }
    if !source_before.ready() {
        codes.insert(LocalInstallBuildExecutionCode::SourceNotReady);
    }
    if !codes.is_empty() {
        return Ok(receipt(
            plan,
            &command.policy().identity,
            directory_preflight,
            cargo_config_preflight,
            toolchain_preflight,
            source_before,
            None,
            LocalInstallBuildExecutionOutcome::Refused,
            codes,
            None,
        ));
    }

    let build = executor.execute_in_directory_with_timeout(
        command.spec(),
        command.working_directory(),
        command.timeout(),
    );
    let build = match build {
        Ok(record) => record,
        Err(error) => {
            codes.insert(match error.kind() {
                io::ErrorKind::TimedOut => LocalInstallBuildExecutionCode::BuildTimeout,
                io::ErrorKind::InvalidData => LocalInstallBuildExecutionCode::BuildOutputLimit,
                _ => LocalInstallBuildExecutionCode::BuildUnavailable,
            });
            return Ok(receipt(
                plan,
                &command.policy().identity,
                directory_preflight,
                cargo_config_preflight,
                toolchain_preflight,
                source_before,
                None,
                LocalInstallBuildExecutionOutcome::Failed,
                codes,
                None,
            ));
        }
    };
    if !record_matches(&build, command.spec()) || !build.success || build.status != Some(0) {
        codes.insert(if dependency_material_missing(&build) {
            LocalInstallBuildExecutionCode::DependencyMaterialMissing
        } else if record_matches(&build, command.spec()) {
            LocalInstallBuildExecutionCode::BuildFailed
        } else {
            LocalInstallBuildExecutionCode::BuildUnavailable
        });
        return Ok(receipt(
            plan,
            &command.policy().identity,
            directory_preflight,
            cargo_config_preflight,
            toolchain_preflight,
            source_before,
            None,
            LocalInstallBuildExecutionOutcome::Failed,
            codes,
            None,
        ));
    }

    let source_after = observe_local_install_source_preflight(
        &plan.source,
        context.source_root(),
        checkout_observer,
        executor,
    );
    if !source_after.ready() || source_after != source_before {
        codes.insert(LocalInstallBuildExecutionCode::SourceChanged);
        return Ok(receipt(
            plan,
            &command.policy().identity,
            directory_preflight,
            cargo_config_preflight,
            toolchain_preflight,
            source_before,
            Some(source_after),
            LocalInstallBuildExecutionOutcome::Failed,
            codes,
            None,
        ));
    }

    let artifact_before = match artifact_snapshot(context.build_root(), command.artifact_path()) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            codes.insert(error.code());
            return Ok(receipt(
                plan,
                &command.policy().identity,
                directory_preflight,
                cargo_config_preflight,
                toolchain_preflight,
                source_before,
                Some(source_after),
                LocalInstallBuildExecutionOutcome::Failed,
                codes,
                None,
            ));
        }
    };
    let version_spec = CommandSpec::new(command.artifact_path().to_path_buf())
        .argument("--version")
        .environment("LANG", "C")
        .environment("LC_ALL", "C");
    let version_record = executor.execute_in_directory_with_timeout(
        &version_spec,
        command.working_directory(),
        LOCAL_INSTALL_ARTIFACT_VERSION_TIMEOUT,
    );
    let version = version_record.ok().and_then(|record| {
        canonical_binary_version(plan.source.identity_generation(), &record, &version_spec)
    });
    let Some(version) = version else {
        codes.insert(LocalInstallBuildExecutionCode::ArtifactVersionInvalid);
        return Ok(receipt(
            plan,
            &command.policy().identity,
            directory_preflight,
            cargo_config_preflight,
            toolchain_preflight,
            source_before,
            Some(source_after),
            LocalInstallBuildExecutionOutcome::Failed,
            codes,
            None,
        ));
    };
    let artifact_after = match artifact_snapshot(context.build_root(), command.artifact_path()) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            codes.insert(LocalInstallBuildExecutionCode::ArtifactChanged);
            return Ok(receipt(
                plan,
                &command.policy().identity,
                directory_preflight,
                cargo_config_preflight,
                toolchain_preflight,
                source_before,
                Some(source_after),
                LocalInstallBuildExecutionOutcome::Failed,
                codes,
                None,
            ));
        }
    };
    if !artifact_before.same_as(&artifact_after) {
        codes.insert(LocalInstallBuildExecutionCode::ArtifactChanged);
        return Ok(receipt(
            plan,
            &command.policy().identity,
            directory_preflight,
            cargo_config_preflight,
            toolchain_preflight,
            source_before,
            Some(source_after),
            LocalInstallBuildExecutionOutcome::Failed,
            codes,
            None,
        ));
    }

    let evidence = BuiltLocalBinaryEvidence::new(
        plan.source.digest().clone(),
        plan.expected_predecessor.clone(),
        artifact_after.digest,
        version,
    )
    .expect("canonical binary version satisfies local-install evidence bounds");
    Ok(receipt(
        plan,
        &command.policy().identity,
        directory_preflight,
        cargo_config_preflight,
        toolchain_preflight,
        source_before,
        Some(source_after),
        LocalInstallBuildExecutionOutcome::Succeeded,
        codes,
        Some(evidence),
    ))
}

#[allow(clippy::too_many_arguments)]
fn receipt(
    plan: &LocalInstallBuildPlan,
    command_identity: &LocalInstallBuildCommandIdentity,
    directory_preflight: LocalInstallDirectoryPreflightReceipt,
    cargo_config_preflight: LocalInstallCargoConfigPreflightReceipt,
    toolchain_preflight: LocalInstallToolchainPreflightReceipt,
    source_before: LocalInstallSourcePreflightReceipt,
    source_after: Option<LocalInstallSourcePreflightReceipt>,
    outcome: LocalInstallBuildExecutionOutcome,
    codes: BTreeSet<LocalInstallBuildExecutionCode>,
    evidence: Option<BuiltLocalBinaryEvidence>,
) -> LocalInstallBuildExecutionReceipt {
    LocalInstallBuildExecutionReceipt {
        schema_version: LOCAL_INSTALL_BUILD_EXECUTION_SCHEMA_VERSION,
        identity_generation: plan.source.identity_generation(),
        command_identity: command_identity.clone(),
        directory_preflight,
        cargo_config_preflight,
        toolchain_preflight,
        source_before,
        source_after,
        outcome,
        codes: codes.into_iter().collect(),
        evidence,
    }
}

fn invalid_command(_: LocalInstallBuildCommandError) -> LocalInstallBuildExecutionError {
    LocalInstallBuildExecutionError {
        kind: LocalInstallBuildExecutionErrorKind::InvalidCommand,
        code: "invalid_command",
        problem: "local self-build command input is outside the reviewed policy",
    }
}

fn record_matches(record: &ExecutionRecord, spec: &CommandSpec) -> bool {
    record.argv == spec.displayed_argv()
        && record.environment_keys == spec.environment.keys().cloned().collect::<Vec<_>>()
}

fn dependency_material_missing(record: &ExecutionRecord) -> bool {
    let output = format!("{}\n{}", record.stdout, record.stderr);
    (output.contains("no matching package named") && output.contains("location searched"))
        || output.contains("attempting to make an HTTP request, but --offline was specified")
        || (output.contains("failed to download") && output.contains("--offline"))
}

fn canonical_binary_version(
    generation: LocalInstallIdentityGeneration,
    record: &ExecutionRecord,
    spec: &CommandSpec,
) -> Option<String> {
    if !record_matches(record, spec)
        || !record.success
        || record.status != Some(0)
        || !record.stderr.is_empty()
        || record.stdout.len() > MAX_LOCAL_INSTALL_BINARY_VERSION_BYTES
        || record.stdout.contains(['\r', '\0'])
    {
        return None;
    }
    let line = record.stdout.strip_suffix('\n').unwrap_or(&record.stdout);
    if line.is_empty() || line.contains('\n') || !line.is_ascii() {
        return None;
    }
    let mut fields = line.split_ascii_whitespace();
    let expected_name = match generation {
        LocalInstallIdentityGeneration::SmolrunnerV1 => "smolrunner",
        LocalInstallIdentityGeneration::GlaedaV2 => "glaeda",
    };
    if fields.next()? != expected_name {
        return None;
    }
    let version = fields.next()?;
    if fields.next().is_some()
        || version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'))
    {
        return None;
    }
    Some(line.to_owned())
}

#[derive(Debug, Clone)]
struct ArtifactSnapshot {
    build_root: PrivateMetadata,
    target: PrivateMetadata,
    release: PrivateMetadata,
    file: PrivateMetadata,
    digest: Sha256Digest,
}

impl ArtifactSnapshot {
    fn same_as(&self, other: &Self) -> bool {
        self.build_root.same_as(&other.build_root)
            && self.target.same_as(&other.target)
            && self.release.same_as(&other.release)
            && self.file.same_as(&other.file)
            && self.digest == other.digest
    }
}

#[derive(Debug, Clone)]
struct PrivateMetadata {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl PrivateMetadata {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
            && self.links == other.links
            && self.size == other.size
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
            && self.changed_seconds == other.changed_seconds
            && self.changed_nanoseconds == other.changed_nanoseconds
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactObservationError {
    Missing,
    Unsafe,
    Unknown,
}

impl ArtifactObservationError {
    const fn code(self) -> LocalInstallBuildExecutionCode {
        match self {
            Self::Missing => LocalInstallBuildExecutionCode::ArtifactMissing,
            Self::Unsafe => LocalInstallBuildExecutionCode::ArtifactUnsafe,
            Self::Unknown => LocalInstallBuildExecutionCode::ArtifactUnknown,
        }
    }
}

fn artifact_snapshot(
    build_root: &Path,
    artifact: &Path,
) -> Result<ArtifactSnapshot, ArtifactObservationError> {
    let expected_relative = artifact
        .strip_prefix(build_root)
        .map_err(|_| ArtifactObservationError::Unsafe)?;
    let components = normal_components(expected_relative);
    if components.len() != 3 || components[0] != "target" || components[1] != "release" {
        return Err(ArtifactObservationError::Unsafe);
    }
    let (mut current, root_metadata) = open_exact_directory(build_root)?;
    if !private_build_root(&root_metadata) {
        return Err(ArtifactObservationError::Unsafe);
    }
    let target = open_child_directory(&current, components[0])?;
    let target_metadata = target
        .metadata()
        .map_err(|_| ArtifactObservationError::Unknown)?;
    if !owned_reviewed_directory(&target_metadata) {
        return Err(ArtifactObservationError::Unsafe);
    }
    current = target;
    let release = open_child_directory(&current, components[1])?;
    let release_metadata = release
        .metadata()
        .map_err(|_| ArtifactObservationError::Unknown)?;
    if !owned_reviewed_directory(&release_metadata) {
        return Err(ArtifactObservationError::Unsafe);
    }
    current = release;

    let opened = fs::openat(current.as_fd(), components[2], FILE_FLAGS, Mode::empty())
        .map_err(map_artifact_open)?;
    let mut file = File::from(opened);
    let before = file
        .metadata()
        .map_err(|_| ArtifactObservationError::Unknown)?;
    if !reviewed_artifact(&before) {
        return Err(ArtifactObservationError::Unsafe);
    }
    let digest = digest_bounded(&mut file, before.len())?;
    let after = file
        .metadata()
        .map_err(|_| ArtifactObservationError::Unknown)?;
    let before_private = PrivateMetadata::from_metadata(&before);
    let after_private = PrivateMetadata::from_metadata(&after);
    if !before_private.same_as(&after_private) {
        return Err(ArtifactObservationError::Unknown);
    }

    Ok(ArtifactSnapshot {
        build_root: PrivateMetadata::from_metadata(&root_metadata),
        target: PrivateMetadata::from_metadata(&target_metadata),
        release: PrivateMetadata::from_metadata(&release_metadata),
        file: after_private,
        digest,
    })
}

fn open_exact_directory(
    path: &Path,
) -> Result<(File, std::fs::Metadata), ArtifactObservationError> {
    if !valid_absolute_path(path) {
        return Err(ArtifactObservationError::Unsafe);
    }
    let root = fs::open("/", DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| ArtifactObservationError::Unknown)?;
    let mut current = File::from(root);
    for component in normal_components(path) {
        current = open_child_directory(&current, component)?;
    }
    let metadata = current
        .metadata()
        .map_err(|_| ArtifactObservationError::Unknown)?;
    Ok((current, metadata))
}

fn open_child_directory(parent: &File, name: &OsStr) -> Result<File, ArtifactObservationError> {
    let opened = fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_artifact_open)?;
    Ok(File::from(opened))
}

fn reviewed_artifact(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file()
        && metadata.uid() == geteuid().as_raw()
        // Cargo normally hard-links the selected release binary from `target/release/deps` into
        // `target/release`. Both names remain under the exact private target root. More links are
        // outside the reviewed Cargo publication shape.
        && (1..=MAX_REVIEWED_CARGO_ARTIFACT_LINKS).contains(&metadata.nlink())
        && metadata.mode() & 0o022 == 0
        && metadata.mode() & 0o111 != 0
        && (4..=MAX_LOCAL_INSTALL_BINARY_BYTES).contains(&metadata.len())
}

fn private_build_root(metadata: &std::fs::Metadata) -> bool {
    metadata.is_dir()
        && metadata.uid() == geteuid().as_raw()
        && metadata.gid() == getegid().as_raw()
        && metadata.mode() & 0o7777 == 0o700
}

fn owned_reviewed_directory(metadata: &std::fs::Metadata) -> bool {
    metadata.is_dir() && metadata.uid() == geteuid().as_raw() && metadata.mode() & 0o022 == 0
}

fn digest_bounded(
    file: &mut File,
    expected_size: u64,
) -> Result<Sha256Digest, ArtifactObservationError> {
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ArtifactObservationError::Unknown)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(ArtifactObservationError::Unsafe)?;
        if total > MAX_LOCAL_INSTALL_BINARY_BYTES {
            return Err(ArtifactObservationError::Unsafe);
        }
        if total == read as u64
            && (read < 4
                || !reviewed_executable_magic(buffer[..4].try_into().expect("four-byte prefix")))
        {
            return Err(ArtifactObservationError::Unsafe);
        }
        digest.update(&buffer[..read]);
    }
    if total != expected_size {
        return Err(ArtifactObservationError::Unknown);
    }
    let value = format!("{SHA256_PREFIX}{:x}", digest.finalize());
    Sha256Digest::parse(&value).map_err(|_| ArtifactObservationError::Unknown)
}

fn reviewed_executable_magic(magic: [u8; 4]) -> bool {
    if cfg!(target_os = "linux") {
        magic == *b"\x7fELF"
    } else if cfg!(target_os = "macos") {
        matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    } else {
        false
    }
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && path.to_str().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn normal_components(path: &Path) -> Vec<&OsStr> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            Component::RootDir => None,
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => None,
        })
        .collect()
}

fn map_artifact_open(error: Errno) -> ArtifactObservationError {
    match error {
        Errno::NOENT => ArtifactObservationError::Missing,
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => ArtifactObservationError::Unsafe,
        _ => ArtifactObservationError::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::artifact::{CommitId, GitTreeId};
    use crate::local_install_plan::{LocalInstallSourceIdentity, LocalInstallToolchainIdentity};
    use crate::process::{CommandExecutor, ProcessExecutor, TimedCommandExecutor};
    use crate::project_checkout_observation::expected_git_timeout;

    use super::*;

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const TREE: &str = "2222222222222222222222222222222222222222";
    const LOCK_BYTES: &[u8] = b"# exact Cargo lock\nversion = 4\n";
    const PRIVATE_SENTINEL: &str = "PRIVATE-COMPILER-OUTPUT-MUST-NOT-ESCAPE";
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        source: PathBuf,
        build: PathBuf,
        cargo: PathBuf,
        rustc: PathBuf,
        rustdoc: PathBuf,
        artifact: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::current_dir()
                .expect("current test checkout")
                .join("target/glaeda-build-execution-tests")
                .join(format!(
                    "glaeda-build-execution-{label}-{}-{sequence}",
                    std::process::id()
                ));
            let source = root.join("source");
            let build = root.join("build");
            let tools = root.join("tools");
            fs::create_dir_all(&source).expect("source root");
            fs::write(source.join("Cargo.lock"), LOCK_BYTES).expect("Cargo.lock");
            fs::create_dir_all(&build).expect("build root");
            set_mode(&build, 0o700);
            for child in ["work", "home", "cargo-home", "target"] {
                let path = build.join(child);
                fs::create_dir(&path).expect("build child");
                set_mode(&path, 0o700);
            }
            fs::create_dir(&tools).expect("tool root");
            let cargo = tools.join("cargo");
            let rustc = tools.join("rustc");
            let rustdoc = tools.join("rustdoc");
            for tool in [&cargo, &rustc, &rustdoc] {
                write_fake_binary(tool);
            }
            let artifact = build.join("target/release/glaeda");
            Self {
                root,
                source,
                build,
                cargo,
                rustc,
                rustdoc,
                artifact,
            }
        }

        fn context(&self) -> LocalInstallBuildCommandContext {
            LocalInstallBuildCommandContext::new(
                self.source.clone(),
                self.build.clone(),
                self.cargo.clone(),
                self.rustc.clone(),
                self.rustdoc.clone(),
            )
            .expect("build context")
        }

        fn plan(&self) -> LocalInstallBuildPlan {
            let lock_digest = digest_bytes(LOCK_BYTES);
            let source = LocalInstallSourceIdentity::new(
                CommitId::parse(COMMIT).expect("commit"),
                GitTreeId::parse(TREE).expect("tree"),
                lock_digest,
                LocalInstallToolchainIdentity::parse("rust-1.97.1-x86_64-unknown-linux-gnu")
                    .expect("toolchain"),
            )
            .expect("source identity");
            LocalInstallBuildPlan {
                target_generation: 1,
                expected_predecessor: None,
                source,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Clone)]
    struct Response {
        stdout: String,
        stderr: String,
        status: i32,
    }

    impl Response {
        fn success(stdout: impl Into<String>) -> Self {
            Self {
                stdout: stdout.into(),
                stderr: String::new(),
                status: 0,
            }
        }

        fn failed(status: i32, stderr: impl Into<String>) -> Self {
            Self {
                stdout: String::new(),
                stderr: stderr.into(),
                status,
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum BuildBehavior {
        Succeed,
        Fail,
        Timeout,
        OutputLimit,
        DependencyMissing,
        ChangeSource,
        ReplaceArtifactOnVersion,
    }

    struct ScriptedExecutor {
        git: RefCell<VecDeque<Response>>,
        artifact: PathBuf,
        cargo: PathBuf,
        rustc: PathBuf,
        rustdoc: PathBuf,
        source_lock: PathBuf,
        build_behavior: BuildBehavior,
        working_calls: Cell<usize>,
    }

    impl ScriptedExecutor {
        fn new(
            fixture: &Fixture,
            source_observations: usize,
            build_behavior: BuildBehavior,
        ) -> Self {
            let mut git = VecDeque::new();
            for _ in 0..source_observations {
                git.extend(git_observation(&fixture.source));
            }
            Self {
                git: RefCell::new(git),
                artifact: fixture.artifact.clone(),
                cargo: fixture.cargo.clone(),
                rustc: fixture.rustc.clone(),
                rustdoc: fixture.rustdoc.clone(),
                source_lock: fixture.source.join("Cargo.lock"),
                build_behavior,
                working_calls: Cell::new(0),
            }
        }
    }

    impl CommandExecutor for ScriptedExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            panic!("build execution requires bounded timed commands")
        }
    }

    impl TimedCommandExecutor for ScriptedExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            if spec.program == self.cargo {
                assert_eq!(timeout, crate::local_install_build_command::toolchain_preflight::LOCAL_INSTALL_TOOLCHAIN_PROBE_TIMEOUT);
                return Ok(success_record(spec, "cargo 1.97.1 (exact)\n"));
            }
            if spec.program == self.rustc {
                return Ok(success_record(spec, "rustc 1.97.1 (exact)\n"));
            }
            if spec.program == self.rustdoc {
                return Ok(success_record(spec, "rustdoc 1.97.1 (exact)\n"));
            }
            assert_eq!(timeout, expected_git_timeout(spec));
            let response = self.git.borrow_mut().pop_front().expect("Git response");
            Ok(record(spec, response))
        }
    }

    impl TimedWorkingDirectoryCommandExecutor for ScriptedExecutor {
        fn execute_in_directory_with_timeout(
            &self,
            spec: &CommandSpec,
            _working_directory: &Path,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            self.working_calls.set(self.working_calls.get() + 1);
            if spec.program == self.cargo {
                assert_eq!(
                    timeout,
                    crate::local_install_build_command::LOCAL_INSTALL_BUILD_TIMEOUT
                );
                let argv = spec.displayed_argv();
                assert!(argv.iter().any(|argument| argument == "--offline"));
                assert!(argv.windows(2).any(|pair| pair == ["--bin", "glaeda"]));
                for forbidden in [
                    "RUSTFLAGS",
                    "RUSTC_WRAPPER",
                    "RUSTC_WORKSPACE_WRAPPER",
                    "CARGO_BUILD_RUSTC_WRAPPER",
                ] {
                    assert!(!spec.environment.contains_key(forbidden));
                }
                return match self.build_behavior {
                    BuildBehavior::Succeed | BuildBehavior::ReplaceArtifactOnVersion => {
                        fs::create_dir_all(self.artifact.parent().expect("release parent"))?;
                        write_fake_binary(&self.artifact);
                        Ok(success_record(spec, ""))
                    }
                    BuildBehavior::ChangeSource => {
                        fs::create_dir_all(self.artifact.parent().expect("release parent"))?;
                        write_fake_binary(&self.artifact);
                        fs::write(&self.source_lock, b"changed during build\n")?;
                        Ok(success_record(spec, ""))
                    }
                    BuildBehavior::Fail => Ok(ExecutionRecord {
                        argv: spec.displayed_argv(),
                        environment_keys: spec.environment.keys().cloned().collect(),
                        status: Some(101),
                        success: false,
                        stdout: String::new(),
                        stderr: PRIVATE_SENTINEL.to_owned(),
                    }),
                    BuildBehavior::Timeout => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "scripted build timeout",
                    )),
                    BuildBehavior::OutputLimit => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "scripted build output limit",
                    )),
                    BuildBehavior::DependencyMissing => Ok(ExecutionRecord {
                        argv: spec.displayed_argv(),
                        environment_keys: spec.environment.keys().cloned().collect(),
                        status: Some(101),
                        success: false,
                        stdout: String::new(),
                        stderr: "error: no matching package named `missing` found\nlocation searched: crates.io index\n"
                            .to_owned(),
                    }),
                };
            }
            assert_eq!(spec.program, self.artifact);
            assert_eq!(timeout, LOCAL_INSTALL_ARTIFACT_VERSION_TIMEOUT);
            if matches!(self.build_behavior, BuildBehavior::ReplaceArtifactOnVersion) {
                let replacement = self.artifact.with_extension("replacement");
                write_fake_binary(&replacement);
                fs::rename(replacement, &self.artifact)?;
            }
            Ok(success_record(spec, "glaeda 0.1.0\n"))
        }
    }

    #[test]
    fn exact_build_emits_bound_artifact_evidence_without_private_material() {
        let fixture = Fixture::new("success");
        let executor = ScriptedExecutor::new(&fixture, 2, BuildBehavior::Succeed);
        let receipt = execute_local_install_build(
            &fixture.plan(),
            LocalInstallPlatform::Linux,
            &fixture.context(),
            4,
            &ProjectCheckoutObserver::new("/usr/bin/git").expect("observer"),
            &executor,
        )
        .expect("execution receipt");

        assert_eq!(
            receipt.outcome(),
            LocalInstallBuildExecutionOutcome::Succeeded,
            "{receipt:?}"
        );
        assert!(receipt.codes().is_empty());
        let evidence = receipt.evidence().expect("built evidence");
        assert_eq!(evidence.binary_version, "glaeda 0.1.0");
        assert_eq!(evidence.binary_digest, digest_bytes(fake_binary_bytes()));
        assert_eq!(executor.working_calls.get(), 2);
        assert!(executor.git.borrow().is_empty());
        let public = serde_json::to_string(&receipt).expect("receipt JSON");
        assert!(!public.contains(fixture.root.to_string_lossy().as_ref()));
        assert!(!public.contains("Cargo.toml"));
    }

    #[test]
    fn compiler_failure_is_bounded_and_never_serializes_raw_output() {
        let fixture = Fixture::new("compiler-failure");
        let executor = ScriptedExecutor::new(&fixture, 1, BuildBehavior::Fail);
        let receipt = execute_local_install_build(
            &fixture.plan(),
            LocalInstallPlatform::Linux,
            &fixture.context(),
            4,
            &ProjectCheckoutObserver::new("/usr/bin/git").expect("observer"),
            &executor,
        )
        .expect("execution receipt");

        assert_eq!(
            receipt.outcome(),
            LocalInstallBuildExecutionOutcome::Failed,
            "{receipt:?}"
        );
        assert_eq!(
            receipt.codes(),
            [LocalInstallBuildExecutionCode::BuildFailed]
        );
        assert!(receipt.evidence().is_none());
        assert_eq!(executor.working_calls.get(), 1);
        let public = serde_json::to_string(&receipt).expect("receipt JSON");
        assert!(!public.contains(PRIVATE_SENTINEL));
        assert!(!format!("{receipt:?}").contains(PRIVATE_SENTINEL));
    }

    #[test]
    fn timeout_output_limit_and_offline_dependency_miss_remain_distinct() {
        for (label, behavior, expected) in [
            (
                "timeout",
                BuildBehavior::Timeout,
                LocalInstallBuildExecutionCode::BuildTimeout,
            ),
            (
                "output-limit",
                BuildBehavior::OutputLimit,
                LocalInstallBuildExecutionCode::BuildOutputLimit,
            ),
            (
                "dependency-missing",
                BuildBehavior::DependencyMissing,
                LocalInstallBuildExecutionCode::DependencyMaterialMissing,
            ),
        ] {
            let fixture = Fixture::new(label);
            let executor = ScriptedExecutor::new(&fixture, 1, behavior);
            let receipt = execute_local_install_build(
                &fixture.plan(),
                LocalInstallPlatform::Linux,
                &fixture.context(),
                4,
                &ProjectCheckoutObserver::new("/usr/bin/git").expect("observer"),
                &executor,
            )
            .expect("execution receipt");

            assert_eq!(receipt.outcome(), LocalInstallBuildExecutionOutcome::Failed);
            assert_eq!(receipt.codes(), [expected]);
            assert!(receipt.evidence().is_none());
            assert_eq!(executor.working_calls.get(), 1);
        }
    }

    #[test]
    fn source_change_during_build_refuses_artifact_acceptance() {
        let fixture = Fixture::new("source-change");
        let executor = ScriptedExecutor::new(&fixture, 2, BuildBehavior::ChangeSource);
        let receipt = execute_local_install_build(
            &fixture.plan(),
            LocalInstallPlatform::Linux,
            &fixture.context(),
            4,
            &ProjectCheckoutObserver::new("/usr/bin/git").expect("observer"),
            &executor,
        )
        .expect("execution receipt");

        assert_eq!(receipt.outcome(), LocalInstallBuildExecutionOutcome::Failed);
        assert_eq!(
            receipt.codes(),
            [LocalInstallBuildExecutionCode::SourceChanged]
        );
        assert!(receipt.source_after().is_some());
        assert!(receipt.evidence().is_none());
        assert_eq!(executor.working_calls.get(), 1);
    }

    #[test]
    fn artifact_replacement_during_version_probe_is_changed() {
        let fixture = Fixture::new("artifact-replacement");
        let executor = ScriptedExecutor::new(&fixture, 2, BuildBehavior::ReplaceArtifactOnVersion);
        let receipt = execute_local_install_build(
            &fixture.plan(),
            LocalInstallPlatform::Linux,
            &fixture.context(),
            4,
            &ProjectCheckoutObserver::new("/usr/bin/git").expect("observer"),
            &executor,
        )
        .expect("execution receipt");

        assert_eq!(receipt.outcome(), LocalInstallBuildExecutionOutcome::Failed);
        assert_eq!(
            receipt.codes(),
            [LocalInstallBuildExecutionCode::ArtifactChanged]
        );
        assert!(receipt.evidence().is_none());
        assert_eq!(executor.working_calls.get(), 2);
    }

    #[test]
    fn invalid_directory_preflight_refuses_before_build_execution() {
        let fixture = Fixture::new("directory-refusal");
        fs::remove_dir(fixture.build.join("work")).expect("remove work");
        let executor = ScriptedExecutor::new(&fixture, 1, BuildBehavior::Succeed);
        let receipt = execute_local_install_build(
            &fixture.plan(),
            LocalInstallPlatform::Linux,
            &fixture.context(),
            4,
            &ProjectCheckoutObserver::new("/usr/bin/git").expect("observer"),
            &executor,
        )
        .expect("execution receipt");

        assert_eq!(
            receipt.outcome(),
            LocalInstallBuildExecutionOutcome::Refused
        );
        assert!(
            receipt
                .codes()
                .contains(&LocalInstallBuildExecutionCode::DirectoryNotReady)
        );
        assert_eq!(executor.working_calls.get(), 0);
    }

    #[test]
    fn artifact_snapshot_rejects_scripts_and_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("unsafe-artifact");
        fs::create_dir_all(fixture.artifact.parent().expect("release parent"))
            .expect("release directory");
        fs::write(&fixture.artifact, b"#!/bin/sh\nexit 0\n").expect("script artifact");
        set_mode(&fixture.artifact, 0o755);
        assert_eq!(
            artifact_snapshot(&fixture.build, &fixture.artifact).expect_err("script rejected"),
            ArtifactObservationError::Unsafe
        );
        fs::remove_file(&fixture.artifact).expect("remove script");
        let real = fixture.artifact.with_extension("real");
        write_fake_binary(&real);
        symlink("glaeda.real", &fixture.artifact).expect("artifact symlink");
        assert_eq!(
            artifact_snapshot(&fixture.build, &fixture.artifact).expect_err("symlink rejected"),
            ArtifactObservationError::Unsafe
        );
    }

    #[test]
    fn artifact_snapshot_accepts_cargo_pair_but_rejects_extra_hard_links() {
        let fixture = Fixture::new("artifact-links");
        fs::create_dir_all(fixture.artifact.parent().expect("release parent"))
            .expect("release directory");
        write_fake_binary(&fixture.artifact);
        let cargo_deps_link = fixture.artifact.with_extension("cargo-deps-link");
        fs::hard_link(&fixture.artifact, &cargo_deps_link).expect("Cargo hard link");
        artifact_snapshot(&fixture.build, &fixture.artifact).expect("Cargo link pair accepted");

        let foreign_shape = fixture.artifact.with_extension("third-link");
        fs::hard_link(&fixture.artifact, &foreign_shape).expect("third hard link");
        assert_eq!(
            artifact_snapshot(&fixture.build, &fixture.artifact)
                .expect_err("extra hard link rejected"),
            ArtifactObservationError::Unsafe
        );
    }

    fn git_observation(source: &Path) -> Vec<Response> {
        let snapshot = || {
            vec![
                Response::success(format!("{COMMIT}\n")),
                Response::success(format!("{TREE}\n")),
                Response::success(
                    "remote.origin.url\nhttps://github.com/teamleaderleo/glaeda.git\0",
                ),
                Response::failed(1, ""),
                Response::success(format!("# branch.oid {COMMIT}\0# branch.head main\0")),
                Response::success("100644\n"),
                Response::success(format!(
                    "worktree {}\0HEAD {COMMIT}\0branch refs/heads/main\0\0",
                    source.display()
                )),
            ]
        };
        let mut responses = vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", source.display())),
        ];
        responses.extend(snapshot());
        responses.extend(snapshot());
        responses
    }

    fn success_record(spec: &CommandSpec, stdout: &str) -> ExecutionRecord {
        record(spec, Response::success(stdout))
    }

    fn record(spec: &CommandSpec, response: Response) -> ExecutionRecord {
        ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(response.status),
            success: response.status == 0,
            stdout: response.stdout,
            stderr: response.stderr,
        }
    }

    fn write_fake_binary(path: &Path) {
        fs::write(path, fake_binary_bytes()).expect("write fake binary");
        set_mode(path, 0o755);
    }

    fn fake_binary_bytes() -> &'static [u8] {
        if cfg!(target_os = "linux") {
            b"\x7fELFfake-glaeda-binary".as_slice()
        } else {
            b"\xfe\xed\xfa\xcffake-glaeda-binary".as_slice()
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
    }

    fn digest_bytes(bytes: &[u8]) -> Sha256Digest {
        let value = format!("sha256:{:x}", Sha256::digest(bytes));
        Sha256Digest::parse(&value).expect("digest")
    }

    #[test]
    fn production_executor_still_implements_both_required_bounds() {
        fn assert_executor<T: TimedCommandExecutor + TimedWorkingDirectoryCommandExecutor>() {}
        assert_executor::<ProcessExecutor>();
    }
}
