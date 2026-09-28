//! Pure correlation of one repository-owned parallel verifier with one personal-worker attempt.
//!
//! Repository-internal work units remain bounded receipt detail. They never become queue requests,
//! reservations, cache leases, resource claims, cleanup authority, or independent attempts. This
//! module performs no execution, filesystem access, receipt-channel I/O, persistence, signalling,
//! scheduling, retry, cache mutation, or lease release.

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::execution_admission::{
    EpochMillis, ExecutionRequestId, ExecutionResourceLimits, ReservationGeneration, ReservationId,
};
use crate::personal_worker_queue::{
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerQueueGeneration,
    PersonalWorkerSourceIdentity,
};
use crate::personal_worker_store::PersonalWorkerStoreRevision;
use crate::verification_profile::{RepositoryCommandIdentity, VerificationProfileId};

pub const PERSONAL_WORKER_REPOSITORY_RESULT_SCHEMA_VERSION: u8 = 1;
pub const MAX_REPOSITORY_WORK_UNITS: usize = 512;
pub const MAX_REPOSITORY_CONCURRENCY: u16 = 512;
pub const MAX_REPOSITORY_RECEIPT_BYTES: u64 = 1_048_576;
pub const MAX_REPOSITORY_WORK_UNIT_WALL_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;

const MAX_ID_BYTES: usize = 96;
const MAX_ATTEMPT_GENERATION: u64 = 1_000_000_000_000;
const MAX_PRODUCER_SCHEMA_VERSION: u32 = 1_000_000;
const ATTEMPT_ID_PREFIX: &str = "pw-job-attempt-v1-";
const CHANNEL_ID_PREFIX: &str = "repository-receipt-channel-v1-";

