use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionRecord, ExecutionAdmissionState,
    ExecutionRequestId, ExecutionResourceLimits, FallbackProfileEligibility, RunnerProfileId,
};
use crate::personal_worker_pid_capacity::{
    PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY, admit_personal_worker_pid_reservation,
};
use crate::verification_profile::{CacheId, VerificationProfileId};

pub const PERSONAL_WORKER_QUEUE_SCHEMA_VERSION: u8 = 3;
pub const MAX_PERSONAL_WORKER_QUEUE_ENTRIES: usize = 256;
pub const MAX_PERSONAL_WORKER_ACTIVE_RESERVATIONS: usize = 2;
pub const PERSONAL_WORKER_TOTAL_CPU_MILLIS: u32 = 8_000;
pub const PERSONAL_WORKER_TOTAL_MEMORY_BYTES: u64 = 10 * 1_024 * 1_024 * 1_024;
pub const PERSONAL_WORKER_RESERVED_CPU_MILLIS: u32 = 1_000;
pub const PERSONAL_WORKER_RESERVED_MEMORY_BYTES: u64 = 2 * 1_024 * 1_024 * 1_024;
pub const PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS: u32 =
    PERSONAL_WORKER_TOTAL_CPU_MILLIS - PERSONAL_WORKER_RESERVED_CPU_MILLIS;
pub const PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES: u64 =
    PERSONAL_WORKER_TOTAL_MEMORY_BYTES - PERSONAL_WORKER_RESERVED_MEMORY_BYTES;
pub const PERSONAL_WORKER_LIGHT_CPU_MILLIS: u32 = 3_500;
pub const PERSONAL_WORKER_LIGHT_MEMORY_BYTES: u64 = 4 * 1_024 * 1_024 * 1_024;
pub const PERSONAL_WORKER_PRIORITY_AGING_MILLIS: u64 = 30 * 60 * 1_000;
pub const PERSONAL_WORKER_INTERACTIVE_COOLDOWN_MILLIS: u64 = 10 * 60 * 1_000;
pub const PERSONAL_WORKER_STOPPED_COOLDOWN_MILLIS: u64 = 30 * 60 * 1_000;

const MAX_QUEUE_GENERATION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PersonalWorkerQueueGeneration(u64);

