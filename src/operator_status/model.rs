use std::fmt;

use serde::Serialize;

use crate::actions_runner_readiness::ActionsRunnerReadinessState;
use crate::artifact::Sha256Digest;
use crate::execution_admission::{EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionState};
use crate::lima_observation::LimaRuntimeState;
use crate::operator_config::OperatorConfigIdentity;
use crate::operator_error::{
    OperatorApprovalClass, OperatorDependencyClass, OperatorErrorCode, OperatorPublicError,
    OperatorRemediationClass, OperatorRetryClass, OperatorSuggestedCommand,
};
use crate::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerProfile, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerSourceIdentity,
};
use crate::personal_worker_read_model::{
    PersonalWorkerJobView, PersonalWorkerStatusView, PersonalWorkerTerminalJobView,
};
use crate::personal_worker_store::PersonalWorkerStoreRevision;

pub const OPERATOR_STATUS_SCHEMA_VERSION: u8 = 3;
pub const MAX_OPERATOR_STATUS_BLOCKERS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorConfigurationCompatibility {
    Compatible,
    Incompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorStatusDisposition {
    Satisfied,
    Blocked,
    Continuation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorConfigurationStatus {
    identity: OperatorConfigIdentity,
    compatibility: OperatorConfigurationCompatibility,
}

impl OperatorConfigurationStatus {
    #[must_use]
    pub const fn new(
        identity: OperatorConfigIdentity,
        compatibility: OperatorConfigurationCompatibility,
    ) -> Self {
        Self {
            identity,
            compatibility,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &OperatorConfigIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn compatibility(&self) -> OperatorConfigurationCompatibility {
        self.compatibility
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperatorWorkerSummary {
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    profile_observation: PersonalWorkerProfileObservation,
    activity_evidence: PersonalWorkerActivityEvidence,
    desired_profile: PersonalWorkerProfile,
    queued_entry_count: u32,
    eligible_queue_count: u32,
    cancelled_queue_count: u32,
    selected_count: u32,
    active_count: u32,
    draining_count: u32,
    cache_lease_count: u32,
    terminal_tombstone_count: u32,
}

impl OperatorWorkerSummary {
    #[must_use]
    pub fn from_status(status: &PersonalWorkerStatusView) -> Self {
        Self {
            store_revision: status.store_revision(),
            queue_generation: status.queue_generation(),
            profile_observation: status.profile_observation(),
            activity_evidence: status.activity_evidence(),
            desired_profile: status.desired_profile(),
            queued_entry_count: status.queued_entry_count(),
            eligible_queue_count: status.eligible_queue_count(),
            cancelled_queue_count: status.cancelled_queue_count(),
            selected_count: status.selected_count(),
            active_count: status.active_count(),
            draining_count: status.draining_count(),
            cache_lease_count: status.cache_lease_count(),
            terminal_tombstone_count: status.terminal_tombstone_count(),
        }
    }

    #[must_use]
    pub const fn store_revision(self) -> PersonalWorkerStoreRevision {
        self.store_revision
    }

    #[must_use]
    pub const fn queue_generation(self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn profile_observation(self) -> PersonalWorkerProfileObservation {
        self.profile_observation
    }

    #[must_use]
    pub const fn current_profile(self) -> Option<PersonalWorkerProfile> {
        self.profile_observation.profile()
    }

    #[must_use]
    pub const fn activity_evidence(self) -> PersonalWorkerActivityEvidence {
        self.activity_evidence
    }

    #[must_use]
    pub const fn desired_profile(self) -> PersonalWorkerProfile {
        self.desired_profile
    }

    #[must_use]
    pub const fn queued_entry_count(self) -> u32 {
        self.queued_entry_count
    }

    #[must_use]
    pub const fn active_count(self) -> u32 {
        self.active_count
    }

    #[must_use]
    pub const fn draining_count(self) -> u32 {
        self.draining_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperatorMachineSummary {
    lima: LimaRuntimeState,
    runner: ActionsRunnerReadinessState,
}

impl OperatorMachineSummary {
    #[must_use]
    pub const fn new(lima: LimaRuntimeState, runner: ActionsRunnerReadinessState) -> Self {
        Self { lima, runner }
    }

    #[must_use]
    pub const fn lima(self) -> LimaRuntimeState {
        self.lima
    }

    #[must_use]
    pub const fn runner(self) -> ActionsRunnerReadinessState {
        self.runner
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorActiveJobSummary {
    identity: ExecutionAdmissionIdentity,
    source: PersonalWorkerSourceIdentity,
    state: ExecutionAdmissionState,
}

impl OperatorActiveJobSummary {
    /// Build one active-job projection from typed evidence.
    ///
    /// # Errors
    ///
    /// Returns a fixed error unless the state is starting, running, or draining.
    pub fn new(
        identity: ExecutionAdmissionIdentity,
        source: PersonalWorkerSourceIdentity,
        state: ExecutionAdmissionState,
    ) -> Result<Self, OperatorStatusError> {
        if !matches!(
            state,
            ExecutionAdmissionState::Starting
                | ExecutionAdmissionState::Running
                | ExecutionAdmissionState::Draining
        ) {
            return Err(OperatorStatusError::new(
                OperatorStatusErrorKind::InvalidActiveJob,
                "active job state must be starting, running, or draining",
            ));
        }
        Ok(Self {
            identity,
            source,
            state,
        })
    }

    /// Project one active durable job view.
    ///
    /// # Errors
    ///
    /// Returns a fixed error unless the view contains active admission evidence.
    pub fn from_job_view(view: &PersonalWorkerJobView) -> Result<Self, OperatorStatusError> {
        let entry = view.entry().ok_or_else(|| {
            OperatorStatusError::new(
                OperatorStatusErrorKind::InvalidActiveJob,
                "active job summary requires one active durable job view",
            )
        })?;
        let admission = view.admission().ok_or_else(|| {
            OperatorStatusError::new(
                OperatorStatusErrorKind::InvalidActiveJob,
                "active job summary requires exact admission evidence",
            )
        })?;
        Self::new(
            ExecutionAdmissionIdentity::new(
                entry.request_id.clone(),
                entry.verification_profile_id.clone(),
                entry.runner_profile_id.clone(),
            ),
            PersonalWorkerSourceIdentity::new(
                entry.repository.clone(),
                entry.commit.clone(),
                entry.tree.clone(),
            ),
            admission.state(),
        )
    }

    #[must_use]
    pub const fn identity(&self) -> &ExecutionAdmissionIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn source(&self) -> &PersonalWorkerSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn state(&self) -> ExecutionAdmissionState {
        self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum OperatorTerminalResult {
    Succeeded,
    Failed { error: OperatorPublicError },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorTerminalScope {
    LocalPersonalWorkerTerminal,
}

impl OperatorTerminalScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalPersonalWorkerTerminal => "local_personal_worker_terminal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorTerminalSummary {
    scope: OperatorTerminalScope,
    identity: ExecutionAdmissionIdentity,
    source: PersonalWorkerSourceIdentity,
    completed_at: EpochMillis,
    evidence_digest: Sha256Digest,
    result: OperatorTerminalResult,
}

impl OperatorTerminalSummary {
    #[must_use]
    pub const fn new(
        identity: ExecutionAdmissionIdentity,
        source: PersonalWorkerSourceIdentity,
        completed_at: EpochMillis,
        evidence_digest: Sha256Digest,
        result: OperatorTerminalResult,
    ) -> Self {
        Self {
            scope: OperatorTerminalScope::LocalPersonalWorkerTerminal,
            identity,
            source,
            completed_at,
            evidence_digest,
            result,
        }
    }

    #[must_use]
    pub fn from_terminal_view(
        view: &PersonalWorkerTerminalJobView,
        result: OperatorTerminalResult,
    ) -> Self {
        Self::new(
            view.request().identity().clone(),
            view.request().source().clone(),
            view.completed_at(),
            view.evidence_digest().clone(),
            result,
        )
    }

    #[must_use]
    pub const fn scope(&self) -> OperatorTerminalScope {
        self.scope
    }

    #[must_use]
    pub const fn result(&self) -> OperatorTerminalResult {
        self.result
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorStatusReport {
    schema_version: u8,
    disposition: OperatorStatusDisposition,
    configuration: OperatorConfigurationStatus,
    worker: OperatorWorkerSummary,
    machine: OperatorMachineSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_job: Option<OperatorActiveJobSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_terminal: Option<OperatorTerminalSummary>,
    blockers: Vec<OperatorPublicError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_action: Option<OperatorSuggestedCommand>,
}

impl OperatorStatusReport {
    /// Compose one bounded report from typed evidence.
    ///
    /// Disposition and next action are derived here. Callers cannot claim a contradictory healthy
    /// state or inject arbitrary commands.
    ///
    /// # Errors
    ///
    /// Returns one fixed error for contradictory machine, worker, configuration, or blocker evidence.
    pub fn new(
        configuration: OperatorConfigurationStatus,
        worker: OperatorWorkerSummary,
        machine: OperatorMachineSummary,
        active_job: Option<OperatorActiveJobSummary>,
        latest_terminal: Option<OperatorTerminalSummary>,
        blockers: impl IntoIterator<Item = OperatorPublicError>,
    ) -> Result<Self, OperatorStatusError> {
        validate_worker_counts(worker)?;
        validate_machine_and_active(worker, machine, active_job.as_ref())?;
        let blockers = bounded_unique_blockers(blockers)?;
        validate_configuration(&configuration, &blockers)?;

        let disposition = if blockers.is_empty() {
            if continuation_required(worker, machine) {
                OperatorStatusDisposition::Continuation
            } else {
                OperatorStatusDisposition::Satisfied
            }
        } else {
            OperatorStatusDisposition::Blocked
        };
        let next_action = match disposition {
            OperatorStatusDisposition::Satisfied => None,
            OperatorStatusDisposition::Continuation => {
                Some(OperatorSuggestedCommand::WorkerRunOnce)
            }
            OperatorStatusDisposition::Blocked => blockers
                .iter()
                .filter_map(OperatorPublicError::suggested_command)
                .min_by_key(command_priority),
        };

        Ok(Self {
            schema_version: OPERATOR_STATUS_SCHEMA_VERSION,
            disposition,
            configuration,
            worker,
            machine,
            active_job,
            latest_terminal,
            blockers,
            next_action,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(&self) -> OperatorStatusDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn worker(&self) -> OperatorWorkerSummary {
        self.worker
    }

    #[must_use]
    pub const fn machine(&self) -> OperatorMachineSummary {
        self.machine
    }

    #[must_use]
    pub const fn active_job(&self) -> Option<&OperatorActiveJobSummary> {
        self.active_job.as_ref()
    }

    #[must_use]
    pub const fn latest_terminal(&self) -> Option<&OperatorTerminalSummary> {
        self.latest_terminal.as_ref()
    }

    #[must_use]
    pub fn blockers(&self) -> &[OperatorPublicError] {
        &self.blockers
    }

    #[must_use]
    pub const fn next_action(&self) -> Option<OperatorSuggestedCommand> {
        self.next_action
    }

    #[must_use]
    pub fn render_human(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for OperatorStatusReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "status: {}", disposition_name(self.disposition))?;
        writeln!(
            formatter,
            "configuration: {} {}",
            compatibility_name(self.configuration.compatibility),
            self.configuration.identity.digest().as_str()
        )?;
        writeln!(
            formatter,
            "worker: revision={} generation={} current={} desired={} queued={} eligible={} cancelled={} selected={} active={} draining={} cache_leases={} terminal_results={}",
            self.worker.store_revision.get(),
            self.worker.queue_generation.get(),
            profile_observation_name(self.worker.profile_observation),
            profile_name(self.worker.desired_profile),
            self.worker.queued_entry_count,
            self.worker.eligible_queue_count,
            self.worker.cancelled_queue_count,
            self.worker.selected_count,
            self.worker.active_count,
            self.worker.draining_count,
            self.worker.cache_lease_count,
            self.worker.terminal_tombstone_count,
        )?;
        writeln!(
            formatter,
            "machine: lima={} runner={}",
            lima_name(self.machine.lima),
            runner_name(self.machine.runner)
        )?;
        match self.worker.activity_evidence {
            PersonalWorkerActivityEvidence::Never => {
                writeln!(formatter, "worker activity: never")?;
            }
            PersonalWorkerActivityEvidence::Observed { last_activity_at } => {
                writeln!(
                    formatter,
                    "worker activity: observed at {}",
                    last_activity_at.get()
                )?;
            }
        }

        if let Some(active) = &self.active_job {
            writeln!(
                formatter,
                "active: request={} state={} source={}@{}",
                active.identity.request_id.as_str(),
                admission_name(active.state),
                active.source.repository.as_str(),
                active.source.commit.as_str(),
            )?;
        }
        if let Some(terminal) = &self.latest_terminal {
            match terminal.result {
                OperatorTerminalResult::Succeeded => writeln!(
                    formatter,
                    "terminal: scope={} request={} result=succeeded completed_at={} evidence={}",
                    terminal.scope.as_str(),
                    terminal.identity.request_id.as_str(),
                    terminal.completed_at.get(),
                    terminal.evidence_digest.as_str(),
                )?,
                OperatorTerminalResult::Failed { error } => writeln!(
                    formatter,
                    "terminal: scope={} request={} result=failed completed_at={} evidence={} summary={}",
                    terminal.scope.as_str(),
                    terminal.identity.request_id.as_str(),
                    terminal.completed_at.get(),
                    terminal.evidence_digest.as_str(),
                    error.summary(),
                )?,
            }
        }
        for blocker in &self.blockers {
            write!(
                formatter,
                "blocker: {} retry={} remediation={}",
                blocker.summary(),
                retry_name(blocker.retry()),
                remediation_name(blocker.remediation())
            )?;
            if let Some(dependency) = blocker.dependency() {
                write!(formatter, " dependency={}", dependency_name(dependency))?;
            }
            if let Some(approval) = blocker.approval() {
                write!(formatter, " approval={}", approval_name(approval))?;
            }
            writeln!(formatter)?;
        }
        if let Some(next_action) = self.next_action {
            writeln!(formatter, "next: {}", next_action.as_str())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorStatusErrorKind {
    TooManyBlockers,
    InvalidWorkerCounts,
    InvalidActiveJob,
    ContradictoryMachineState,
    ContradictoryConfigurationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperatorStatusError {
    kind: OperatorStatusErrorKind,
    public_message: &'static str,
}

impl OperatorStatusError {
    const fn new(kind: OperatorStatusErrorKind, public_message: &'static str) -> Self {
        Self {
            kind,
            public_message,
        }
    }

    #[must_use]
    pub const fn kind(self) -> OperatorStatusErrorKind {
        self.kind
    }
}

impl fmt::Display for OperatorStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for OperatorStatusError {}

fn validate_worker_counts(worker: OperatorWorkerSummary) -> Result<(), OperatorStatusError> {
    if worker
        .eligible_queue_count
        .saturating_add(worker.cancelled_queue_count)
        > worker.queued_entry_count
        || worker.selected_count > worker.eligible_queue_count
        || worker.draining_count > worker.active_count
    {
        return Err(OperatorStatusError::new(
            OperatorStatusErrorKind::InvalidWorkerCounts,
            "worker count evidence is internally inconsistent",
        ));
    }
    Ok(())
}

fn validate_machine_and_active(
    worker: OperatorWorkerSummary,
    machine: OperatorMachineSummary,
    active_job: Option<&OperatorActiveJobSummary>,
) -> Result<(), OperatorStatusError> {
    if machine.lima != LimaRuntimeState::Running
        && matches!(
            machine.runner,
            ActionsRunnerReadinessState::IdleReady
                | ActionsRunnerReadinessState::Busy
                | ActionsRunnerReadinessState::Draining
        )
    {
        return Err(OperatorStatusError::new(
            OperatorStatusErrorKind::ContradictoryMachineState,
            "a non-running Lima instance cannot carry a ready or active runner state",
        ));
    }
    if (worker.active_count == 0) != active_job.is_none() {
        return Err(OperatorStatusError::new(
            OperatorStatusErrorKind::InvalidActiveJob,
            "active job summary must agree with the durable active count",
        ));
    }
    match machine.runner {
        ActionsRunnerReadinessState::IdleReady
            if worker.active_count > 0 || worker.draining_count > 0 =>
        {
            return Err(OperatorStatusError::new(
                OperatorStatusErrorKind::ContradictoryMachineState,
                "an idle-ready runner cannot carry active or draining durable work",
            ));
        }
        ActionsRunnerReadinessState::Busy
            if worker.active_count == 0 || worker.draining_count > 0 =>
        {
            return Err(OperatorStatusError::new(
                OperatorStatusErrorKind::ContradictoryMachineState,
                "a busy runner requires active non-draining durable work",
            ));
        }
        ActionsRunnerReadinessState::Draining
            if worker.active_count == 0 || worker.draining_count == 0 =>
        {
            return Err(OperatorStatusError::new(
                OperatorStatusErrorKind::ContradictoryMachineState,
                "a draining runner requires draining durable work",
            ));
        }
        _ => {}
    }
    if let Some(active) = active_job {
        if active.state == ExecutionAdmissionState::Draining && worker.draining_count == 0 {
            return Err(OperatorStatusError::new(
                OperatorStatusErrorKind::InvalidActiveJob,
                "a draining active job requires durable draining evidence",
            ));
        }
        if machine.runner == ActionsRunnerReadinessState::Draining
            && active.state != ExecutionAdmissionState::Draining
        {
            return Err(OperatorStatusError::new(
                OperatorStatusErrorKind::InvalidActiveJob,
                "a draining runner requires a draining representative job",
            ));
        }
    }
    Ok(())
}

fn bounded_unique_blockers(
    blockers: impl IntoIterator<Item = OperatorPublicError>,
) -> Result<Vec<OperatorPublicError>, OperatorStatusError> {
    let mut unique = Vec::new();
    for blocker in blockers {
        if unique
            .iter()
            .any(|existing: &OperatorPublicError| existing.code() == blocker.code())
        {
            continue;
        }
        if unique.len() == MAX_OPERATOR_STATUS_BLOCKERS {
            return Err(OperatorStatusError::new(
                OperatorStatusErrorKind::TooManyBlockers,
                "operator status exceeds the bounded blocker count",
            ));
        }
        unique.push(blocker);
    }
    unique.sort_by_key(|blocker| {
        (
            remediation_priority(blocker.remediation()),
            blocker.code() as u16,
        )
    });
    Ok(unique)
}

const fn remediation_priority(value: OperatorRemediationClass) -> u8 {
    match value {
        OperatorRemediationClass::ApprovalRequired => 0,
        OperatorRemediationClass::Repair => 1,
        OperatorRemediationClass::Dependency => 2,
        OperatorRemediationClass::Refresh => 3,
        OperatorRemediationClass::Retry => 4,
        OperatorRemediationClass::Terminal => 5,
    }
}

const fn command_priority(value: &OperatorSuggestedCommand) -> u8 {
    match value {
        OperatorSuggestedCommand::WorkerInit => 0,
        OperatorSuggestedCommand::Status => 1,
        OperatorSuggestedCommand::WorkerRunOnce => 2,
        OperatorSuggestedCommand::QueueList => 3,
    }
}

fn validate_configuration(
    configuration: &OperatorConfigurationStatus,
    blockers: &[OperatorPublicError],
) -> Result<(), OperatorStatusError> {
    let has_incompatible = blockers
        .iter()
        .any(|blocker| blocker.code() == OperatorErrorCode::ConfigurationIncompatible);
    match configuration.compatibility {
        OperatorConfigurationCompatibility::Compatible if has_incompatible => {
            Err(OperatorStatusError::new(
                OperatorStatusErrorKind::ContradictoryConfigurationState,
                "compatible configuration cannot carry the incompatible blocker",
            ))
        }
        OperatorConfigurationCompatibility::Incompatible if !has_incompatible => {
            Err(OperatorStatusError::new(
                OperatorStatusErrorKind::ContradictoryConfigurationState,
                "incompatible configuration requires the exact public blocker",
            ))
        }
        _ => Ok(()),
    }
}

fn continuation_required(
    worker: OperatorWorkerSummary,
    machine: OperatorMachineSummary,
) -> bool {
    worker.profile_observation.profile() != Some(worker.desired_profile)
        || worker.queued_entry_count > 0
        || worker.selected_count > 0
        || worker.active_count > 0
        || worker.draining_count > 0
        || machine.lima == LimaRuntimeState::Installing
        || machine.runner == ActionsRunnerReadinessState::Starting
}

const fn disposition_name(value: OperatorStatusDisposition) -> &'static str {
    match value {
        OperatorStatusDisposition::Satisfied => "satisfied",
        OperatorStatusDisposition::Blocked => "blocked",
        OperatorStatusDisposition::Continuation => "continuation",
    }
}

const fn compatibility_name(value: OperatorConfigurationCompatibility) -> &'static str {
    match value {
        OperatorConfigurationCompatibility::Compatible => "compatible",
        OperatorConfigurationCompatibility::Incompatible => "incompatible",
    }
}

const fn profile_name(value: PersonalWorkerProfile) -> &'static str {
    match value {
        PersonalWorkerProfile::Stopped => "stopped",
        PersonalWorkerProfile::Interactive => "interactive",
        PersonalWorkerProfile::Work => "work",
    }
}

const fn profile_observation_name(value: PersonalWorkerProfileObservation) -> &'static str {
    match value {
        PersonalWorkerProfileObservation::Unobserved => "unobserved",
        PersonalWorkerProfileObservation::Observed { profile } => profile_name(profile),
    }
}

const fn lima_name(value: LimaRuntimeState) -> &'static str {
    match value {
        LimaRuntimeState::Uninitialized => "uninitialized",
        LimaRuntimeState::Installing => "installing",
        LimaRuntimeState::Broken => "broken",
        LimaRuntimeState::Stopped => "stopped",
        LimaRuntimeState::Running => "running",
    }
}

const fn runner_name(value: ActionsRunnerReadinessState) -> &'static str {
    match value {
        ActionsRunnerReadinessState::Offline => "offline",
        ActionsRunnerReadinessState::Starting => "starting",
        ActionsRunnerReadinessState::IdleReady => "idle_ready",
        ActionsRunnerReadinessState::Busy => "busy",
        ActionsRunnerReadinessState::Draining => "draining",
        ActionsRunnerReadinessState::Stale => "stale",
    }
}

const fn admission_name(value: ExecutionAdmissionState) -> &'static str {
    match value {
        ExecutionAdmissionState::Requested => "requested",
        ExecutionAdmissionState::Admitted => "admitted",
        ExecutionAdmissionState::Queued => "queued",
        ExecutionAdmissionState::Reserved => "reserved",
        ExecutionAdmissionState::Starting => "starting",
        ExecutionAdmissionState::Running => "running",
        ExecutionAdmissionState::Draining => "draining",
        ExecutionAdmissionState::Unavailable => "unavailable",
    }
}

const fn retry_name(value: OperatorRetryClass) -> &'static str {
    match value {
        OperatorRetryClass::Immediate => "immediate",
        OperatorRetryClass::AfterRefresh => "after_refresh",
        OperatorRetryClass::AfterRepair => "after_repair",
        OperatorRetryClass::AfterDependency => "after_dependency",
        OperatorRetryClass::Never => "never",
    }
}

const fn remediation_name(value: OperatorRemediationClass) -> &'static str {
    match value {
        OperatorRemediationClass::Retry => "retry",
        OperatorRemediationClass::Refresh => "refresh",
        OperatorRemediationClass::Repair => "repair",
        OperatorRemediationClass::Dependency => "dependency",
        OperatorRemediationClass::ApprovalRequired => "approval_required",
        OperatorRemediationClass::Terminal => "terminal",
    }
}

const fn dependency_name(value: OperatorDependencyClass) -> &'static str {
    match value {
        OperatorDependencyClass::Configuration => "configuration",
        OperatorDependencyClass::DurableState => "durable_state",
        OperatorDependencyClass::Lima => "lima",
        OperatorDependencyClass::RunnerReadiness => "runner_readiness",
        OperatorDependencyClass::Repository => "repository",
        OperatorDependencyClass::Github => "github",
        OperatorDependencyClass::Service => "service",
        OperatorDependencyClass::Release => "release",
    }
}

const fn approval_name(value: OperatorApprovalClass) -> &'static str {
    match value {
        OperatorApprovalClass::CredentialChange => "credential_change",
        OperatorApprovalClass::OperatorService => "operator_service",
        OperatorApprovalClass::PaidCapacity => "paid_capacity",
        OperatorApprovalClass::ExternalPublication => "external_publication",
        OperatorApprovalClass::ReleaseSigning => "release_signing",
        OperatorApprovalClass::DestructiveDataChange => "destructive_data_change",
        OperatorApprovalClass::IrreversibleMigration => "irreversible_migration",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::artifact::{CommitId, GitTreeId, RepositoryRef};
    use crate::execution_admission::{ExecutionRequestId, RunnerProfileId};
    use crate::lima_observation::LimaInstanceName;
    use crate::mac_availability::AvailabilityRequest;
    use crate::operator_config::{
        GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
        OperatorRemediationPreference, PersonalWorkerStateRoot,
    };
    use crate::verification_profile::VerificationProfileId;

    fn configuration(
        compatibility: OperatorConfigurationCompatibility,
    ) -> OperatorConfigurationStatus {
        let config = OperatorConfig::new(
            PersonalWorkerStateRoot::parse("/Users/private-operator/smolrunner")
                .expect("state root"),
            LimaInstanceName::parse("smolrunner").expect("instance"),
            GuestWorkspacePath::parse("/home/lima/private-workspace").expect("workspace"),
            VerificationProfileId::parse("smolrunner.required").expect("profile"),
            AvailabilityRequest::Active,
            OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
            OperatorOutputPreference::Json,
            OperatorRemediationPreference::IncludeSuggestions,
        )
        .expect("config");
        OperatorConfigurationStatus::new(config.identity().clone(), compatibility)
    }

    #[allow(clippy::too_many_arguments)]
    fn worker(
        current_profile: PersonalWorkerProfile,
        desired_profile: PersonalWorkerProfile,
        queued: u32,
        eligible: u32,
        selected: u32,
        active: u32,
        draining: u32,
        terminal: u32,
    ) -> OperatorWorkerSummary {
        OperatorWorkerSummary {
            store_revision: PersonalWorkerStoreRevision::new(7).expect("revision"),
            queue_generation: PersonalWorkerQueueGeneration::new(9).expect("generation"),
            profile_observation: PersonalWorkerProfileObservation::observed(current_profile),
            activity_evidence: PersonalWorkerActivityEvidence::observed(
                EpochMillis::new(1).expect("activity time"),
            ),
            desired_profile,
            queued_entry_count: queued,
            eligible_queue_count: eligible,
            cancelled_queue_count: queued.saturating_sub(eligible),
            selected_count: selected,
            active_count: active,
            draining_count: draining,
            cache_lease_count: active,
            terminal_tombstone_count: terminal,
        }
    }

    fn identity(value: &str) -> ExecutionAdmissionIdentity {
        ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse(value).expect("request"),
            VerificationProfileId::parse("smolrunner.required").expect("profile"),
            RunnerProfileId::parse("mac-m5").expect("runner profile"),
        )
    }

    fn source() -> PersonalWorkerSourceIdentity {
        PersonalWorkerSourceIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1a".repeat(20)).expect("commit"),
            GitTreeId::parse(&"2b".repeat(20)).expect("tree"),
        )
    }

    fn idle_report() -> OperatorStatusReport {
        OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker(
                PersonalWorkerProfile::Stopped,
                PersonalWorkerProfile::Stopped,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Stopped,
                ActionsRunnerReadinessState::Offline,
            ),
            None,
            None,
            [],
        )
        .expect("idle")
    }

    #[test]
    fn idle_worker_has_matching_human_and_json_semantics() {
        let report = idle_report();
        assert_eq!(report.disposition(), OperatorStatusDisposition::Satisfied);
        assert_eq!(report.next_action(), None);
        let value = serde_json::to_value(&report).expect("JSON");
        assert_eq!(value["disposition"], json!("satisfied"));
        assert_eq!(
            value["worker"]["profile_observation"],
            json!({"state": "observed", "profile": "stopped"})
        );
        assert_eq!(value["machine"]["runner"], json!("offline"));
        let human = report.render_human();
        for expected in ["status: satisfied", "current=stopped", "runner=offline"] {
            assert!(human.contains(expected));
        }
    }

    #[test]
    fn unobserved_never_active_worker_requires_run_once_without_invented_values() {
        let mut worker = worker(
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Stopped,
            0,
            0,
            0,
            0,
            0,
            0,
        );
        worker.profile_observation = PersonalWorkerProfileObservation::Unobserved;
        worker.activity_evidence = PersonalWorkerActivityEvidence::Never;
        let report = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker,
            OperatorMachineSummary::new(
                LimaRuntimeState::Uninitialized,
                ActionsRunnerReadinessState::Offline,
            ),
            None,
            None,
            [],
        )
        .expect("unobserved worker status");
        assert_eq!(
            report.disposition(),
            OperatorStatusDisposition::Continuation
        );
        assert_eq!(
            report.next_action(),
            Some(OperatorSuggestedCommand::WorkerRunOnce)
        );
        let value = serde_json::to_value(&report).expect("JSON");
        assert_eq!(
            value["worker"]["profile_observation"],
            json!({"state": "unobserved"})
        );
        assert_eq!(
            value["worker"]["activity_evidence"],
            json!({"state": "never"})
        );
        let human = report.render_human();
        assert!(human.contains("current=unobserved"));
        assert!(human.contains("worker activity: never"));
        assert!(human.contains("next: glaeda worker run-once"));
    }

    #[test]
    fn queued_work_derives_one_continuation_action() {
        let report = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker(
                PersonalWorkerProfile::Interactive,
                PersonalWorkerProfile::Work,
                2,
                2,
                1,
                0,
                0,
                0,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Installing,
                ActionsRunnerReadinessState::Starting,
            ),
            None,
            None,
            [],
        )
        .expect("continuation");
        assert_eq!(report.disposition(), OperatorStatusDisposition::Continuation);
        assert_eq!(
            report.next_action(),
            Some(OperatorSuggestedCommand::WorkerRunOnce)
        );
    }

    #[test]
    fn blockers_deduplicate_and_drive_fixed_action() {
        let missing = OperatorPublicError::from_code(OperatorErrorCode::DurableStateMissing);
        let report = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker(
                PersonalWorkerProfile::Stopped,
                PersonalWorkerProfile::Stopped,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Uninitialized,
                ActionsRunnerReadinessState::Offline,
            ),
            None,
            None,
            [missing, missing],
        )
        .expect("blocked");
        assert_eq!(report.blockers().len(), 1);
        assert_eq!(
            report.next_action(),
            Some(OperatorSuggestedCommand::WorkerInit)
        );
    }

    #[test]
    fn blocker_permutations_have_identical_output_and_action() {
        let queue = OperatorPublicError::from_code(OperatorErrorCode::QueueCapacityReached);
        let missing = OperatorPublicError::from_code(OperatorErrorCode::DurableStateMissing);
        let approval = OperatorPublicError::from_code(
            OperatorErrorCode::OperatorServiceApprovalRequired,
        );
        let build = |blockers: [OperatorPublicError; 3]| {
            OperatorStatusReport::new(
                configuration(OperatorConfigurationCompatibility::Compatible),
                worker(
                    PersonalWorkerProfile::Stopped,
                    PersonalWorkerProfile::Stopped,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ),
                OperatorMachineSummary::new(
                    LimaRuntimeState::Stopped,
                    ActionsRunnerReadinessState::Offline,
                ),
                None,
                None,
                blockers,
            )
            .expect("blocked report")
        };
        let reports = [
            build([queue, missing, approval]),
            build([approval, queue, missing]),
            build([missing, approval, queue]),
        ];
        let expected_json = serde_json::to_string(&reports[0]).expect("JSON");
        let expected_human = reports[0].render_human();
        for report in &reports {
            assert_eq!(report.disposition(), OperatorStatusDisposition::Blocked);
            assert_eq!(
                report.next_action(),
                Some(OperatorSuggestedCommand::WorkerInit)
            );
            assert_eq!(serde_json::to_string(report).expect("JSON"), expected_json);
            assert_eq!(report.render_human(), expected_human);
        }
    }

    #[test]
    fn incompatible_configuration_requires_exact_blocker() {
        let error = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Incompatible),
            worker(
                PersonalWorkerProfile::Stopped,
                PersonalWorkerProfile::Stopped,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Stopped,
                ActionsRunnerReadinessState::Offline,
            ),
            None,
            None,
            [],
        )
        .expect_err("missing blocker");
        assert_eq!(
            error.kind(),
            OperatorStatusErrorKind::ContradictoryConfigurationState
        );
    }

    #[test]
    fn active_job_and_runner_evidence_must_agree() {
        let active = OperatorActiveJobSummary::new(
            identity("request-1"),
            source(),
            ExecutionAdmissionState::Running,
        )
        .expect("active");
        let report = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker(
                PersonalWorkerProfile::Work,
                PersonalWorkerProfile::Work,
                0,
                0,
                0,
                1,
                0,
                0,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Running,
                ActionsRunnerReadinessState::Busy,
            ),
            Some(active),
            None,
            [],
        )
        .expect("active report");
        assert_eq!(report.disposition(), OperatorStatusDisposition::Continuation);
    }

    #[test]
    fn impossible_machine_evidence_fails_closed() {
        let error = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker(
                PersonalWorkerProfile::Stopped,
                PersonalWorkerProfile::Stopped,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Stopped,
                ActionsRunnerReadinessState::IdleReady,
            ),
            None,
            None,
            [],
        )
        .expect_err("contradiction");
        assert_eq!(
            error.kind(),
            OperatorStatusErrorKind::ContradictoryMachineState
        );
    }

    #[test]
    fn terminal_summary_is_separate_from_current_activity() {
        let terminal = OperatorTerminalSummary::new(
            identity("terminal-1"),
            source(),
            EpochMillis::new(5_000).expect("time"),
            Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("digest"),
            OperatorTerminalResult::Failed {
                error: OperatorPublicError::from_code(
                    OperatorErrorCode::TerminalClassificationInconclusive,
                ),
            },
        );
        let report = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker(
                PersonalWorkerProfile::Stopped,
                PersonalWorkerProfile::Stopped,
                0,
                0,
                0,
                0,
                0,
                1,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Stopped,
                ActionsRunnerReadinessState::Offline,
            ),
            None,
            Some(terminal),
            [],
        )
        .expect("terminal");
        assert!(report.active_job().is_none());
        let terminal = report.latest_terminal().expect("terminal summary");
        assert_eq!(
            terminal.scope(),
            OperatorTerminalScope::LocalPersonalWorkerTerminal
        );
        let value = serde_json::to_value(&report).expect("JSON");
        assert_eq!(value["schema_version"], json!(3));
        assert_eq!(
            value["latest_terminal"]["scope"],
            json!("local_personal_worker_terminal")
        );
        let human = report.render_human();
        assert!(human.contains("scope=local_personal_worker_terminal"));
        assert!(human.contains("result=failed"));
    }

    #[test]
    fn blockers_are_bounded() {
        let codes = [
            OperatorErrorCode::ConfigurationMissing,
            OperatorErrorCode::ConfigurationVersionUnsupported,
            OperatorErrorCode::ConfigurationInvalid,
            OperatorErrorCode::DurableStateMissing,
            OperatorErrorCode::DurableStateUnsafe,
            OperatorErrorCode::DurableStateCorrupt,
            OperatorErrorCode::DurableStateVersionIncompatible,
            OperatorErrorCode::DurableStateRecoveryRequired,
            OperatorErrorCode::DurableStateBusy,
            OperatorErrorCode::DurableStateRevisionStale,
            OperatorErrorCode::DurableStateGenerationStale,
            OperatorErrorCode::JobDuplicate,
            OperatorErrorCode::JobConflict,
            OperatorErrorCode::QueueCapacityReached,
            OperatorErrorCode::CancellationConflict,
            OperatorErrorCode::TerminalReplay,
            OperatorErrorCode::RepositoryUnavailable,
        ];
        let error = OperatorStatusReport::new(
            configuration(OperatorConfigurationCompatibility::Compatible),
            worker(
                PersonalWorkerProfile::Stopped,
                PersonalWorkerProfile::Stopped,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            OperatorMachineSummary::new(
                LimaRuntimeState::Stopped,
                ActionsRunnerReadinessState::Offline,
            ),
            None,
            None,
            codes.map(OperatorPublicError::from_code),
        )
        .expect_err("bounded");
        assert_eq!(error.kind(), OperatorStatusErrorKind::TooManyBlockers);
    }

    #[test]
    fn public_surfaces_exclude_private_evidence() {
        let report = idle_report();
        let outputs = [
            serde_json::to_string(&report).expect("JSON"),
            report.render_human(),
            format!("{report:?}"),
        ];
        for forbidden in [
            "/Users/private-operator",
            "/home/lima/private-workspace",
            "PRIVATE_TOKEN_SENTINEL",
            "private child stderr",
        ] {
            assert!(outputs.iter().all(|output| !output.contains(forbidden)));
        }
    }

    #[test]
    fn module_contains_no_observation_or_mutation_authority() {
        let source = include_str!("model.rs");
        for forbidden in [
            concat!("std::", "env::"),
            concat!("std::", "fs::"),
            concat!("std::", "process::"),
            concat!("std::", "time::"),
            concat!("Command", "::"),
            concat!("System", "Time"),
            concat!("lima", "ctl"),
            concat!("key", "chain"),
            concat!("git", "hub"),
        ] {
            assert!(!source.contains(forbidden), "forbidden token: {forbidden}");
        }
    }
}