macro_rules! public_token_type {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded public identifier without path or whitespace syntax.
            ///
            /// # Errors
            ///
            /// Returns an error for an empty, oversized, non-ASCII, or path-shaped value.
            pub fn parse(value: &str) -> Result<Self, PersonalWorkerRepositoryResultError> {
                if !valid_public_token(value) {
                    return Err(PersonalWorkerRepositoryResultError::invalid_identity(
                        $field,
                    ));
                }
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

public_token_type!(RepositoryVerifierProducerId, "receipt.producer_id");
public_token_type!(RepositoryWorkUnitId, "receipt.work_units.id");

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PersonalWorkerJobAttemptId(String);

impl PersonalWorkerJobAttemptId {
    /// Parse one opaque personal-worker job-attempt identity.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is the fixed prefix plus 64 lowercase hexadecimal bytes.
    pub fn parse(value: &str) -> Result<Self, PersonalWorkerRepositoryResultError> {
        if !valid_opaque_identity(value, ATTEMPT_ID_PREFIX) {
            return Err(PersonalWorkerRepositoryResultError::invalid_identity(
                "attempt.id",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PersonalWorkerJobAttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PersonalWorkerJobAttemptId(<opaque>)")
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RepositoryReceiptChannelId(String);

impl RepositoryReceiptChannelId {
    /// Parse one opaque pre-opened repository receipt-channel identity.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is the fixed prefix plus 64 lowercase hexadecimal bytes.
    pub fn parse(value: &str) -> Result<Self, PersonalWorkerRepositoryResultError> {
        if !valid_opaque_identity(value, CHANNEL_ID_PREFIX) {
            return Err(PersonalWorkerRepositoryResultError::invalid_identity(
                "receipt.channel_id",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RepositoryReceiptChannelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RepositoryReceiptChannelId(<opaque>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PersonalWorkerJobAttemptGeneration(u64);

impl PersonalWorkerJobAttemptGeneration {
    /// Construct one positive bounded job-attempt generation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or an implementation-exceeding generation.
    pub fn new(value: u64) -> Result<Self, PersonalWorkerRepositoryResultError> {
        if !(1..=MAX_ATTEMPT_GENERATION).contains(&value) {
            return Err(PersonalWorkerRepositoryResultError::invalid_identity(
                "attempt.generation",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RepositoryConcurrencyGrant(u16);

impl RepositoryConcurrencyGrant {
    /// Construct one explicit bounded repository-internal concurrency ceiling.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or a value above the public work-unit bound.
    pub fn new(value: u16) -> Result<Self, PersonalWorkerRepositoryResultError> {
        if value == 0 || value > MAX_REPOSITORY_CONCURRENCY {
            return Err(PersonalWorkerRepositoryResultError::invalid_binding(
                "resources.repository_concurrency",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryReceiptContract {
    producer_id: RepositoryVerifierProducerId,
    producer_schema_version: u32,
    channel_id: RepositoryReceiptChannelId,
    maximum_bytes: u64,
}

impl RepositoryReceiptContract {
    /// Declare the checked-in producer and bounded output channel before execution.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero/oversized producer schema or channel byte bound.
    pub fn new(
        producer_id: RepositoryVerifierProducerId,
        producer_schema_version: u32,
        channel_id: RepositoryReceiptChannelId,
        maximum_bytes: u64,
    ) -> Result<Self, PersonalWorkerRepositoryResultError> {
        if producer_schema_version == 0
            || producer_schema_version > MAX_PRODUCER_SCHEMA_VERSION
            || maximum_bytes == 0
            || maximum_bytes > MAX_REPOSITORY_RECEIPT_BYTES
        {
            return Err(PersonalWorkerRepositoryResultError::invalid_binding(
                "receipt.contract",
            ));
        }
        Ok(Self {
            producer_id,
            producer_schema_version,
            channel_id,
            maximum_bytes,
        })
    }

    #[must_use]
    pub const fn producer_id(&self) -> &RepositoryVerifierProducerId {
        &self.producer_id
    }

    #[must_use]
    pub const fn producer_schema_version(&self) -> u32 {
        self.producer_schema_version
    }

    #[must_use]
    pub const fn channel_id(&self) -> &RepositoryReceiptChannelId {
        &self.channel_id
    }

    #[must_use]
    pub const fn maximum_bytes(&self) -> u64 {
        self.maximum_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerRepositoryAttemptInput {
    pub request_id: ExecutionRequestId,
    pub attempt_id: PersonalWorkerJobAttemptId,
    pub attempt_generation: PersonalWorkerJobAttemptGeneration,
    pub predecessor_store_revision: PersonalWorkerStoreRevision,
    pub predecessor_queue_generation: PersonalWorkerQueueGeneration,
    pub source: PersonalWorkerSourceIdentity,
    pub verification_profile_id: VerificationProfileId,
    pub command: RepositoryCommandIdentity,
    pub toolchain_envelope_digest: Sha256Digest,
    pub requested_limits: ExecutionResourceLimits,
    pub applied_limits: ExecutionResourceLimits,
    pub repository_concurrency: RepositoryConcurrencyGrant,
    pub reservation_id: ReservationId,
    pub reservation_generation: ReservationGeneration,
    pub cache_namespace: PersonalWorkerCacheNamespace,
    pub cache_access: PersonalWorkerCacheAccessMode,
    pub cache_lease_acquired_at: EpochMillis,
    pub bound_at: EpochMillis,
    pub not_after: EpochMillis,
    pub receipt_contract: RepositoryReceiptContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRepositoryAttemptBinding {
    schema_version: u8,
    pub(crate) request_id: ExecutionRequestId,
    pub(crate) attempt_id: PersonalWorkerJobAttemptId,
    pub(crate) attempt_generation: PersonalWorkerJobAttemptGeneration,
    pub(crate) predecessor_store_revision: PersonalWorkerStoreRevision,
    pub(crate) predecessor_queue_generation: PersonalWorkerQueueGeneration,
    pub(crate) source: PersonalWorkerSourceIdentity,
    pub(crate) verification_profile_id: VerificationProfileId,
    pub(crate) command: RepositoryCommandIdentity,
    pub(crate) toolchain_envelope_digest: Sha256Digest,
    pub(crate) requested_limits: ExecutionResourceLimits,
    pub(crate) applied_limits: ExecutionResourceLimits,
    pub(crate) repository_concurrency: RepositoryConcurrencyGrant,
    pub(crate) reservation_id: ReservationId,
    pub(crate) reservation_generation: ReservationGeneration,
    pub(crate) cache_namespace: PersonalWorkerCacheNamespace,
    pub(crate) cache_access: PersonalWorkerCacheAccessMode,
    pub(crate) cache_lease_acquired_at: EpochMillis,
    pub(crate) bound_at: EpochMillis,
    pub(crate) not_after: EpochMillis,
    pub(crate) receipt_contract: RepositoryReceiptContract,
}

impl PersonalWorkerRepositoryAttemptBinding {
    #[must_use]
    pub const fn request_id(&self) -> &ExecutionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn attempt_id(&self) -> &PersonalWorkerJobAttemptId {
        &self.attempt_id
    }

    #[must_use]
    pub const fn attempt_generation(&self) -> PersonalWorkerJobAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn predecessor_store_revision(&self) -> PersonalWorkerStoreRevision {
        self.predecessor_store_revision
    }

    #[must_use]
    pub const fn predecessor_queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.predecessor_queue_generation
    }

    #[must_use]
    pub const fn source(&self) -> &PersonalWorkerSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn repository_concurrency(&self) -> RepositoryConcurrencyGrant {
        self.repository_concurrency
    }

    #[must_use]
    pub const fn receipt_contract(&self) -> &RepositoryReceiptContract {
        &self.receipt_contract
    }
}

/// Seal one path-free attempt/result correlation contract before repository execution.
///
/// This declaration grants no execution or channel authority. The later B06 adapter must construct
/// it only after binding the exact attempt and pre-opening the bounded receipt output channel.
///
/// # Errors
///
/// Returns an error when source/command/cache, limits/concurrency, or timeline facts conflict.
pub fn bind_personal_worker_repository_attempt(
    input: PersonalWorkerRepositoryAttemptInput,
) -> Result<PersonalWorkerRepositoryAttemptBinding, PersonalWorkerRepositoryResultError> {
    if input.command.repository() != &input.source.repository {
        return Err(PersonalWorkerRepositoryResultError::source_mismatch());
    }
    if !valid_limits(input.requested_limits)
        || !valid_limits(input.applied_limits)
        || !input.applied_limits.fits_within(input.requested_limits)
        || u32::from(input.repository_concurrency.get()) > input.applied_limits.pids
    {
        return Err(PersonalWorkerRepositoryResultError::resource_mismatch());
    }
    if let PersonalWorkerCacheNamespace::RepositoryBuild { repository, .. } = &input.cache_namespace
        && repository != &input.source.repository
    {
        return Err(PersonalWorkerRepositoryResultError::cache_mismatch());
    }
    if input.cache_lease_acquired_at > input.bound_at || input.bound_at >= input.not_after {
        return Err(PersonalWorkerRepositoryResultError::invalid_timeline());
    }

    Ok(PersonalWorkerRepositoryAttemptBinding {
        schema_version: PERSONAL_WORKER_REPOSITORY_RESULT_SCHEMA_VERSION,
        request_id: input.request_id,
        attempt_id: input.attempt_id,
        attempt_generation: input.attempt_generation,
        predecessor_store_revision: input.predecessor_store_revision,
        predecessor_queue_generation: input.predecessor_queue_generation,
        source: input.source,
        verification_profile_id: input.verification_profile_id,
        command: input.command,
        toolchain_envelope_digest: input.toolchain_envelope_digest,
        requested_limits: input.requested_limits,
        applied_limits: input.applied_limits,
        repository_concurrency: input.repository_concurrency,
        reservation_id: input.reservation_id,
        reservation_generation: input.reservation_generation,
        cache_namespace: input.cache_namespace,
        cache_access: input.cache_access,
        cache_lease_acquired_at: input.cache_lease_acquired_at,
        bound_at: input.bound_at,
        not_after: input.not_after,
        receipt_contract: input.receipt_contract,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryWorkUnitOutcome {
    Passed,
    Failed,
    CompileSetupFailed,
    TimedOut,
    Cancelled,
    NotStarted,
    ResourceExhausted,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryWorkUnitRecord {
    pub id: RepositoryWorkUnitId,
    pub command_digest: Sha256Digest,
    pub wall_millis: u64,
    pub outcome: RepositoryWorkUnitOutcome,
    pub output_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "retention", rename_all = "snake_case")]
pub enum RepositoryWorkDetail {
    WorkUnits {
        work_units: Vec<RepositoryWorkUnitRecord>,
    },
    DetailDigest {
        work_unit_count: u32,
        detail_digest: Sha256Digest,
    },
}

impl RepositoryWorkDetail {
    #[must_use]
    pub fn work_unit_count(&self) -> usize {
        match self {
            Self::WorkUnits { work_units } => work_units.len(),
            Self::DetailDigest {
                work_unit_count, ..
            } => usize::try_from(*work_unit_count).unwrap_or(usize::MAX),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReceiptTerminalClass {
    Passed,
    VerificationFailed,
    CompileSetupFailed,
    Timeout,
    Cancelled,
    ResourceExhausted,
    DiagnosticInconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryProcessTerminalClass {
    ExitedSuccess,
    ExitedFailure,
    Timeout,
    Cancelled,
    ResourceExhausted,
    RunnerLost,
    DiagnosticInconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRepositoryTerminalClass {
    Passed,
    RepositoryVerificationFailed,
    CompileSetupFailed,
    Timeout,
    Cancelled,
    ResourceExhausted,
    RunnerLost,
    ReceiptMissing,
    ReceiptMalformed,
    CleanupIncomplete,
    DiagnosticInconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryStopReason {
    OperatorRequested,
    RequestSuperseded,
    DeadlineExceeded,
    RunnerDrain,
    HostShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositorySignalClass {
    Interrupt,
    Quit,
    Terminate,
    Kill,
    Abort,
    CpuTime,
    FileSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RepositoryStopEvidence {
    pub reason: RepositoryStopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<RepositorySignalClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryResourceExhaustionClass {
    Memory,
    Pids,
    CpuTime,
    OutputLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryResourceExhaustionEvidence {
    pub class: RepositoryResourceExhaustionClass,
    pub observation_digest: Sha256Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCleanupDisposition {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryAggregateReceipt {
    pub request_id: ExecutionRequestId,
    pub attempt_id: PersonalWorkerJobAttemptId,
    pub attempt_generation: PersonalWorkerJobAttemptGeneration,
    pub predecessor_store_revision: PersonalWorkerStoreRevision,
    pub predecessor_queue_generation: PersonalWorkerQueueGeneration,
    pub source: PersonalWorkerSourceIdentity,
    pub verification_profile_id: VerificationProfileId,
    pub command: RepositoryCommandIdentity,
    pub toolchain_envelope_digest: Sha256Digest,
    pub requested_limits: ExecutionResourceLimits,
    pub applied_limits: ExecutionResourceLimits,
    pub repository_concurrency: RepositoryConcurrencyGrant,
    pub reservation_id: ReservationId,
    pub reservation_generation: ReservationGeneration,
    pub cache_namespace: PersonalWorkerCacheNamespace,
    pub cache_access: PersonalWorkerCacheAccessMode,
    pub cache_lease_acquired_at: EpochMillis,
    pub not_after: EpochMillis,
    pub producer_id: RepositoryVerifierProducerId,
    pub producer_schema_version: u32,
    pub receipt_digest: Sha256Digest,
    pub aggregate_started_at: EpochMillis,
    pub aggregate_terminal_at: EpochMillis,
    pub terminal_class: RepositoryReceiptTerminalClass,
    pub maximum_parallelism_observed: u16,
    pub work_detail: RepositoryWorkDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<RepositoryStopEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_exhaustion: Option<RepositoryResourceExhaustionEvidence>,
    pub repository_cleanup: RepositoryCleanupDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "assessment", rename_all = "snake_case")]
pub enum RepositoryReceiptAssessment {
    Present {
        receipt: Box<RepositoryAggregateReceipt>,
    },
    Missing,
    Malformed {
        #[serde(skip_serializing_if = "Option::is_none")]
        observed_digest: Option<Sha256Digest>,
    },
}

impl RepositoryReceiptAssessment {
    #[must_use]
    pub fn present(receipt: RepositoryAggregateReceipt) -> Self {
        Self::Present {
            receipt: Box::new(receipt),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RepositoryReceiptChannelObservation {
    Captured {
        channel_id: RepositoryReceiptChannelId,
        bytes_written: u64,
        digest: Sha256Digest,
    },
    Empty {
        channel_id: RepositoryReceiptChannelId,
    },
    Malformed {
        channel_id: RepositoryReceiptChannelId,
        bytes_written: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        observed_digest: Option<Sha256Digest>,
    },
    Overflow {
        channel_id: RepositoryReceiptChannelId,
        bytes_observed_at_least: u64,
    },
}

impl RepositoryReceiptChannelObservation {
    const fn channel_id(&self) -> &RepositoryReceiptChannelId {
        match self {
            Self::Captured { channel_id, .. }
            | Self::Empty { channel_id }
            | Self::Malformed { channel_id, .. }
            | Self::Overflow { channel_id, .. } => channel_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerOuterTerminalEvidence {
    pub request_id: ExecutionRequestId,
    pub attempt_id: PersonalWorkerJobAttemptId,
    pub attempt_generation: PersonalWorkerJobAttemptGeneration,
    pub reservation_id: ReservationId,
    pub reservation_generation: ReservationGeneration,
    pub started_at: EpochMillis,
    pub terminal_at: EpochMillis,
    pub process_terminal: RepositoryProcessTerminalClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<RepositoryStopEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_exhaustion: Option<RepositoryResourceExhaustionEvidence>,
    pub outer_cleanup: RepositoryCleanupDisposition,
    pub receipt_channel: RepositoryReceiptChannelObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRepositoryCompletionInput {
    schema_version: u8,
    binding: PersonalWorkerRepositoryAttemptBinding,
    outer: PersonalWorkerOuterTerminalEvidence,
    repository_receipt: RepositoryReceiptAssessment,
    terminal_class: PersonalWorkerRepositoryTerminalClass,
}

impl PersonalWorkerRepositoryCompletionInput {
    #[must_use]
    pub const fn binding(&self) -> &PersonalWorkerRepositoryAttemptBinding {
        &self.binding
    }

    #[must_use]
    pub const fn outer(&self) -> &PersonalWorkerOuterTerminalEvidence {
        &self.outer
    }

    #[must_use]
    pub const fn repository_receipt(&self) -> &RepositoryReceiptAssessment {
        &self.repository_receipt
    }

    #[must_use]
    pub const fn terminal_class(&self) -> PersonalWorkerRepositoryTerminalClass {
        self.terminal_class
    }
}

/// Correlate repository and outer terminal evidence into one B07-ready completion input.
///
/// Missing, malformed, failed, timed-out, cancelled, exhausted, lost-runner, and inconclusive
/// observations still produce typed terminal evidence when their outer attempt binding is valid.
/// Success is impossible unless the repository receipt, process outcome, bounded channel, exact
/// identities, concurrency observation, and both cleanup scopes agree.
///
/// # Errors
///
/// Returns an error for identity, source, profile/command/toolchain, resource, reservation/cache,
/// deadline, timeline, receipt-channel, work-detail, cancellation, or terminal drift.
pub fn correlate_personal_worker_repository_result(
    binding: PersonalWorkerRepositoryAttemptBinding,
    outer: PersonalWorkerOuterTerminalEvidence,
    repository_receipt: RepositoryReceiptAssessment,
) -> Result<PersonalWorkerRepositoryCompletionInput, PersonalWorkerRepositoryResultError> {
    validate_outer(&binding, &outer)?;
    let terminal_class = match &repository_receipt {
        RepositoryReceiptAssessment::Present { receipt } => {
            validate_present_receipt(&binding, &outer, receipt)?;
            classify_present(&outer, receipt)?
        }
        RepositoryReceiptAssessment::Missing => {
            validate_missing_receipt(&binding, &outer)?;
            classify_absent(
                &outer,
                PersonalWorkerRepositoryTerminalClass::ReceiptMissing,
            )
        }
        RepositoryReceiptAssessment::Malformed { observed_digest } => {
            validate_malformed_receipt(&binding, &outer, observed_digest.as_ref())?;
            classify_absent(
                &outer,
                PersonalWorkerRepositoryTerminalClass::ReceiptMalformed,
            )
        }
    };

    Ok(PersonalWorkerRepositoryCompletionInput {
        schema_version: PERSONAL_WORKER_REPOSITORY_RESULT_SCHEMA_VERSION,
        binding,
        outer,
        repository_receipt,
        terminal_class,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCompletionReplayDisposition {
    New,
    ExactReplay,
}

/// Classify one pure durable-completion proposal without mutating the personal-worker store.
///
/// Exact equality wins before predecessor-currentness checks so a response-loss retry remains
/// idempotent after the first durable publication advanced the store. A new value must bind the
/// caller's exact predecessor store and queue generations; any changed retained value conflicts.
///
/// # Errors
///
/// Returns an error for a stale new predecessor or any changed-input replay.
pub fn classify_repository_completion_replay(
    expected_store_revision: PersonalWorkerStoreRevision,
    expected_queue_generation: PersonalWorkerQueueGeneration,
    existing: Option<&PersonalWorkerRepositoryCompletionInput>,
    candidate: &PersonalWorkerRepositoryCompletionInput,
) -> Result<RepositoryCompletionReplayDisposition, PersonalWorkerRepositoryResultError> {
    if let Some(existing) = existing {
        return if existing == candidate {
            Ok(RepositoryCompletionReplayDisposition::ExactReplay)
        } else {
            Err(PersonalWorkerRepositoryResultError::changed_replay())
        };
    }
    if candidate.binding.predecessor_store_revision != expected_store_revision
        || candidate.binding.predecessor_queue_generation != expected_queue_generation
    {
        return Err(PersonalWorkerRepositoryResultError::stale_predecessor());
    }
    Ok(RepositoryCompletionReplayDisposition::New)
}

fn validate_outer(
    binding: &PersonalWorkerRepositoryAttemptBinding,
    outer: &PersonalWorkerOuterTerminalEvidence,
) -> Result<(), PersonalWorkerRepositoryResultError> {
    if outer.request_id != binding.request_id
        || outer.attempt_id != binding.attempt_id
        || outer.attempt_generation != binding.attempt_generation
    {
        return Err(PersonalWorkerRepositoryResultError::attempt_mismatch());
    }
    if outer.reservation_id != binding.reservation_id
        || outer.reservation_generation != binding.reservation_generation
    {
        return Err(PersonalWorkerRepositoryResultError::reservation_mismatch());
    }
    if outer.started_at < binding.bound_at || outer.terminal_at < outer.started_at {
        return Err(PersonalWorkerRepositoryResultError::invalid_timeline());
    }
    if outer.process_terminal == RepositoryProcessTerminalClass::ExitedSuccess
        && outer.terminal_at > binding.not_after
    {
        return Err(PersonalWorkerRepositoryResultError::deadline_mismatch());
    }
    match (outer.process_terminal, &outer.stop) {
        (RepositoryProcessTerminalClass::Timeout, Some(stop))
            if stop.reason == RepositoryStopReason::DeadlineExceeded => {}
        (RepositoryProcessTerminalClass::Cancelled, Some(stop))
            if stop.reason != RepositoryStopReason::DeadlineExceeded => {}
        (
            RepositoryProcessTerminalClass::Timeout | RepositoryProcessTerminalClass::Cancelled,
            _,
        ) => {
            return Err(PersonalWorkerRepositoryResultError::cancellation_mismatch());
        }
        (_, None) => {}
        (_, Some(_)) => {
            return Err(PersonalWorkerRepositoryResultError::cancellation_mismatch());
        }
    }
    if (outer.process_terminal == RepositoryProcessTerminalClass::ResourceExhausted)
        != outer.resource_exhaustion.is_some()
    {
        return Err(PersonalWorkerRepositoryResultError::resource_mismatch());
    }
    if outer.receipt_channel.channel_id() != binding.receipt_contract.channel_id() {
        return Err(PersonalWorkerRepositoryResultError::receipt_contract_mismatch());
    }
    Ok(())
}

fn validate_present_receipt(
    binding: &PersonalWorkerRepositoryAttemptBinding,
    outer: &PersonalWorkerOuterTerminalEvidence,
    receipt: &RepositoryAggregateReceipt,
) -> Result<(), PersonalWorkerRepositoryResultError> {
    if receipt.request_id != binding.request_id
        || receipt.attempt_id != binding.attempt_id
        || receipt.attempt_generation != binding.attempt_generation
        || receipt.predecessor_store_revision != binding.predecessor_store_revision
        || receipt.predecessor_queue_generation != binding.predecessor_queue_generation
    {
        return Err(PersonalWorkerRepositoryResultError::attempt_mismatch());
    }
    if receipt.reservation_id != binding.reservation_id
        || receipt.reservation_generation != binding.reservation_generation
    {
        return Err(PersonalWorkerRepositoryResultError::reservation_mismatch());
    }
    if receipt.source != binding.source {
        return Err(PersonalWorkerRepositoryResultError::source_mismatch());
    }
    if receipt.verification_profile_id != binding.verification_profile_id
        || receipt.command != binding.command
        || receipt.toolchain_envelope_digest != binding.toolchain_envelope_digest
    {
        return Err(PersonalWorkerRepositoryResultError::verification_mismatch());
    }
    if receipt.requested_limits != binding.requested_limits
        || receipt.applied_limits != binding.applied_limits
        || receipt.repository_concurrency != binding.repository_concurrency
    {
        return Err(PersonalWorkerRepositoryResultError::resource_mismatch());
    }
    if receipt.cache_namespace != binding.cache_namespace
        || receipt.cache_access != binding.cache_access
        || receipt.cache_lease_acquired_at != binding.cache_lease_acquired_at
    {
        return Err(PersonalWorkerRepositoryResultError::cache_mismatch());
    }
    if receipt.not_after != binding.not_after {
        return Err(PersonalWorkerRepositoryResultError::deadline_mismatch());
    }
    if receipt.producer_id != *binding.receipt_contract.producer_id()
        || receipt.producer_schema_version != binding.receipt_contract.producer_schema_version()
    {
        return Err(PersonalWorkerRepositoryResultError::receipt_contract_mismatch());
    }
    let RepositoryReceiptChannelObservation::Captured {
        bytes_written,
        digest,
        ..
    } = &outer.receipt_channel
    else {
        return Err(PersonalWorkerRepositoryResultError::receipt_contract_mismatch());
    };
    if *bytes_written == 0
        || *bytes_written > binding.receipt_contract.maximum_bytes()
        || digest != &receipt.receipt_digest
    {
        return Err(PersonalWorkerRepositoryResultError::receipt_contract_mismatch());
    }
    if receipt.aggregate_started_at < outer.started_at
        || receipt.aggregate_terminal_at < receipt.aggregate_started_at
        || receipt.aggregate_terminal_at > outer.terminal_at
        || (receipt.terminal_class == RepositoryReceiptTerminalClass::Passed
            && receipt.aggregate_terminal_at > binding.not_after)
    {
        return Err(PersonalWorkerRepositoryResultError::invalid_timeline());
    }
    validate_work_detail(receipt, binding.repository_concurrency)?;
    match (receipt.terminal_class, &receipt.stop) {
        (RepositoryReceiptTerminalClass::Timeout, Some(stop))
            if stop.reason == RepositoryStopReason::DeadlineExceeded
                && receipt.stop == outer.stop => {}
        (RepositoryReceiptTerminalClass::Cancelled, Some(stop))
            if stop.reason != RepositoryStopReason::DeadlineExceeded
                && receipt.stop == outer.stop => {}
        (
            RepositoryReceiptTerminalClass::Timeout | RepositoryReceiptTerminalClass::Cancelled,
            _,
        ) => {
            return Err(PersonalWorkerRepositoryResultError::cancellation_mismatch());
        }
        (_, None) => {}
        (_, Some(_)) => {
            return Err(PersonalWorkerRepositoryResultError::cancellation_mismatch());
        }
    }
    if (receipt.terminal_class == RepositoryReceiptTerminalClass::ResourceExhausted)
        != receipt.resource_exhaustion.is_some()
        || receipt.resource_exhaustion != outer.resource_exhaustion
    {
        return Err(PersonalWorkerRepositoryResultError::resource_mismatch());
    }
    Ok(())
}

fn validate_work_detail(
    receipt: &RepositoryAggregateReceipt,
    grant: RepositoryConcurrencyGrant,
) -> Result<(), PersonalWorkerRepositoryResultError> {
    let count = receipt.work_detail.work_unit_count();
    if count > MAX_REPOSITORY_WORK_UNITS
        || usize::from(receipt.maximum_parallelism_observed) > count
        || receipt.maximum_parallelism_observed > grant.get()
        || (count == 0 && receipt.maximum_parallelism_observed != 0)
        || (count > 0 && receipt.maximum_parallelism_observed == 0)
        || (receipt.terminal_class == RepositoryReceiptTerminalClass::Passed && count == 0)
    {
        return Err(PersonalWorkerRepositoryResultError::concurrency_exceeded());
    }
    let RepositoryWorkDetail::WorkUnits { work_units } = &receipt.work_detail else {
        return Ok(());
    };
    let aggregate_wall_millis = receipt
        .aggregate_terminal_at
        .get()
        .checked_sub(receipt.aggregate_started_at.get())
        .ok_or_else(PersonalWorkerRepositoryResultError::invalid_timeline)?;
    let mut identities = BTreeSet::new();
    for work_unit in work_units {
        if !identities.insert(&work_unit.id)
            || work_unit.wall_millis > MAX_REPOSITORY_WORK_UNIT_WALL_MILLIS
            || work_unit.wall_millis > aggregate_wall_millis
        {
            return Err(PersonalWorkerRepositoryResultError::invalid_work_detail());
        }
    }
    if receipt.terminal_class == RepositoryReceiptTerminalClass::Passed
        && work_units
            .iter()
            .any(|work_unit| work_unit.outcome != RepositoryWorkUnitOutcome::Passed)
    {
        return Err(PersonalWorkerRepositoryResultError::terminal_mismatch());
    }
    Ok(())
}

fn validate_missing_receipt(
    _binding: &PersonalWorkerRepositoryAttemptBinding,
    outer: &PersonalWorkerOuterTerminalEvidence,
) -> Result<(), PersonalWorkerRepositoryResultError> {
    if !matches!(
        outer.receipt_channel,
        RepositoryReceiptChannelObservation::Empty { .. }
    ) {
        return Err(PersonalWorkerRepositoryResultError::receipt_contract_mismatch());
    }
    Ok(())
}

fn validate_malformed_receipt(
    binding: &PersonalWorkerRepositoryAttemptBinding,
    outer: &PersonalWorkerOuterTerminalEvidence,
    assessed_digest: Option<&Sha256Digest>,
) -> Result<(), PersonalWorkerRepositoryResultError> {
    match &outer.receipt_channel {
        RepositoryReceiptChannelObservation::Malformed {
            bytes_written,
            observed_digest,
            ..
        } if *bytes_written > 0
            && *bytes_written <= binding.receipt_contract.maximum_bytes()
            && observed_digest.as_ref() == assessed_digest =>
        {
            Ok(())
        }
        RepositoryReceiptChannelObservation::Overflow {
            bytes_observed_at_least,
            ..
        } if *bytes_observed_at_least > binding.receipt_contract.maximum_bytes()
            && assessed_digest.is_none() =>
        {
            Ok(())
        }
        _ => Err(PersonalWorkerRepositoryResultError::receipt_contract_mismatch()),
    }
}

fn classify_present(
    outer: &PersonalWorkerOuterTerminalEvidence,
    receipt: &RepositoryAggregateReceipt,
) -> Result<PersonalWorkerRepositoryTerminalClass, PersonalWorkerRepositoryResultError> {
    let result = if outer.process_terminal == RepositoryProcessTerminalClass::RunnerLost {
        PersonalWorkerRepositoryTerminalClass::RunnerLost
    } else {
        match (receipt.terminal_class, outer.process_terminal) {
            (
                RepositoryReceiptTerminalClass::Passed,
                RepositoryProcessTerminalClass::ExitedSuccess,
            ) => PersonalWorkerRepositoryTerminalClass::Passed,
            (
                RepositoryReceiptTerminalClass::VerificationFailed,
                RepositoryProcessTerminalClass::ExitedFailure,
            ) => PersonalWorkerRepositoryTerminalClass::RepositoryVerificationFailed,
            (
                RepositoryReceiptTerminalClass::CompileSetupFailed,
                RepositoryProcessTerminalClass::ExitedFailure,
            ) => PersonalWorkerRepositoryTerminalClass::CompileSetupFailed,
            (RepositoryReceiptTerminalClass::Timeout, RepositoryProcessTerminalClass::Timeout) => {
                PersonalWorkerRepositoryTerminalClass::Timeout
            }
            (
                RepositoryReceiptTerminalClass::Cancelled,
                RepositoryProcessTerminalClass::Cancelled,
            ) => PersonalWorkerRepositoryTerminalClass::Cancelled,
            (
                RepositoryReceiptTerminalClass::ResourceExhausted,
                RepositoryProcessTerminalClass::ResourceExhausted,
            ) => PersonalWorkerRepositoryTerminalClass::ResourceExhausted,
            (
                RepositoryReceiptTerminalClass::DiagnosticInconclusive,
                RepositoryProcessTerminalClass::ExitedFailure
                | RepositoryProcessTerminalClass::DiagnosticInconclusive,
            ) => PersonalWorkerRepositoryTerminalClass::DiagnosticInconclusive,
            _ => return Err(PersonalWorkerRepositoryResultError::terminal_mismatch()),
        }
    };
    if outer.outer_cleanup == RepositoryCleanupDisposition::Incomplete
        || receipt.repository_cleanup == RepositoryCleanupDisposition::Incomplete
    {
        Ok(PersonalWorkerRepositoryTerminalClass::CleanupIncomplete)
    } else {
        Ok(result)
    }
}

fn classify_absent(
    outer: &PersonalWorkerOuterTerminalEvidence,
    absent_class: PersonalWorkerRepositoryTerminalClass,
) -> PersonalWorkerRepositoryTerminalClass {
    if outer.outer_cleanup == RepositoryCleanupDisposition::Incomplete {
        PersonalWorkerRepositoryTerminalClass::CleanupIncomplete
    } else {
        match outer.process_terminal {
            RepositoryProcessTerminalClass::Timeout => {
                PersonalWorkerRepositoryTerminalClass::Timeout
            }
            RepositoryProcessTerminalClass::Cancelled => {
                PersonalWorkerRepositoryTerminalClass::Cancelled
            }
            RepositoryProcessTerminalClass::ResourceExhausted => {
                PersonalWorkerRepositoryTerminalClass::ResourceExhausted
            }
            RepositoryProcessTerminalClass::RunnerLost => {
                PersonalWorkerRepositoryTerminalClass::RunnerLost
            }
            RepositoryProcessTerminalClass::ExitedSuccess
            | RepositoryProcessTerminalClass::ExitedFailure
            | RepositoryProcessTerminalClass::DiagnosticInconclusive => absent_class,
        }
    }
}

const fn valid_limits(limits: ExecutionResourceLimits) -> bool {
    limits.cpu_millis > 0 && limits.memory_bytes > 0 && limits.pids > 0
}

fn valid_public_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.is_ascii()
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_opaque_identity(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|suffix| suffix.len() == 64 && suffix.bytes().all(is_lower_hex))
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRepositoryResultErrorKind {
    InvalidIdentity,
    InvalidBinding,
    AttemptMismatch,
    SourceMismatch,
    VerificationMismatch,
    ResourceMismatch,
    ReservationMismatch,
    CacheMismatch,
    DeadlineMismatch,
    InvalidTimeline,
    ReceiptContractMismatch,
    InvalidWorkDetail,
    ConcurrencyExceeded,
    TerminalMismatch,
    CancellationMismatch,
    StalePredecessor,
    ChangedReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRepositoryResultError {
    kind: PersonalWorkerRepositoryResultErrorKind,
    field: &'static str,
    message: &'static str,
}

impl PersonalWorkerRepositoryResultError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerRepositoryResultErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    const fn new(
        kind: PersonalWorkerRepositoryResultErrorKind,
        field: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            field,
            message,
        }
    }

    const fn invalid_identity(field: &'static str) -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::InvalidIdentity,
            field,
            "repository-result identity is invalid",
        )
    }

    const fn invalid_binding(field: &'static str) -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::InvalidBinding,
            field,
            "repository-result binding is invalid",
        )
    }

    const fn attempt_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::AttemptMismatch,
            "attempt",
            "repository result does not match the exact personal-worker attempt",
        )
    }

    const fn source_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::SourceMismatch,
            "source",
            "repository result does not match the exact commit and tree",
        )
    }

    const fn verification_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::VerificationMismatch,
            "verification",
            "repository result does not match the exact profile, command, and toolchain envelope",
        )
    }

    const fn resource_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::ResourceMismatch,
            "resources",
            "repository result does not match the exact resource and concurrency grant",
        )
    }

    const fn reservation_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::ReservationMismatch,
            "reservation",
            "repository result does not match the exact reservation",
        )
    }

    const fn cache_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::CacheMismatch,
            "cache",
            "repository result does not match the exact durable cache lease",
        )
    }

    const fn deadline_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::DeadlineMismatch,
            "deadline",
            "repository result does not match the exact accepted deadline",
        )
    }

    const fn invalid_timeline() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::InvalidTimeline,
            "timeline",
            "repository result timeline is invalid or outside the outer attempt",
        )
    }

    const fn receipt_contract_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::ReceiptContractMismatch,
            "receipt",
            "repository receipt does not match the pre-opened bounded channel contract",
        )
    }

    const fn invalid_work_detail() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::InvalidWorkDetail,
            "receipt.work_units",
            "repository work-unit detail is duplicate, invalid, or outside the aggregate timeline",
        )
    }

    const fn concurrency_exceeded() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::ConcurrencyExceeded,
            "receipt.maximum_parallelism_observed",
            "repository parallelism or work-unit count exceeds its explicit bound",
        )
    }

    const fn terminal_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::TerminalMismatch,
            "terminal",
            "repository and outer terminal evidence do not agree",
        )
    }

    const fn cancellation_mismatch() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::CancellationMismatch,
            "cancellation",
            "cancellation reason and signal classification do not match the terminal evidence",
        )
    }

    const fn stale_predecessor() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::StalePredecessor,
            "predecessor",
            "new repository completion input is based on a stale store or queue generation",
        )
    }

    const fn changed_replay() -> Self {
        Self::new(
            PersonalWorkerRepositoryResultErrorKind::ChangedReplay,
            "replay",
            "repository completion replay changed retained terminal semantics",
        )
    }
}

impl fmt::Display for PersonalWorkerRepositoryResultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRepositoryResultError {}