impl PersonalWorkerQueueGeneration {
    /// Construct one positive bounded queue generation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or an implementation-exceeding generation.
    pub fn new(value: u64) -> Result<Self, PersonalWorkerQueueError> {
        if !(1..=MAX_QUEUE_GENERATION).contains(&value) {
            return Err(PersonalWorkerQueueError::new(
                "generation",
                "invalid_queue_generation",
                "queue generation must be within the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Return the exact next queue generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the bounded generation space is exhausted.
    pub fn next(self) -> Result<Self, PersonalWorkerQueueError> {
        Self::new(self.0.saturating_add(1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerPriority {
    Background,
    Normal,
    Interactive,
}

impl PersonalWorkerPriority {
    const fn base_rank(self) -> u8 {
        match self {
            Self::Background => 0,
            Self::Normal => 1,
            Self::Interactive => 2,
        }
    }

    fn effective_rank(self, age_millis: u64) -> u8 {
        let promotions =
            u8::try_from(age_millis / PERSONAL_WORKER_PRIORITY_AGING_MILLIS).unwrap_or(u8::MAX);
        self.base_rank().saturating_add(promotions).min(2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerProfile {
    Stopped,
    Interactive,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PersonalWorkerProfileObservation {
    Unobserved,
    Observed { profile: PersonalWorkerProfile },
}

impl PersonalWorkerProfileObservation {
    #[must_use]
    pub const fn observed(profile: PersonalWorkerProfile) -> Self {
        Self::Observed { profile }
    }

    #[must_use]
    pub const fn profile(self) -> Option<PersonalWorkerProfile> {
        match self {
            Self::Unobserved => None,
            Self::Observed { profile } => Some(profile),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PersonalWorkerActivityEvidence {
    Never,
    Observed { last_activity_at: EpochMillis },
}

impl PersonalWorkerActivityEvidence {
    #[must_use]
    pub const fn observed(last_activity_at: EpochMillis) -> Self {
        Self::Observed { last_activity_at }
    }

    #[must_use]
    pub const fn last_activity_at(self) -> Option<EpochMillis> {
        match self {
            Self::Never => None,
            Self::Observed { last_activity_at } => Some(last_activity_at),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerJobClass {
    Light,
    Heavy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerSourceIdentity {
    pub repository: RepositoryRef,
    pub commit: CommitId,
    pub tree: GitTreeId,
}

impl PersonalWorkerSourceIdentity {
    #[must_use]
    pub const fn new(repository: RepositoryRef, commit: CommitId, tree: GitTreeId) -> Self {
        Self {
            repository,
            commit,
            tree,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum PersonalWorkerCacheNamespace {
    RepositoryBuild {
        cache_id: CacheId,
        repository: RepositoryRef,
        namespace_digest: Sha256Digest,
    },
    SharedDownload {
        cache_id: CacheId,
        namespace_digest: Sha256Digest,
    },
}

impl PersonalWorkerCacheNamespace {
    #[must_use]
    pub const fn cache_id(&self) -> &CacheId {
        match self {
            Self::RepositoryBuild { cache_id, .. } | Self::SharedDownload { cache_id, .. } => {
                cache_id
            }
        }
    }

    fn validate_for_source(
        &self,
        source: &PersonalWorkerSourceIdentity,
    ) -> Result<(), PersonalWorkerQueueError> {
        if let Self::RepositoryBuild { repository, .. } = self
            && repository != &source.repository
        {
            return Err(PersonalWorkerQueueError::new(
                "cache.repository",
                "cache_repository_mismatch",
                "repository build cache namespace must match the exact job repository",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerCacheAccessMode {
    Read,
    Write,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PersonalWorkerCancellationState {
    Active,
    Cancelled { cancelled_at: EpochMillis },
}

impl PersonalWorkerCancellationState {
    #[must_use]
    pub const fn is_cancelled(self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerJobRequest {
    pub identity: ExecutionAdmissionIdentity,
    pub source: PersonalWorkerSourceIdentity,
    pub priority: PersonalWorkerPriority,
    pub requested_limits: ExecutionResourceLimits,
    pub cache_namespace: PersonalWorkerCacheNamespace,
    pub cache_access: PersonalWorkerCacheAccessMode,
    pub submitted_at: EpochMillis,
    pub operator_deadline: Option<EpochMillis>,
    pub cancellation: PersonalWorkerCancellationState,
    pub fallback_eligibility: FallbackProfileEligibility,
}

impl PersonalWorkerJobRequest {
    #[must_use]
    pub fn job_class(&self) -> PersonalWorkerJobClass {
        if self.requested_limits.cpu_millis <= PERSONAL_WORKER_LIGHT_CPU_MILLIS
            && self.requested_limits.memory_bytes <= PERSONAL_WORKER_LIGHT_MEMORY_BYTES
        {
            PersonalWorkerJobClass::Light
        } else {
            PersonalWorkerJobClass::Heavy
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerActiveReservation {
    pub request: PersonalWorkerJobRequest,
    pub admission: ExecutionAdmissionRecord,
    pub started_at: Option<EpochMillis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalWorkerPendingProfileChange {
    pub target: PersonalWorkerProfile,
    pub requested_at: EpochMillis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerQueueInput {
    pub generation: PersonalWorkerQueueGeneration,
    pub observed_at: EpochMillis,
    pub profile_observation: PersonalWorkerProfileObservation,
    pub activity_evidence: PersonalWorkerActivityEvidence,
    pub queued: Vec<PersonalWorkerJobRequest>,
    pub active: Vec<PersonalWorkerActiveReservation>,
    pub pending_profile_change: Option<PersonalWorkerPendingProfileChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerQueueEntryState {
    Queued,
    Selected,
    Reserved,
    Starting,
    Running,
    Draining,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PersonalWorkerCacheLeaseState {
    Available,
    SharedRead,
    HeldRead,
    HeldWrite,
    HeldExclusive,
    BlockedByWriter,
    BlockedByExclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerQueueVisibility {
    pub request_id: ExecutionRequestId,
    pub repository: RepositoryRef,
    pub commit: CommitId,
    pub tree: GitTreeId,
    pub verification_profile_id: VerificationProfileId,
    pub runner_profile_id: RunnerProfileId,
    pub priority: PersonalWorkerPriority,
    pub effective_priority_rank: u8,
    pub age_millis: u64,
    pub state: PersonalWorkerQueueEntryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u32>,
    pub requested_cpu_millis: u32,
    pub requested_memory_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_cpu_millis: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_memory_bytes: Option<u64>,
    pub cache_namespace: PersonalWorkerCacheNamespace,
    pub cache_access: PersonalWorkerCacheAccessMode,
    pub cache_lease: PersonalWorkerCacheLeaseState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<EpochMillis>,
    pub worker_profile: PersonalWorkerProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerSelection {
    pub request_id: ExecutionRequestId,
    pub repository: RepositoryRef,
    pub verification_profile_id: VerificationProfileId,
    pub runner_profile_id: RunnerProfileId,
    pub priority: PersonalWorkerPriority,
    pub effective_priority_rank: u8,
    pub job_class: PersonalWorkerJobClass,
    pub reserved_limits: ExecutionResourceLimits,
    pub cache_namespace: PersonalWorkerCacheNamespace,
    pub cache_access: PersonalWorkerCacheAccessMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerQueueDecision {
    pub schema_version: u8,
    pub generation: PersonalWorkerQueueGeneration,
    pub observed_at: EpochMillis,
    pub profile_observation: PersonalWorkerProfileObservation,
    pub activity_evidence: PersonalWorkerActivityEvidence,
    pub desired_profile: PersonalWorkerProfile,
    pub cancel_pending_downscale: bool,
    pub profile_change_permitted: bool,
    pub schedulable_cpu_millis: u32,
    pub schedulable_memory_bytes: u64,
    pub schedulable_pids: u32,
    pub selected: Vec<PersonalWorkerSelection>,
    pub visibility: Vec<PersonalWorkerQueueVisibility>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerQueueError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl PersonalWorkerQueueError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for PersonalWorkerQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PersonalWorkerQueueError {}

/// Evaluate one complete personal-worker queue observation without reading time or mutating state.
///
/// The evaluator keeps priority classes fixed, ages waiting jobs deterministically, favours
/// repositories without active or earlier selected work, preserves the guest/cache resource reserve,
/// and grants no cache writer or exclusive lease when the namespace is already in use.
///
/// # Errors
///
/// Returns a bounded static error for stale generations or observations, identity duplication,
/// invalid resource reservations, incompatible cache leases, inconsistent active admission state,
/// or unsafe profile-change evidence.
pub fn evaluate_personal_worker_queue(
    input: &PersonalWorkerQueueInput,
    previous: Option<&PersonalWorkerQueueDecision>,
) -> Result<PersonalWorkerQueueDecision, PersonalWorkerQueueError> {
    validate_generation(input, previous)?;
    validate_input(input)?;

    let active_limits = aggregate_active_limits(&input.active)?;
    let mut held_leases = active_leases(&input.active);
    let mut repository_load = active_repository_load(&input.active);
    let mut remaining = input
        .queued
        .iter()
        .filter(|request| !request.cancellation.is_cancelled())
        .collect::<Vec<_>>();

    let mut selected = Vec::new();
    let mut used_cpu = active_limits.cpu_millis;
    let mut used_memory = active_limits.memory_bytes;
    let mut used_pids = active_limits.pids;
    let active_heavy = input
        .active
        .iter()
        .any(|reservation| reservation.request.job_class() == PersonalWorkerJobClass::Heavy);
    let mut selected_heavy = false;

    while selected.len() + input.active.len() < MAX_PERSONAL_WORKER_ACTIVE_RESERVATIONS {
        remaining.sort_by(|left, right| {
            compare_candidates(left, right, input.observed_at, &repository_load)
        });
        let chosen = remaining.iter().position(|request| {
            let job_class = request.job_class();
            if active_heavy
                || selected_heavy
                || (job_class == PersonalWorkerJobClass::Heavy && !input.active.is_empty())
                || (job_class == PersonalWorkerJobClass::Heavy && !selected.is_empty())
            {
                return false;
            }
            let next_cpu = used_cpu.saturating_add(request.requested_limits.cpu_millis);
            let next_memory = used_memory.saturating_add(request.requested_limits.memory_bytes);
            let pid_capacity_available =
                admit_personal_worker_pid_reservation(&[used_pids], request.requested_limits.pids)
                    .is_ok();
            next_cpu <= PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS
                && next_memory <= PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES
                && pid_capacity_available
                && lease_conflict(&held_leases, &request.cache_namespace, request.cache_access)
                    .is_none()
        });
        let Some(chosen) = chosen else {
            break;
        };
        let request = remaining.remove(chosen);
        let job_class = request.job_class();
        held_leases
            .entry(request.cache_namespace.clone())
            .or_default()
            .push(request.cache_access);
        *repository_load
            .entry(request.source.repository.clone())
            .or_default() += 1;
        used_cpu = used_cpu.saturating_add(request.requested_limits.cpu_millis);
        used_memory = used_memory.saturating_add(request.requested_limits.memory_bytes);
        used_pids =
            admit_personal_worker_pid_reservation(&[used_pids], request.requested_limits.pids)
                .expect("selected PID capacity was checked before reservation")
                .projected_reserved_pids();
        selected_heavy = job_class == PersonalWorkerJobClass::Heavy;
        selected.push(PersonalWorkerSelection {
            request_id: request.identity.request_id.clone(),
            repository: request.source.repository.clone(),
            verification_profile_id: request.identity.verification_profile_id.clone(),
            runner_profile_id: request.identity.runner_profile_id.clone(),
            priority: request.priority,
            effective_priority_rank: effective_priority(request, input.observed_at),
            job_class,
            reserved_limits: request.requested_limits,
            cache_namespace: request.cache_namespace.clone(),
            cache_access: request.cache_access,
        });
    }

    let has_queued_work = input
        .queued
        .iter()
        .any(|request| !request.cancellation.is_cancelled());
    let has_work = has_queued_work || !input.active.is_empty();
    let desired_profile = desired_profile(input, has_work);
    let cancel_pending_downscale = has_work
        && input
            .pending_profile_change
            .is_some_and(|pending| pending.target != PersonalWorkerProfile::Work);
    let profile_change_permitted = input.active.is_empty();
    let visibility = build_visibility(input, &selected, desired_profile);

    Ok(PersonalWorkerQueueDecision {
        schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
        generation: input.generation,
        observed_at: input.observed_at,
        profile_observation: input.profile_observation,
        activity_evidence: input.activity_evidence,
        desired_profile,
        cancel_pending_downscale,
        profile_change_permitted,
        schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
        schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        schedulable_pids: PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY,
        selected,
        visibility,
    })
}

fn validate_generation(
    input: &PersonalWorkerQueueInput,
    previous: Option<&PersonalWorkerQueueDecision>,
) -> Result<(), PersonalWorkerQueueError> {
    if let Some(previous) = previous {
        if previous.schema_version != PERSONAL_WORKER_QUEUE_SCHEMA_VERSION {
            return Err(PersonalWorkerQueueError::new(
                "previous.schema_version",
                "unsupported_previous_queue_schema",
                "previous queue decision schema is not supported",
            ));
        }
        let expected = previous.generation.next()?;
        if input.generation != expected {
            return Err(PersonalWorkerQueueError::new(
                "generation",
                "stale_or_skipped_queue_generation",
                "queue generation must advance by exactly one",
            ));
        }
        if input.observed_at < previous.observed_at {
            return Err(PersonalWorkerQueueError::new(
                "observed_at",
                "queue_observation_time_reversal",
                "queue observation time cannot move backwards",
            ));
        }
    }
    Ok(())
}

fn validate_input(input: &PersonalWorkerQueueInput) -> Result<(), PersonalWorkerQueueError> {
    if input.queued.len() > MAX_PERSONAL_WORKER_QUEUE_ENTRIES {
        return Err(PersonalWorkerQueueError::new(
            "queued",
            "queue_limit_exceeded",
            "personal worker queue exceeds the bounded entry limit",
        ));
    }
    if input.active.len() > MAX_PERSONAL_WORKER_ACTIVE_RESERVATIONS {
        return Err(PersonalWorkerQueueError::new(
            "active",
            "active_reservation_limit_exceeded",
            "personal worker allows at most two active light reservations",
        ));
    }
    if input
        .activity_evidence
        .last_activity_at()
        .is_some_and(|last_activity_at| last_activity_at > input.observed_at)
    {
        return Err(PersonalWorkerQueueError::new(
            "activity_evidence.last_activity_at",
            "future_last_activity",
            "last activity cannot be newer than the queue observation",
        ));
    }
    if input.activity_evidence == PersonalWorkerActivityEvidence::Never
        && (!input.queued.is_empty()
            || !input.active.is_empty()
            || input.pending_profile_change.is_some())
    {
        return Err(PersonalWorkerQueueError::new(
            "activity_evidence",
            "activity_evidence_required",
            "durable work and profile intents require observed activity evidence",
        ));
    }
    if !input.active.is_empty()
        && input.profile_observation
            != PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Work)
    {
        return Err(PersonalWorkerQueueError::new(
            "profile_observation",
            "active_work_requires_work_profile",
            "active reservations require an observed work worker profile",
        ));
    }
    if let Some(pending) = input.pending_profile_change {
        if input.profile_observation.profile().is_none() {
            return Err(PersonalWorkerQueueError::new(
                "profile_observation",
                "profile_intent_requires_observation",
                "profile-change intent requires an observed current profile",
            ));
        }
        if pending.requested_at > input.observed_at {
            return Err(PersonalWorkerQueueError::new(
                "pending_profile_change.requested_at",
                "future_profile_change_request",
                "profile-change request cannot be newer than the queue observation",
            ));
        }
    }

    let mut request_ids = BTreeSet::new();
    for request in &input.queued {
        validate_request(request, input.observed_at)?;
        if !request_ids.insert(request.identity.request_id.clone()) {
            return Err(PersonalWorkerQueueError::new(
                "identity.request_id",
                "duplicate_request_identity",
                "queue request identity must be unique",
            ));
        }
    }
    for reservation in &input.active {
        validate_active(reservation, input.observed_at)?;
        if !request_ids.insert(reservation.request.identity.request_id.clone()) {
            return Err(PersonalWorkerQueueError::new(
                "identity.request_id",
                "duplicate_request_identity",
                "queued and active request identities must be unique",
            ));
        }
    }

    aggregate_active_limits(&input.active)?;
    validate_active_leases(&input.active)?;
    Ok(())
}

fn validate_request(
    request: &PersonalWorkerJobRequest,
    observed_at: EpochMillis,
) -> Result<(), PersonalWorkerQueueError> {
    if request.submitted_at > observed_at {
        return Err(PersonalWorkerQueueError::new(
            "submitted_at",
            "future_submission_time",
            "job submission cannot be newer than the queue observation",
        ));
    }
    if request
        .operator_deadline
        .is_some_and(|deadline| deadline <= request.submitted_at)
    {
        return Err(PersonalWorkerQueueError::new(
            "operator_deadline",
            "invalid_operator_deadline",
            "operator deadline must be later than job submission",
        ));
    }
    if let PersonalWorkerCancellationState::Cancelled { cancelled_at } = request.cancellation
        && (cancelled_at < request.submitted_at || cancelled_at > observed_at)
    {
        return Err(PersonalWorkerQueueError::new(
            "cancellation.cancelled_at",
            "invalid_cancellation_time",
            "cancellation time must be between submission and observation",
        ));
    }
    validate_limits(request.requested_limits)?;
    request
        .cache_namespace
        .validate_for_source(&request.source)?;
    validate_fallback(&request.identity, &request.fallback_eligibility)
}

fn validate_active(
    reservation: &PersonalWorkerActiveReservation,
    observed_at: EpochMillis,
) -> Result<(), PersonalWorkerQueueError> {
    validate_request(&reservation.request, observed_at)?;
    let admission_observed_at = reservation.admission.observed_at();
    if admission_observed_at < reservation.request.submitted_at
        || admission_observed_at > observed_at
    {
        return Err(PersonalWorkerQueueError::new(
            "admission.observed_at",
            "active_admission_time_out_of_bounds",
            "active admission observation must be between submission and queue observation",
        ));
    }
    if let PersonalWorkerCancellationState::Cancelled { cancelled_at } =
        reservation.request.cancellation
        && cancelled_at > admission_observed_at
    {
        return Err(PersonalWorkerQueueError::new(
            "cancellation.cancelled_at",
            "cancellation_after_admission_observation",
            "draining admission must be observed at or after cancellation",
        ));
    }
    if reservation.request.cancellation.is_cancelled()
        && reservation.admission.state() != ExecutionAdmissionState::Draining
    {
        return Err(PersonalWorkerQueueError::new(
            "cancellation",
            "active_cancellation_requires_drain",
            "cancelled active work must be observed in draining admission state",
        ));
    }
    if reservation.admission.identity() != &reservation.request.identity {
        return Err(PersonalWorkerQueueError::new(
            "admission.identity",
            "active_admission_identity_drift",
            "active admission identity must match the exact queue request",
        ));
    }
    if reservation.admission.requested_limits() != reservation.request.requested_limits {
        return Err(PersonalWorkerQueueError::new(
            "admission.requested_limits",
            "active_requested_limits_drift",
            "active admission limits must match the exact queue request",
        ));
    }
    if reservation.admission.fallback_eligibility() != &reservation.request.fallback_eligibility {
        return Err(PersonalWorkerQueueError::new(
            "admission.fallback_eligibility",
            "active_fallback_eligibility_drift",
            "active admission fallback eligibility must match the exact queue request",
        ));
    }
    if !matches!(
        reservation.admission.state(),
        ExecutionAdmissionState::Reserved
            | ExecutionAdmissionState::Starting
            | ExecutionAdmissionState::Running
            | ExecutionAdmissionState::Draining
    ) {
        return Err(PersonalWorkerQueueError::new(
            "admission.state",
            "invalid_active_admission_state",
            "active worker reservations require reserved, starting, running, or draining state",
        ));
    }
    let evidence = reservation.admission.reservation().ok_or_else(|| {
        PersonalWorkerQueueError::new(
            "admission.reservation",
            "missing_active_reservation",
            "active admission requires exact reservation evidence",
        )
    })?;
    if evidence.expires_at <= observed_at {
        return Err(PersonalWorkerQueueError::new(
            "admission.reservation.expires_at",
            "expired_active_reservation",
            "active reservation must not be expired at queue observation time",
        ));
    }
    let applied = reservation.admission.applied_limits().ok_or_else(|| {
        PersonalWorkerQueueError::new(
            "admission.applied_limits",
            "missing_active_applied_limits",
            "active admission requires applied resource limits",
        )
    })?;
    validate_limits(applied)?;
    if applied != reservation.request.requested_limits {
        return Err(PersonalWorkerQueueError::new(
            "admission.applied_limits",
            "active_applied_limits_drift",
            "personal-worker reservation must apply the exact requested limits",
        ));
    }
    match reservation.admission.state() {
        ExecutionAdmissionState::Reserved => {
            if reservation.started_at.is_some() {
                return Err(PersonalWorkerQueueError::new(
                    "started_at",
                    "premature_start_time",
                    "reserved work must not carry a start time",
                ));
            }
        }
        ExecutionAdmissionState::Starting
        | ExecutionAdmissionState::Running
        | ExecutionAdmissionState::Draining => {
            if reservation.started_at.is_none_or(|started| {
                started < evidence.reserved_at || started > admission_observed_at
            }) {
                return Err(PersonalWorkerQueueError::new(
                    "started_at",
                    "invalid_active_start_time",
                    "started work requires a bounded time after reservation and before admission observation",
                ));
            }
        }
        _ => unreachable!("active state validated above"),
    }
    Ok(())
}

fn validate_limits(limits: ExecutionResourceLimits) -> Result<(), PersonalWorkerQueueError> {
    if limits.cpu_millis > PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS
        || limits.memory_bytes > PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES
        || limits.pids > PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY
    {
        return Err(PersonalWorkerQueueError::new(
            "requested_limits",
            "personal_worker_reserve_violation",
            "job limits must preserve the fixed guest, listener, Podman, and cache reserve",
        ));
    }
    Ok(())
}

fn validate_fallback(
    identity: &ExecutionAdmissionIdentity,
    fallback: &FallbackProfileEligibility,
) -> Result<(), PersonalWorkerQueueError> {
    if let FallbackProfileEligibility::Eligible { runner_profile_id } = fallback
        && runner_profile_id == &identity.runner_profile_id
    {
        return Err(PersonalWorkerQueueError::new(
            "fallback_eligibility.runner_profile_id",
            "fallback_profile_matches_primary",
            "fallback profile must differ from the personal worker profile",
        ));
    }
    Ok(())
}

fn aggregate_active_limits(
    active: &[PersonalWorkerActiveReservation],
) -> Result<ExecutionResourceLimits, PersonalWorkerQueueError> {
    let mut cpu_millis = 0_u32;
    let mut memory_bytes = 0_u64;
    let mut pids = 0_u32;
    let mut heavy_count = 0_usize;
    for reservation in active {
        let limits = reservation.request.requested_limits;
        cpu_millis = cpu_millis.checked_add(limits.cpu_millis).ok_or_else(|| {
            PersonalWorkerQueueError::new(
                "active.requested_limits.cpu_millis",
                "active_cpu_overflow",
                "active CPU reservations exceed the bounded aggregate",
            )
        })?;
        memory_bytes = memory_bytes
            .checked_add(limits.memory_bytes)
            .ok_or_else(|| {
                PersonalWorkerQueueError::new(
                    "active.requested_limits.memory_bytes",
                    "active_memory_overflow",
                    "active memory reservations exceed the bounded aggregate",
                )
            })?;
        pids = pids.checked_add(limits.pids).ok_or_else(|| {
            PersonalWorkerQueueError::new(
                "active.requested_limits.pids",
                "active_pid_overflow",
                "active PID reservations exceed the bounded aggregate",
            )
        })?;
        if reservation.request.job_class() == PersonalWorkerJobClass::Heavy {
            heavy_count += 1;
        }
    }
    if cpu_millis > PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS
        || memory_bytes > PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES
        || pids > PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY
        || heavy_count > 1
        || (heavy_count == 1 && active.len() > 1)
    {
        return Err(PersonalWorkerQueueError::new(
            "active",
            "active_resource_overcommit",
            "active reservations must fit one heavy job or at most two light jobs",
        ));
    }
    Ok(ExecutionResourceLimits {
        cpu_millis,
        memory_bytes,
        pids,
    })
}

fn validate_active_leases(
    active: &[PersonalWorkerActiveReservation],
) -> Result<(), PersonalWorkerQueueError> {
    let mut leases =
        BTreeMap::<PersonalWorkerCacheNamespace, Vec<PersonalWorkerCacheAccessMode>>::new();
    for reservation in active {
        let modes = leases
            .entry(reservation.request.cache_namespace.clone())
            .or_default();
        if lease_conflict_for_modes(modes, reservation.request.cache_access).is_some() {
            return Err(PersonalWorkerQueueError::new(
                "active.cache_access",
                "active_cache_lease_conflict",
                "active cache leases conflict for one namespace",
            ));
        }
        modes.push(reservation.request.cache_access);
    }
    Ok(())
}

fn active_leases(
    active: &[PersonalWorkerActiveReservation],
) -> BTreeMap<PersonalWorkerCacheNamespace, Vec<PersonalWorkerCacheAccessMode>> {
    let mut leases = BTreeMap::new();
    for reservation in active {
        leases
            .entry(reservation.request.cache_namespace.clone())
            .or_insert_with(Vec::new)
            .push(reservation.request.cache_access);
    }
    leases
}

fn active_repository_load(
    active: &[PersonalWorkerActiveReservation],
) -> BTreeMap<RepositoryRef, usize> {
    let mut load = BTreeMap::new();
    for reservation in active {
        *load
            .entry(reservation.request.source.repository.clone())
            .or_default() += 1;
    }
    load
}

fn compare_candidates(
    left: &PersonalWorkerJobRequest,
    right: &PersonalWorkerJobRequest,
    observed_at: EpochMillis,
    repository_load: &BTreeMap<RepositoryRef, usize>,
) -> Ordering {
    let left_rank = effective_priority(left, observed_at);
    let right_rank = effective_priority(right, observed_at);
    right_rank
        .cmp(&left_rank)
        .then_with(|| {
            repository_load
                .get(&left.source.repository)
                .copied()
                .unwrap_or_default()
                .cmp(
                    &repository_load
                        .get(&right.source.repository)
                        .copied()
                        .unwrap_or_default(),
                )
        })
        .then_with(|| compare_deadline(left.operator_deadline, right.operator_deadline))
        .then_with(|| left.submitted_at.cmp(&right.submitted_at))
        .then_with(|| left.source.repository.cmp(&right.source.repository))
        .then_with(|| left.identity.request_id.cmp(&right.identity.request_id))
}

fn compare_deadline(left: Option<EpochMillis>, right: Option<EpochMillis>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn effective_priority(request: &PersonalWorkerJobRequest, observed_at: EpochMillis) -> u8 {
    request
        .priority
        .effective_rank(observed_at.get().saturating_sub(request.submitted_at.get()))
}

fn lease_conflict(
    held: &BTreeMap<PersonalWorkerCacheNamespace, Vec<PersonalWorkerCacheAccessMode>>,
    namespace: &PersonalWorkerCacheNamespace,
    requested: PersonalWorkerCacheAccessMode,
) -> Option<PersonalWorkerCacheLeaseState> {
    lease_conflict_for_modes(
        held.get(namespace).map(Vec::as_slice).unwrap_or_default(),
        requested,
    )
}

fn lease_conflict_for_modes(
    held: &[PersonalWorkerCacheAccessMode],
    requested: PersonalWorkerCacheAccessMode,
) -> Option<PersonalWorkerCacheLeaseState> {
    if held.is_empty() {
        return None;
    }
    if held.contains(&PersonalWorkerCacheAccessMode::Exclusive) {
        return Some(PersonalWorkerCacheLeaseState::BlockedByExclusive);
    }
    if requested == PersonalWorkerCacheAccessMode::Exclusive {
        return Some(PersonalWorkerCacheLeaseState::BlockedByExclusive);
    }
    if held.contains(&PersonalWorkerCacheAccessMode::Write)
        || requested == PersonalWorkerCacheAccessMode::Write
    {
        return Some(PersonalWorkerCacheLeaseState::BlockedByWriter);
    }
    None
}

fn desired_profile(input: &PersonalWorkerQueueInput, has_work: bool) -> PersonalWorkerProfile {
    if has_work {
        return PersonalWorkerProfile::Work;
    }
    let Some(last_activity_at) = input.activity_evidence.last_activity_at() else {
        return PersonalWorkerProfile::Stopped;
    };
    let idle_millis = input
        .observed_at
        .get()
        .saturating_sub(last_activity_at.get());
    if idle_millis < PERSONAL_WORKER_INTERACTIVE_COOLDOWN_MILLIS {
        PersonalWorkerProfile::Work
    } else if idle_millis < PERSONAL_WORKER_STOPPED_COOLDOWN_MILLIS {
        PersonalWorkerProfile::Interactive
    } else {
        PersonalWorkerProfile::Stopped
    }
}

fn build_visibility(
    input: &PersonalWorkerQueueInput,
    selected: &[PersonalWorkerSelection],
    desired_profile: PersonalWorkerProfile,
) -> Vec<PersonalWorkerQueueVisibility> {
    let selected_ids = selected
        .iter()
        .map(|selection| selection.request_id.clone())
        .collect::<BTreeSet<_>>();
    let mut held = active_leases(&input.active);
    for selection in selected {
        held.entry(selection.cache_namespace.clone())
            .or_default()
            .push(selection.cache_access);
    }
    let mut queue_repository_load = active_repository_load(&input.active);
    for selection in selected {
        *queue_repository_load
            .entry(selection.repository.clone())
            .or_default() += 1;
    }
    let mut queued = input.queued.iter().collect::<Vec<_>>();
    queued.sort_by(|left, right| {
        compare_candidates(left, right, input.observed_at, &queue_repository_load)
    });
    let queue_positions = queued
        .iter()
        .filter(|request| {
            !request.cancellation.is_cancelled()
                && !selected_ids.contains(&request.identity.request_id)
        })
        .enumerate()
        .map(|(index, request)| (request.identity.request_id.clone(), index + 1))
        .collect::<BTreeMap<_, _>>();

    let mut visibility = Vec::with_capacity(input.queued.len() + input.active.len());
    for request in queued {
        let selected = selected_ids.contains(&request.identity.request_id);
        let state = if request.cancellation.is_cancelled() {
            PersonalWorkerQueueEntryState::Cancelled
        } else if selected {
            PersonalWorkerQueueEntryState::Selected
        } else {
            PersonalWorkerQueueEntryState::Queued
        };
        let cache_lease = if request.cancellation.is_cancelled() {
            PersonalWorkerCacheLeaseState::Available
        } else if selected {
            held_lease_state(request.cache_access)
        } else {
            lease_conflict(&held, &request.cache_namespace, request.cache_access).unwrap_or_else(
                || {
                    if request.cache_access == PersonalWorkerCacheAccessMode::Read {
                        PersonalWorkerCacheLeaseState::SharedRead
                    } else {
                        PersonalWorkerCacheLeaseState::Available
                    }
                },
            )
        };
        visibility.push(visibility_for_request(
            request,
            input.observed_at,
            state,
            queue_positions
                .get(&request.identity.request_id)
                .and_then(|position| u32::try_from(*position).ok()),
            if selected {
                Some(request.requested_limits)
            } else {
                None
            },
            cache_lease,
            None,
            desired_profile,
        ));
    }
    for active in &input.active {
        let state = match active.admission.state() {
            ExecutionAdmissionState::Reserved => PersonalWorkerQueueEntryState::Reserved,
            ExecutionAdmissionState::Starting => PersonalWorkerQueueEntryState::Starting,
            ExecutionAdmissionState::Running => PersonalWorkerQueueEntryState::Running,
            ExecutionAdmissionState::Draining => PersonalWorkerQueueEntryState::Draining,
            _ => unreachable!("active state validated before visibility"),
        };
        visibility.push(visibility_for_request(
            &active.request,
            input.observed_at,
            state,
            None,
            active.admission.applied_limits(),
            held_lease_state(active.request.cache_access),
            active.started_at,
            PersonalWorkerProfile::Work,
        ));
    }
    visibility
}

fn held_lease_state(access: PersonalWorkerCacheAccessMode) -> PersonalWorkerCacheLeaseState {
    match access {
        PersonalWorkerCacheAccessMode::Read => PersonalWorkerCacheLeaseState::HeldRead,
        PersonalWorkerCacheAccessMode::Write => PersonalWorkerCacheLeaseState::HeldWrite,
        PersonalWorkerCacheAccessMode::Exclusive => PersonalWorkerCacheLeaseState::HeldExclusive,
    }
}

#[allow(clippy::too_many_arguments)]
fn visibility_for_request(
    request: &PersonalWorkerJobRequest,
    observed_at: EpochMillis,
    state: PersonalWorkerQueueEntryState,
    queue_position: Option<u32>,
    reserved_limits: Option<ExecutionResourceLimits>,
    cache_lease: PersonalWorkerCacheLeaseState,
    start_time: Option<EpochMillis>,
    worker_profile: PersonalWorkerProfile,
) -> PersonalWorkerQueueVisibility {
    PersonalWorkerQueueVisibility {
        request_id: request.identity.request_id.clone(),
        repository: request.source.repository.clone(),
        commit: request.source.commit.clone(),
        tree: request.source.tree.clone(),
        verification_profile_id: request.identity.verification_profile_id.clone(),
        runner_profile_id: request.identity.runner_profile_id.clone(),
        priority: request.priority,
        effective_priority_rank: effective_priority(request, observed_at),
        age_millis: observed_at.get().saturating_sub(request.submitted_at.get()),
        state,
        queue_position,
        requested_cpu_millis: request.requested_limits.cpu_millis,
        requested_memory_bytes: request.requested_limits.memory_bytes,
        reserved_cpu_millis: reserved_limits.map(|limits| limits.cpu_millis),
        reserved_memory_bytes: reserved_limits.map(|limits| limits.memory_bytes),
        cache_namespace: request.cache_namespace.clone(),
        cache_access: request.cache_access,
        cache_lease,
        start_time,
        worker_profile,
    }
}
