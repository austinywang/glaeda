use std::fmt;

use serde::{Serialize, Serializer};
use sha2::{Digest as _, Sha256};

use crate::actions_runner_readiness::ActionsRunnerReadinessState;
use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::execution_admission::{
    EpochMillis, ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    ReservationGeneration, ReservationId, RunnerProfileId,
};
use crate::lima_lifecycle::LimaResourceProfile;
use crate::operator_config::{OperatorConfig, OperatorConfigIdentity};
use crate::personal_worker_operator_read::{
    PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION, PersonalWorkerOperatorJobRead,
};
use crate::personal_worker_queue::{
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerCancellationState,
    PersonalWorkerQueueGeneration,
};
use crate::personal_worker_read_model::PersonalWorkerJobStateView;
use crate::personal_worker_runner_readiness::{
    PERSONAL_WORKER_RUNNER_READINESS_SCHEMA_VERSION, PersonalWorkerRunnerReadinessDisposition,
    PersonalWorkerRunnerReadinessObservation, PersonalWorkerRunnerReadinessReason,
};
use crate::personal_worker_store::PersonalWorkerStoreRevision;
use crate::repository_source_observation::{
    REPOSITORY_SOURCE_OBSERVATION_SCHEMA_VERSION, RepositoryCleanliness,
    RepositorySourceObservation,
};
use crate::rust_verification_envelope::{
    RustCacheIdentityClass, RustVerificationEnvelope, RustVerificationSourceIdentity,
};
use crate::rust_verification_envelope_digest::digest_rust_verification_envelope;
use crate::trusted_workspace_receipt::TrustedWorkspaceCacheReceipt;
use crate::verification_cache_generation::{
    VerificationCacheIdentityGeneration, verification_cache_identity_generation,
};
use crate::verification_profile::{
    CacheId, ExactBuildScope, ExactVerificationScope, RepositoryCommandIdentity,
    VerificationProfileId,
};
use crate::verification_profile_registry::smolrunner_profile_registry;

pub const PERSONAL_WORKER_VERIFICATION_PLAN_SCHEMA_VERSION: u8 = 1;
pub const MAX_VERIFICATION_STDOUT_BYTES: u64 = 1_048_576;
pub const MAX_VERIFICATION_STDERR_BYTES: u64 = 1_048_576;

const PERSONAL_LIMA_WORK_RUNNER_PROFILE: &str = "personal-lima-work";
const REDACTED_PRIVATE_PLAN_EVIDENCE: &str = "<private-verification-plan-evidence>";
const SMOLRUNNER_SOURCE_COMMAND_CACHE_DOMAIN: &[u8] =
    b"smolrunner-verification-source-command-cache-v1";
const SMOLRUNNER_ENVELOPE_CACHE_DOMAIN: &[u8] = b"smolrunner-verification-envelope-cache-v1";
const GLAEDA_SOURCE_COMMAND_CACHE_DOMAIN: &[u8] = b"glaeda-verification-source-command-cache-v2";
const GLAEDA_ENVELOPE_CACHE_DOMAIN: &[u8] = b"glaeda-verification-envelope-cache-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationWorkspaceMountPolicy {
    ReadOnlyExactObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCacheScopePolicy {
    ExactSourceCommandEnvelopeAndRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCredentialPolicy {
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationNetworkPolicy {
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationEnvironmentPolicy {
    EmptyThenFixedAllowlist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationContainerPolicy {
    RootlessDisposableDigestBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationRootFilesystemPolicy {
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPrivilegePolicy {
    DropAllCapabilitiesAndDenyEscalation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationExecutionGroupPolicy {
    DedicatedCgroupV2ProveEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDetachedProcessPolicy {
    Forbidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VerificationRuntimeRequirements {
    pub container: VerificationContainerPolicy,
    pub root_filesystem: VerificationRootFilesystemPolicy,
    pub workspace_mount: VerificationWorkspaceMountPolicy,
    pub cache_scope: VerificationCacheScopePolicy,
    pub privilege: VerificationPrivilegePolicy,
    pub network: VerificationNetworkPolicy,
    pub credentials: VerificationCredentialPolicy,
    pub environment: VerificationEnvironmentPolicy,
    pub execution_group: VerificationExecutionGroupPolicy,
    pub detached_processes: VerificationDetachedProcessPolicy,
    pub revalidate_workspace_object_before_mutation: bool,
    pub bind_runtime_image_and_toolchain_to_cache: bool,
}

impl VerificationRuntimeRequirements {
    const REQUIRED: Self = Self {
        container: VerificationContainerPolicy::RootlessDisposableDigestBound,
        root_filesystem: VerificationRootFilesystemPolicy::ReadOnly,
        workspace_mount: VerificationWorkspaceMountPolicy::ReadOnlyExactObject,
        cache_scope: VerificationCacheScopePolicy::ExactSourceCommandEnvelopeAndRuntime,
        privilege: VerificationPrivilegePolicy::DropAllCapabilitiesAndDenyEscalation,
        network: VerificationNetworkPolicy::Denied,
        credentials: VerificationCredentialPolicy::Absent,
        environment: VerificationEnvironmentPolicy::EmptyThenFixedAllowlist,
        execution_group: VerificationExecutionGroupPolicy::DedicatedCgroupV2ProveEmpty,
        detached_processes: VerificationDetachedProcessPolicy::Forbidden,
        revalidate_workspace_object_before_mutation: true,
        bind_runtime_image_and_toolchain_to_cache: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VerificationOutputLimits {
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationCommandBinding {
    pub identity: RepositoryCommandIdentity,
    pub test_scope: ExactVerificationScope,
    pub build_scope: ExactBuildScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationCacheBinding {
    pub generation: VerificationCacheIdentityGeneration,
    pub installation_id: crate::verification_profile::RunnerInstallationId,
    pub workspace_id: crate::verification_profile::RunnerWorkspaceId,
    pub cache_id: CacheId,
    pub protected_namespace_digest: Sha256Digest,
    pub source_command_envelope_namespace_digest: Sha256Digest,
    pub access: PersonalWorkerCacheAccessMode,
    pub reservation_id: ReservationId,
    pub reservation_generation: ReservationGeneration,
    pub acquired_at: EpochMillis,
    pub scope_policy: VerificationCacheScopePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerVerificationPlanReport {
    schema_version: u8,
    config_identity: OperatorConfigIdentity,
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    request_id: ExecutionRequestId,
    repository: RepositoryRef,
    commit: CommitId,
    tree: GitTreeId,
    verification_profile_id: VerificationProfileId,
    runner_profile_id: RunnerProfileId,
    lima_profile: LimaResourceProfile,
    expected_runner_identity: crate::actions_runner_readiness::ActionsRunnerConfiguredIdentity,
    command: VerificationCommandBinding,
    rust_envelope: RustVerificationEnvelope,
    rust_envelope_digest: Sha256Digest,
    requested_limits: ExecutionResourceLimits,
    applied_limits: ExecutionResourceLimits,
    cache: VerificationCacheBinding,
    planned_at: EpochMillis,
    not_after: EpochMillis,
    timeout_seconds: u64,
    output_limits: VerificationOutputLimits,
    runtime_requirements: VerificationRuntimeRequirements,
    trusted_workspace_evidence_digest: Sha256Digest,
}

impl PersonalWorkerVerificationPlanReport {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn request_id(&self) -> &ExecutionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn command(&self) -> &VerificationCommandBinding {
        &self.command
    }

    #[must_use]
    pub const fn rust_envelope(&self) -> &RustVerificationEnvelope {
        &self.rust_envelope
    }

    #[must_use]
    pub const fn rust_envelope_digest(&self) -> &Sha256Digest {
        &self.rust_envelope_digest
    }

    #[must_use]
    pub const fn cache(&self) -> &VerificationCacheBinding {
        &self.cache
    }

    #[must_use]
    pub const fn applied_limits(&self) -> ExecutionResourceLimits {
        self.applied_limits
    }

    #[must_use]
    pub const fn not_after(&self) -> EpochMillis {
        self.not_after
    }

    #[must_use]
    pub const fn runtime_requirements(&self) -> VerificationRuntimeRequirements {
        self.runtime_requirements
    }
}

pub struct PersonalWorkerVerificationPlan {
    report: PersonalWorkerVerificationPlanReport,
    workspace_receipt: TrustedWorkspaceCacheReceipt,
}

impl PersonalWorkerVerificationPlan {
    #[must_use]
    pub const fn report(&self) -> &PersonalWorkerVerificationPlanReport {
        &self.report
    }

    #[must_use]
    pub const fn workspace_receipt(&self) -> &TrustedWorkspaceCacheReceipt {
        &self.workspace_receipt
    }
}

impl Serialize for PersonalWorkerVerificationPlan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.report.serialize(serializer)
    }
}

impl fmt::Debug for PersonalWorkerVerificationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerVerificationPlan")
            .field("report", &self.report)
            .field("workspace_receipt", &REDACTED_PRIVATE_PLAN_EVIDENCE)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerVerificationPlanErrorKind {
    ConfigurationMismatch,
    SnapshotMismatch,
    RunnerNotReady,
    JobNotReserved,
    SourceMismatch,
    WorkspaceMismatch,
    ProfileMismatch,
    ResourceMismatch,
    CacheMismatch,
    DeadlineExpired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerVerificationPlanError {
    kind: PersonalWorkerVerificationPlanErrorKind,
    code: &'static str,
    message: &'static str,
}

impl PersonalWorkerVerificationPlanError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerVerificationPlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    const fn new(
        kind: PersonalWorkerVerificationPlanErrorKind,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            code,
            message,
        }
    }
}

impl fmt::Display for PersonalWorkerVerificationPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerVerificationPlanError {}

/// Build one immutable, non-executing verification authorization plan from exact sealed evidence.
///
/// The returned plan grants no subprocess, filesystem, container, cgroup, credential, network,
/// cache-path, or mutation authority. A later executor must freshly satisfy every runtime
/// requirement and re-open the exact workspace object before its first mutation.
///
/// # Errors
///
/// Returns one bounded error when configuration, snapshot, runner, source, workspace, profile,
/// resource, cache, or deadline evidence does not bind exactly.
pub fn plan_personal_worker_verification(
    config: &OperatorConfig,
    job: &PersonalWorkerOperatorJobRead,
    readiness: &PersonalWorkerRunnerReadinessObservation,
    source: &RepositorySourceObservation,
    workspace_receipt: TrustedWorkspaceCacheReceipt,
    planned_at: EpochMillis,
) -> Result<PersonalWorkerVerificationPlan, PersonalWorkerVerificationPlanError> {
    let runner = readiness.report();
    if job.schema_version() != PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION
        || runner.schema_version != PERSONAL_WORKER_RUNNER_READINESS_SCHEMA_VERSION
        || source.schema_version() != REPOSITORY_SOURCE_OBSERVATION_SCHEMA_VERSION
    {
        return Err(configuration_mismatch());
    }
    if job.config_identity() != config.identity()
        || runner.config_identity != *config.identity()
        || runner.instance != *config.lima_instance()
    {
        return Err(configuration_mismatch());
    }
    if runner.store_revision != job.view().store_revision()
        || runner.queue_generation != job.view().queue_generation()
    {
        return Err(snapshot_mismatch());
    }
    let Some(runner_active) = runner.active.as_ref() else {
        return Err(runner_not_ready());
    };
    let Some(runner_report) = runner.runner.as_ref() else {
        return Err(runner_not_ready());
    };
    if runner.disposition != PersonalWorkerRunnerReadinessDisposition::Ready
        || runner.reason != PersonalWorkerRunnerReadinessReason::ReservedJobReady
        || runner.lima_profile != LimaResourceProfile::Work
        || runner_report.state != ActionsRunnerReadinessState::IdleReady
        || runner_active.admission_state != ExecutionAdmissionState::Reserved
    {
        return Err(runner_not_ready());
    }
    let runner_expiry_millis = runner_report
        .timing
        .expires_at_unix_seconds
        .checked_mul(1_000)
        .ok_or_else(runner_not_ready)?;
    if planned_at.get() >= runner_expiry_millis {
        return Err(runner_not_ready());
    }
    let runner_observed_millis = runner_report
        .timing
        .observed_at_unix_seconds
        .checked_mul(1_000)
        .ok_or_else(runner_not_ready)?;
    if planned_at.get() < runner_observed_millis {
        return Err(runner_not_ready());
    }

    let PersonalWorkerJobStateView::Active {
        entry,
        admission,
        durable_cache_lease,
    } = job.view().state()
    else {
        return Err(job_not_reserved());
    };
    if admission.state() != ExecutionAdmissionState::Reserved
        || runner_active.request_id != entry.request_id
        || job.view().cancellation() != PersonalWorkerCancellationState::Active
        || entry.runner_profile_id.as_str() != PERSONAL_LIMA_WORK_RUNNER_PROFILE
    {
        return Err(job_not_reserved());
    }
    if durable_cache_lease.reservation_id() != admission.reservation().id()
        || durable_cache_lease.reservation_generation() != admission.reservation().generation()
        || runner_active.request_id != entry.request_id
    {
        return Err(snapshot_mismatch());
    }
    if planned_at < job.view().observed_at()
        || planned_at < admission.observed_at()
        || planned_at < admission.reservation().reserved_at()
        || planned_at < durable_cache_lease.acquired_at()
    {
        return Err(snapshot_mismatch());
    }

    if source.cleanliness() != RepositoryCleanliness::Clean
        || source.verification_profile() != &entry.verification_profile_id
        || source.source().repository != entry.repository
        || source.source().commit != entry.commit
        || source.source().tree != entry.tree
    {
        return Err(source_mismatch());
    }
    if workspace_receipt.repository() != &entry.repository
        || workspace_receipt.workspace_location_identity() != source.workspace_location_identity()
    {
        return Err(workspace_mismatch());
    }

    let registry = smolrunner_profile_registry().map_err(|_| profile_mismatch())?;
    let profile = registry
        .lookup(&entry.verification_profile_id)
        .map_err(|_| profile_mismatch())?;
    if profile.canonical_command().identity().repository() != &entry.repository {
        return Err(profile_mismatch());
    }
    let cache_generation =
        verification_cache_identity_generation(profile).map_err(|_| profile_mismatch())?;
    if entry.requested_cpu_millis != admission.requested_limits().cpu_millis
        || entry.requested_memory_bytes != admission.requested_limits().memory_bytes
    {
        return Err(resource_mismatch());
    }

    let (cache_id, cache_repository, protected_namespace_digest) = match &entry.cache_namespace {
        PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id,
            repository,
            namespace_digest,
        } => (cache_id, repository, namespace_digest),
        PersonalWorkerCacheNamespace::SharedDownload { .. } => return Err(cache_mismatch()),
    };
    if cache_id != profile.cache_class().cache_id()
        || cache_id != workspace_receipt.cache_id()
        || cache_repository != &entry.repository
        || protected_namespace_digest != workspace_receipt.cache_namespace_digest()
        || entry.cache_access == PersonalWorkerCacheAccessMode::Read
    {
        return Err(cache_mismatch());
    }

    let source_command_namespace_digest = source_command_namespace_digest(
        cache_generation,
        protected_namespace_digest,
        source,
        profile.canonical_command().identity(),
    )?;
    let rust_envelope = registry
        .resolve_rust_envelope(
            &entry.verification_profile_id,
            RustVerificationSourceIdentity::new(
                source.source().repository.clone(),
                source.source().commit.clone(),
                source.source().tree.clone(),
            ),
            source_command_namespace_digest.clone(),
        )
        .map_err(|_| profile_mismatch())?;
    if rust_envelope.profile_id() != &entry.verification_profile_id
        || rust_envelope.command() != profile.canonical_command().identity()
        || rust_envelope.source().repository != entry.repository
        || rust_envelope.source().commit != entry.commit
        || rust_envelope.source().tree != entry.tree
    {
        return Err(profile_mismatch());
    }
    if rust_envelope.resources().required_worker_profile != LimaResourceProfile::Work
        || rust_envelope.resources().reserved_resources != admission.applied_limits()
        || !admission
            .applied_limits()
            .fits_within(admission.requested_limits())
    {
        return Err(resource_mismatch());
    }
    if rust_envelope.cache().identity_class != RustCacheIdentityClass::SourceScoped
        || rust_envelope.cache().cargo_target_directory.cache_id != *cache_id
        || rust_envelope
            .cache()
            .cargo_target_directory
            .namespace_digest
            != source_command_namespace_digest
    {
        return Err(cache_mismatch());
    }
    let envelope_capabilities = rust_envelope.required_capabilities();
    let mut registered_capabilities = profile
        .required_capabilities()
        .iter()
        .map(|required| required.capability.clone())
        .collect::<Vec<_>>();
    registered_capabilities.sort();
    if envelope_capabilities != registered_capabilities {
        return Err(profile_mismatch());
    }

    let not_after = effective_not_after(
        admission.reservation().expires_at(),
        job.view().operator_deadline(),
    );
    if planned_at >= not_after {
        return Err(deadline_expired());
    }
    let rust_envelope_digest =
        digest_rust_verification_envelope(&rust_envelope).map_err(|_| profile_mismatch())?;
    let source_command_envelope_namespace_digest = envelope_cache_namespace_digest(
        cache_generation,
        protected_namespace_digest,
        &source_command_namespace_digest,
        &rust_envelope_digest,
    )?;
    let command = profile.canonical_command();
    let report = PersonalWorkerVerificationPlanReport {
        schema_version: PERSONAL_WORKER_VERIFICATION_PLAN_SCHEMA_VERSION,
        config_identity: config.identity().clone(),
        store_revision: job.view().store_revision(),
        queue_generation: job.view().queue_generation(),
        request_id: entry.request_id.clone(),
        repository: entry.repository.clone(),
        commit: entry.commit.clone(),
        tree: entry.tree.clone(),
        verification_profile_id: entry.verification_profile_id.clone(),
        runner_profile_id: entry.runner_profile_id.clone(),
        lima_profile: runner.lima_profile,
        expected_runner_identity: runner.expected_runner_identity.clone(),
        command: VerificationCommandBinding {
            identity: command.identity().clone(),
            test_scope: command.test_scope().clone(),
            build_scope: command.build_scope().clone(),
        },
        rust_envelope,
        rust_envelope_digest,
        requested_limits: admission.requested_limits(),
        applied_limits: admission.applied_limits(),
        cache: VerificationCacheBinding {
            generation: cache_generation,
            installation_id: workspace_receipt.installation_id().clone(),
            workspace_id: workspace_receipt.workspace_id().clone(),
            cache_id: cache_id.clone(),
            protected_namespace_digest: protected_namespace_digest.clone(),
            source_command_envelope_namespace_digest,
            access: entry.cache_access,
            reservation_id: admission.reservation().id().clone(),
            reservation_generation: admission.reservation().generation(),
            acquired_at: durable_cache_lease.acquired_at(),
            scope_policy: VerificationCacheScopePolicy::ExactSourceCommandEnvelopeAndRuntime,
        },
        planned_at,
        not_after,
        timeout_seconds: profile.timeout().total_seconds(),
        output_limits: VerificationOutputLimits {
            stdout_bytes: MAX_VERIFICATION_STDOUT_BYTES,
            stderr_bytes: MAX_VERIFICATION_STDERR_BYTES,
        },
        runtime_requirements: VerificationRuntimeRequirements::REQUIRED,
        trusted_workspace_evidence_digest: workspace_receipt.trusted_evidence_digest().clone(),
    };
    Ok(PersonalWorkerVerificationPlan {
        report,
        workspace_receipt,
    })
}

const fn effective_not_after(
    reservation_expiry: EpochMillis,
    operator_deadline: Option<EpochMillis>,
) -> EpochMillis {
    match operator_deadline {
        Some(deadline) if deadline.get() < reservation_expiry.get() => deadline,
        Some(_) | None => reservation_expiry,
    }
}

const fn source_command_cache_domain(
    generation: VerificationCacheIdentityGeneration,
) -> &'static [u8] {
    match generation {
        VerificationCacheIdentityGeneration::SmolrunnerV1 => SMOLRUNNER_SOURCE_COMMAND_CACHE_DOMAIN,
        VerificationCacheIdentityGeneration::GlaedaV2 => GLAEDA_SOURCE_COMMAND_CACHE_DOMAIN,
    }
}

const fn envelope_cache_domain(generation: VerificationCacheIdentityGeneration) -> &'static [u8] {
    match generation {
        VerificationCacheIdentityGeneration::SmolrunnerV1 => SMOLRUNNER_ENVELOPE_CACHE_DOMAIN,
        VerificationCacheIdentityGeneration::GlaedaV2 => GLAEDA_ENVELOPE_CACHE_DOMAIN,
    }
}

fn source_command_namespace_digest(
    generation: VerificationCacheIdentityGeneration,
    protected_namespace: &Sha256Digest,
    source: &RepositorySourceObservation,
    command: &RepositoryCommandIdentity,
) -> Result<Sha256Digest, PersonalWorkerVerificationPlanError> {
    let command_identity = serde_json::to_vec(command).map_err(|_| cache_mismatch())?;
    digest_namespace_fields(
        source_command_cache_domain(generation),
        &[
            protected_namespace.as_str().as_bytes(),
            source.source().repository.as_str().as_bytes(),
            source.source().commit.as_str().as_bytes(),
            source.source().tree.as_str().as_bytes(),
            command_identity.as_slice(),
        ],
    )
}

fn envelope_cache_namespace_digest(
    generation: VerificationCacheIdentityGeneration,
    protected_namespace: &Sha256Digest,
    source_command_namespace: &Sha256Digest,
    rust_envelope_digest: &Sha256Digest,
) -> Result<Sha256Digest, PersonalWorkerVerificationPlanError> {
    digest_namespace_fields(
        envelope_cache_domain(generation),
        &[
            protected_namespace.as_str().as_bytes(),
            source_command_namespace.as_str().as_bytes(),
            rust_envelope_digest.as_str().as_bytes(),
        ],
    )
}

fn digest_namespace_fields(
    domain: &[u8],
    fields: &[&[u8]],
) -> Result<Sha256Digest, PersonalWorkerVerificationPlanError> {
    let mut digest = Sha256::new();
    digest.update(domain);
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    Sha256Digest::parse(&format!("sha256:{:x}", digest.finalize())).map_err(|_| cache_mismatch())
}

const fn configuration_mismatch() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::ConfigurationMismatch,
        "configuration_mismatch",
        "verification planning evidence does not match the exact operator configuration",
    )
}

const fn snapshot_mismatch() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::SnapshotMismatch,
        "snapshot_mismatch",
        "verification planning evidence does not describe one exact durable snapshot",
    )
}

const fn runner_not_ready() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::RunnerNotReady,
        "runner_not_ready",
        "the exact personal worker runner is not ready for the reserved job",
    )
}

const fn job_not_reserved() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::JobNotReserved,
        "job_not_reserved",
        "the exact job is not an active uncancelled work-profile reservation",
    )
}

const fn source_mismatch() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::SourceMismatch,
        "source_mismatch",
        "repository observation does not match the exact reserved source and profile",
    )
}

const fn workspace_mismatch() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::WorkspaceMismatch,
        "workspace_mismatch",
        "protected workspace evidence does not match the exact observed repository object",
    )
}

const fn profile_mismatch() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::ProfileMismatch,
        "profile_mismatch",
        "the reserved verification profile is not one exact checked-in command contract",
    )
}

const fn resource_mismatch() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::ResourceMismatch,
        "resource_mismatch",
        "applied worker resources do not satisfy the exact checked-in profile envelope",
    )
}

const fn cache_mismatch() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::CacheMismatch,
        "cache_mismatch",
        "durable job and protected workspace cache evidence do not match exactly",
    )
}

const fn deadline_expired() -> PersonalWorkerVerificationPlanError {
    PersonalWorkerVerificationPlanError::new(
        PersonalWorkerVerificationPlanErrorKind::DeadlineExpired,
        "deadline_expired",
        "the reservation or operator deadline has expired before verification planning",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::actions_runner_readiness::{
        ACTIONS_RUNNER_READINESS_SCHEMA_VERSION, ActionsRunnerConfiguredIdentity,
        ActionsRunnerName, ActionsRunnerReadinessReport,
    };
    use crate::lima_observation::{
        LIMA_OBSERVATION_SCHEMA_VERSION, LimaArchitecture, LimaConfiguredInstance,
        LimaFilesystemObjectIdentity, LimaGuestObservation, LimaGuestResources, LimaInstanceName,
        LimaInstanceObservationReport, LimaObservationFreshness, LimaObservationTiming,
        LimaObservedGuest, LimaPersistentIdentity, LimaRuntimeState, LimaVmType,
    };
    use crate::mac_availability::AvailabilityRequest;
    use crate::operator_config::{
        GuestWorkspacePath, OperatorIdlePolicy, OperatorOutputPreference,
        OperatorRemediationPreference, PersonalWorkerStateRoot,
    };
    use crate::personal_worker_operator_read::PersonalWorkerOperatorRead;
    use crate::personal_worker_queue::{
        PersonalWorkerCacheLeaseState, PersonalWorkerPriority, PersonalWorkerProfile,
        PersonalWorkerQueueEntryState, PersonalWorkerQueueVisibility, PersonalWorkerSourceIdentity,
    };
    use crate::personal_worker_read_model::PersonalWorkerJobView;
    use crate::personal_worker_runner_readiness::{
        PersonalWorkerRunnerActiveEvidence, PersonalWorkerRunnerReadinessReport,
    };
    use crate::repository_source_observation::RepositoryWorkspaceLocationIdentity;
    use crate::verification_profile::{RunnerInstallationId, RunnerWorkspaceId};
    use crate::verification_profile_registry::{
        GLAEDA_DOCTOR_PROFILE_ID, GLAEDA_PLAN_PROFILE_ID, GLAEDA_REQUIRED_PROFILE_ID,
        SMOLRUNNER_DOCTOR_PROFILE_ID, SMOLRUNNER_PLAN_PROFILE_ID, SMOLRUNNER_REQUIRED_PROFILE_ID,
    };

    const GIB: u64 = 1_024 * 1_024 * 1_024;

    fn time(value: u64) -> EpochMillis {
        EpochMillis::new(value).expect("time")
    }

    fn limits(cpu_millis: u32, memory_bytes: u64) -> ExecutionResourceLimits {
        ExecutionResourceLimits::new(cpu_millis, memory_bytes, 2_048).expect("limits")
    }

    fn digest(hex: &str) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.repeat(64))).expect("digest")
    }

    fn config(profile_id: &str) -> OperatorConfig {
        OperatorConfig::new(
            PersonalWorkerStateRoot::parse("/private/test-state").expect("state root"),
            LimaInstanceName::parse("smolrunner").expect("instance"),
            GuestWorkspacePath::parse("/home/runner/workspace").expect("workspace"),
            VerificationProfileId::parse(profile_id).expect("profile"),
            AvailabilityRequest::Auto,
            OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
            OperatorOutputPreference::Json,
            OperatorRemediationPreference::CodesOnly,
        )
        .expect("config")
    }

    struct PlanningFixture {
        config: OperatorConfig,
        job: PersonalWorkerOperatorJobRead,
        readiness: PersonalWorkerRunnerReadinessObservation,
        source: RepositorySourceObservation,
        receipt: TrustedWorkspaceCacheReceipt,
        planned_at: EpochMillis,
    }

    impl PlanningFixture {
        fn plan(
            self,
        ) -> Result<PersonalWorkerVerificationPlan, PersonalWorkerVerificationPlanError> {
            plan_personal_worker_verification(
                &self.config,
                &self.job,
                &self.readiness,
                &self.source,
                self.receipt,
                self.planned_at,
            )
        }
    }

    fn planning_fixture(receipt_inode: u64) -> PlanningFixture {
        planning_fixture_with(receipt_inode, time(1_000_000), limits(4_000, 4 * GIB))
    }

    fn planning_fixture_with_job_observed(
        receipt_inode: u64,
        job_observed_at: EpochMillis,
    ) -> PlanningFixture {
        planning_fixture_with(receipt_inode, job_observed_at, limits(4_000, 4 * GIB))
    }

    fn planning_fixture_with(
        receipt_inode: u64,
        job_observed_at: EpochMillis,
        applied: ExecutionResourceLimits,
    ) -> PlanningFixture {
        planning_fixture_for(
            receipt_inode,
            job_observed_at,
            applied,
            "teamleaderleo/smolrunner",
            SMOLRUNNER_REQUIRED_PROFILE_ID,
        )
    }

    fn planning_fixture_for(
        receipt_inode: u64,
        job_observed_at: EpochMillis,
        applied: ExecutionResourceLimits,
        repository: &str,
        profile_id: &str,
    ) -> PlanningFixture {
        let config = config(profile_id);
        let repository = RepositoryRef::parse(repository).expect("repository");
        let commit = CommitId::parse(&"1".repeat(40)).expect("commit");
        let tree = GitTreeId::parse(&"2".repeat(40)).expect("tree");
        let profile_id = VerificationProfileId::parse(profile_id).expect("profile");
        let request_id = ExecutionRequestId::parse("job-one").expect("request ID");
        let runner_profile_id =
            RunnerProfileId::parse(PERSONAL_LIMA_WORK_RUNNER_PROFILE).expect("runner profile");
        let cache_id = CacheId::parse("cargo-target").expect("cache ID");
        let protected_namespace = digest("a");
        let requested = limits(4_000, 4 * GIB);
        let reservation_id = ReservationId::parse("reservation-one").expect("reservation ID");
        let reservation_generation = ReservationGeneration::new(1).expect("reservation generation");
        let revision = PersonalWorkerStoreRevision::new(7).expect("revision");
        let generation = PersonalWorkerQueueGeneration::new(9).expect("generation");
        let entry = PersonalWorkerQueueVisibility {
            request_id: request_id.clone(),
            repository: repository.clone(),
            commit: commit.clone(),
            tree: tree.clone(),
            verification_profile_id: profile_id.clone(),
            runner_profile_id,
            priority: PersonalWorkerPriority::Normal,
            effective_priority_rank: 1,
            age_millis: 100_000,
            state: PersonalWorkerQueueEntryState::Reserved,
            queue_position: None,
            requested_cpu_millis: requested.cpu_millis,
            requested_memory_bytes: requested.memory_bytes,
            reserved_cpu_millis: Some(applied.cpu_millis),
            reserved_memory_bytes: Some(applied.memory_bytes),
            cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
                cache_id: cache_id.clone(),
                repository: repository.clone(),
                namespace_digest: protected_namespace.clone(),
            },
            cache_access: PersonalWorkerCacheAccessMode::Write,
            cache_lease: PersonalWorkerCacheLeaseState::HeldWrite,
            start_time: None,
            worker_profile: PersonalWorkerProfile::Work,
        };
        let job_view = PersonalWorkerJobView::active_for_verification_plan_test(
            revision,
            generation,
            job_observed_at,
            time(800_000),
            Some(time(1_050_000)),
            PersonalWorkerCancellationState::Active,
            entry,
            ExecutionAdmissionState::Reserved,
            time(1_000_000),
            requested,
            applied,
            reservation_id.clone(),
            reservation_generation,
            time(900_000),
            time(1_100_000),
            time(900_000),
        );
        let job = PersonalWorkerOperatorRead::for_verification_plan_test(
            config.identity().clone(),
            job_view,
        );
        let runner_name = ActionsRunnerName::parse("smolrunner-macbook").expect("runner name");
        let expected_runner_identity = ActionsRunnerConfiguredIdentity {
            runner_name: runner_name.clone(),
            configuration_digest: digest("b"),
            runner_root: LimaFilesystemObjectIdentity {
                device_id: 2_049,
                inode: 500,
            },
        };
        let timing = LimaObservationTiming {
            started_at_unix_seconds: 999,
            observed_at_unix_seconds: 1_000,
            expires_at_unix_seconds: 1_020,
            duration_seconds: 1,
            freshness: LimaObservationFreshness::Fresh,
        };
        let lima = LimaInstanceObservationReport {
            schema_version: LIMA_OBSERVATION_SCHEMA_VERSION,
            instance: config.lima_instance().clone(),
            configured: LimaConfiguredInstance {
                runtime_state: LimaRuntimeState::Running,
                vm_type: LimaVmType::Vz,
                architecture: LimaArchitecture::Aarch64,
                cpus: 8,
                memory_bytes: 10 * GIB,
                primary_disk_bytes: 100 * GIB,
            },
            guest: LimaGuestObservation::Observed(LimaObservedGuest {
                resources: LimaGuestResources {
                    architecture: LimaArchitecture::Aarch64,
                    cpus: 8,
                    memory_bytes: 10 * GIB,
                },
                persistent_identity: LimaPersistentIdentity {
                    guest_machine_id_digest: digest("c"),
                    root_filesystem: LimaFilesystemObjectIdentity {
                        device_id: 2_049,
                        inode: 600,
                    },
                    cache_directory: LimaFilesystemObjectIdentity {
                        device_id: 2_049,
                        inode: 700,
                    },
                },
            }),
            timing: timing.clone(),
        };
        let runner = ActionsRunnerReadinessReport {
            schema_version: ACTIONS_RUNNER_READINESS_SCHEMA_VERSION,
            instance: config.lima_instance().clone(),
            runner_name,
            state: ActionsRunnerReadinessState::IdleReady,
            configured_identity: Some(expected_runner_identity.clone()),
            timing,
        };
        let readiness =
            PersonalWorkerRunnerReadinessObservation::from_report_for_verification_plan_test(
                PersonalWorkerRunnerReadinessReport {
                    schema_version: PERSONAL_WORKER_RUNNER_READINESS_SCHEMA_VERSION,
                    config_identity: config.identity().clone(),
                    store_revision: revision,
                    queue_generation: generation,
                    instance: config.lima_instance().clone(),
                    lima_profile: LimaResourceProfile::Work,
                    expected_runner_identity,
                    disposition: PersonalWorkerRunnerReadinessDisposition::Ready,
                    reason: PersonalWorkerRunnerReadinessReason::ReservedJobReady,
                    lima,
                    runner: Some(runner),
                    active: Some(PersonalWorkerRunnerActiveEvidence {
                        request_id,
                        admission_state: ExecutionAdmissionState::Reserved,
                    }),
                },
            );
        let source_location = RepositoryWorkspaceLocationIdentity::from_validated(
            PathBuf::from("/private/test-workspace"),
            10,
            20,
        );
        let source = RepositorySourceObservation::for_verification_plan_test(
            PersonalWorkerSourceIdentity::new(repository.clone(), commit, tree),
            profile_id,
            source_location,
        );
        let receipt_location = RepositoryWorkspaceLocationIdentity::from_validated(
            PathBuf::from("/private/test-workspace"),
            10,
            receipt_inode,
        );
        let receipt = TrustedWorkspaceCacheReceipt::for_verification_plan_test(
            RunnerInstallationId::parse("installation-one").expect("installation ID"),
            RunnerWorkspaceId::parse("workspace-one").expect("workspace ID"),
            repository,
            cache_id,
            protected_namespace,
            digest("d"),
            receipt_location,
        );
        PlanningFixture {
            config,
            job,
            readiness,
            source,
            receipt,
            planned_at: time(1_010_000),
        }
    }

    #[test]
    fn sealed_evidence_builds_one_bounded_non_executing_plan() {
        let plan = planning_fixture(20).plan().expect("verification plan");
        let report = plan.report();
        assert_eq!(
            report.schema_version(),
            PERSONAL_WORKER_VERIFICATION_PLAN_SCHEMA_VERSION
        );
        assert_eq!(report.request_id().as_str(), "job-one");
        assert_eq!(report.applied_limits(), limits(4_000, 4 * GIB));
        assert_eq!(report.not_after(), time(1_050_000));
        assert_eq!(
            report.command().identity.repository().as_str(),
            "teamleaderleo/smolrunner"
        );
        assert_eq!(
            report.cache().generation,
            VerificationCacheIdentityGeneration::SmolrunnerV1
        );
        assert_ne!(
            report.cache().source_command_envelope_namespace_digest,
            report.cache().protected_namespace_digest
        );
        assert_eq!(
            report.rust_envelope().resources().reserved_resources,
            limits(4_000, 4 * GIB)
        );
        assert_eq!(
            report
                .rust_envelope()
                .required_capabilities()
                .iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>(),
            ["cargo", "clippy", "rustc", "rustfmt"]
        );
        assert_eq!(
            digest_rust_verification_envelope(report.rust_envelope())
                .expect("canonical envelope digest"),
            *report.rust_envelope_digest()
        );
        assert_eq!(
            report.runtime_requirements().network,
            VerificationNetworkPolicy::Denied
        );

        let public = format!(
            "{}\n{plan:?}",
            serde_json::to_string(&plan).expect("plan JSON")
        );
        for private in [
            "/private/test-workspace",
            "/private/test-state",
            "workspace_root",
            "cache_path",
            "github_token",
        ] {
            assert!(
                !public.contains(private),
                "private evidence leaked: {private}"
            );
        }

        let mismatch = planning_fixture(21)
            .plan()
            .expect_err("same path on a different object must fail closed");
        assert_eq!(
            mismatch.kind(),
            PersonalWorkerVerificationPlanErrorKind::WorkspaceMismatch
        );

        let future_snapshot = planning_fixture_with_job_observed(20, time(1_015_000))
            .plan()
            .expect_err("future durable observation must fail closed");
        assert_eq!(
            future_snapshot.kind(),
            PersonalWorkerVerificationPlanErrorKind::SnapshotMismatch
        );

        let underprovisioned = planning_fixture_with(20, time(1_000_000), limits(1_000, 4 * GIB))
            .plan()
            .expect_err("one vCPU cannot satisfy the checked-in Rust envelope");
        assert_eq!(
            underprovisioned.kind(),
            PersonalWorkerVerificationPlanErrorKind::ResourceMismatch
        );
    }

    #[test]
    fn current_glaeda_plan_uses_successor_cache_generation() {
        let legacy = planning_fixture(20)
            .plan()
            .expect("legacy verification plan");
        let current = planning_fixture_for(
            20,
            time(1_000_000),
            limits(4_000, 4 * GIB),
            "teamleaderleo/glaeda",
            GLAEDA_REQUIRED_PROFILE_ID,
        )
        .plan()
        .expect("current Glaeda verification plan");

        assert_eq!(
            current.report().command().identity.repository().as_str(),
            "teamleaderleo/glaeda"
        );
        assert_eq!(
            current.report().cache().generation,
            VerificationCacheIdentityGeneration::GlaedaV2
        );
        assert_ne!(
            current
                .report()
                .cache()
                .source_command_envelope_namespace_digest,
            legacy
                .report()
                .cache()
                .source_command_envelope_namespace_digest
        );
        assert_eq!(
            current
                .report()
                .cache()
                .source_command_envelope_namespace_digest
                .as_str(),
            "sha256:e228e51012fcee696df3cec29fbc7ad9b96043d359cde02104568d8791781d10"
        );
    }

    #[test]
    fn checked_in_profiles_select_exact_cache_generation_and_refuse_cross_commands() {
        let registry = smolrunner_profile_registry().expect("registry");
        for (profile_id, expected) in [
            (
                GLAEDA_REQUIRED_PROFILE_ID,
                VerificationCacheIdentityGeneration::GlaedaV2,
            ),
            (
                GLAEDA_DOCTOR_PROFILE_ID,
                VerificationCacheIdentityGeneration::GlaedaV2,
            ),
            (
                GLAEDA_PLAN_PROFILE_ID,
                VerificationCacheIdentityGeneration::GlaedaV2,
            ),
            (
                SMOLRUNNER_REQUIRED_PROFILE_ID,
                VerificationCacheIdentityGeneration::SmolrunnerV1,
            ),
            (
                SMOLRUNNER_DOCTOR_PROFILE_ID,
                VerificationCacheIdentityGeneration::SmolrunnerV1,
            ),
            (
                SMOLRUNNER_PLAN_PROFILE_ID,
                VerificationCacheIdentityGeneration::SmolrunnerV1,
            ),
        ] {
            let id = VerificationProfileId::parse(profile_id).expect("profile ID");
            let profile = registry.lookup(&id).expect("registered profile");
            assert_eq!(
                verification_cache_identity_generation(profile).unwrap(),
                expected
            );
        }

        let glaeda = registry
            .lookup(&VerificationProfileId::parse(GLAEDA_REQUIRED_PROFILE_ID).unwrap())
            .unwrap();
        let smolrunner = registry
            .lookup(&VerificationProfileId::parse(SMOLRUNNER_REQUIRED_PROFILE_ID).unwrap())
            .unwrap();
        assert!(
            glaeda
                .select_command(smolrunner.canonical_command().identity())
                .is_err()
        );
        assert!(
            smolrunner
                .select_command(glaeda.canonical_command().identity())
                .is_err()
        );
    }

    #[test]
    fn runtime_requirements_are_closed_and_non_downgradable() {
        let required = VerificationRuntimeRequirements::REQUIRED;
        assert_eq!(
            required.container,
            VerificationContainerPolicy::RootlessDisposableDigestBound
        );
        assert_eq!(
            required.execution_group,
            VerificationExecutionGroupPolicy::DedicatedCgroupV2ProveEmpty
        );
        assert_eq!(required.network, VerificationNetworkPolicy::Denied);
        assert_eq!(required.credentials, VerificationCredentialPolicy::Absent);
        assert_eq!(
            required.detached_processes,
            VerificationDetachedProcessPolicy::Forbidden
        );
        assert!(required.revalidate_workspace_object_before_mutation);
        assert!(required.bind_runtime_image_and_toolchain_to_cache);
        let json = serde_json::to_string(&required).expect("requirements JSON");
        assert!(json.contains("rootless_disposable_digest_bound"));
        assert!(json.contains("dedicated_cgroup_v2_prove_empty"));
        assert!(!json.contains("optional"));
    }

    #[test]
    fn cache_subnamespace_generations_are_pinned_deterministic_and_field_framed() {
        let fields = &[
            b"parent".as_slice(),
            b"repo",
            b"commit",
            b"tree",
            b"command",
        ];
        let old_source = digest_namespace_fields(
            source_command_cache_domain(VerificationCacheIdentityGeneration::SmolrunnerV1),
            fields,
        )
        .expect("old source digest");
        let new_source = digest_namespace_fields(
            source_command_cache_domain(VerificationCacheIdentityGeneration::GlaedaV2),
            fields,
        )
        .expect("new source digest");
        assert_eq!(
            old_source.as_str(),
            "sha256:b11266e3841edaf6c2170420fb2063b5b78028642f04ab640aa6142157b870f8"
        );
        assert_eq!(
            new_source.as_str(),
            "sha256:5e59cee48307a05dd1c2762f3204ef33984ae8fae6959f04ac108963e6390b3c"
        );
        assert_ne!(old_source, new_source);

        let replay = digest_namespace_fields(
            source_command_cache_domain(VerificationCacheIdentityGeneration::GlaedaV2),
            fields,
        )
        .expect("replay digest");
        let changed_tree = digest_namespace_fields(
            source_command_cache_domain(VerificationCacheIdentityGeneration::GlaedaV2),
            &[b"parent", b"repo", b"commit", b"different-tree", b"command"],
        )
        .expect("changed tree digest");
        let changed_command = digest_namespace_fields(
            source_command_cache_domain(VerificationCacheIdentityGeneration::GlaedaV2),
            &[b"parent", b"repo", b"commit", b"tree", b"different-command"],
        )
        .expect("changed command digest");
        let left =
            digest_namespace_fields(b"test-cache-domain-v1", &[b"ab", b"c"]).expect("left digest");
        let right =
            digest_namespace_fields(b"test-cache-domain-v1", &[b"a", b"bc"]).expect("right digest");
        assert_eq!(new_source, replay);
        assert_ne!(new_source, changed_tree);
        assert_ne!(new_source, changed_command);
        assert_ne!(left, right);

        let protected = digest("a");
        let source_command = digest("b");
        let envelope = digest("c");
        let old_envelope = envelope_cache_namespace_digest(
            VerificationCacheIdentityGeneration::SmolrunnerV1,
            &protected,
            &source_command,
            &envelope,
        )
        .expect("old envelope digest");
        let new_envelope = envelope_cache_namespace_digest(
            VerificationCacheIdentityGeneration::GlaedaV2,
            &protected,
            &source_command,
            &envelope,
        )
        .expect("new envelope digest");
        let changed_envelope = envelope_cache_namespace_digest(
            VerificationCacheIdentityGeneration::GlaedaV2,
            &protected,
            &source_command,
            &digest("d"),
        )
        .expect("changed envelope digest");
        assert_eq!(
            old_envelope.as_str(),
            "sha256:5d6aafbb018f52018fee81f7a68c795b82c361a4461dcabe9c785f2088de2021"
        );
        assert_eq!(
            new_envelope.as_str(),
            "sha256:5db35abef5490ab8cfe7946c85f6b30bee3ee8650591cf3352f6bfa59602fb06"
        );
        assert_ne!(old_envelope, new_envelope);
        assert_ne!(new_envelope, changed_envelope);
        assert!(new_source.as_str().starts_with("sha256:"));
    }

    #[test]
    fn cache_generation_is_identity_only_and_contains_no_path_or_mutation_surface() {
        for generation in [
            VerificationCacheIdentityGeneration::SmolrunnerV1,
            VerificationCacheIdentityGeneration::GlaedaV2,
        ] {
            let public = format!(
                "{}\n{generation:?}",
                serde_json::to_string(&generation).expect("generation JSON")
            );
            for forbidden in ["/", "path", "delete", "rename", "adopt", "mutate"] {
                assert!(!public.contains(forbidden));
            }
        }
    }

    #[test]
    fn reservation_and_operator_deadline_choose_the_earliest_boundary() {
        assert_eq!(effective_not_after(time(20_000), None), time(20_000));
        assert_eq!(
            effective_not_after(time(20_000), Some(time(15_000))),
            time(15_000)
        );
        assert_eq!(
            effective_not_after(time(20_000), Some(time(25_000))),
            time(20_000)
        );
    }

    #[test]
    fn public_errors_are_fixed_and_private_evidence_free() {
        let errors = [
            configuration_mismatch(),
            snapshot_mismatch(),
            runner_not_ready(),
            job_not_reserved(),
            source_mismatch(),
            workspace_mismatch(),
            profile_mismatch(),
            resource_mismatch(),
            cache_mismatch(),
            deadline_expired(),
        ];
        for error in errors {
            let public = format!(
                "{}\n{:?}\n{}",
                serde_json::to_string(&error).expect("error JSON"),
                error,
                error
            );
            for private in [
                "/home/lima/private-workspace",
                "github_token",
                "private stderr",
                "st_ino",
                "st_dev",
            ] {
                assert!(!public.contains(private));
            }
        }
    }
}
