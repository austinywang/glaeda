//! Pure composition of one captured Quarry receipt with one outer Glaeda attempt.
//!
//! The adapter classifies already-captured bytes and delegates final identity, terminal, cleanup,
//! and resource correlation to `personal_worker_repository_result`. It cannot open a channel,
//! execute work, persist a result, publish a completion, signal a process, or release a lease.

use std::fmt;

use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::execution_admission::{
    EpochMillis, ExecutionRequestId, ReservationGeneration, ReservationId,
};
use crate::personal_worker_repository_result::{
    PersonalWorkerJobAttemptGeneration, PersonalWorkerJobAttemptId,
    PersonalWorkerOuterTerminalEvidence, PersonalWorkerRepositoryAttemptBinding,
    PersonalWorkerRepositoryCompletionInput, PersonalWorkerRepositoryResultErrorKind,
    RepositoryAggregateReceipt, RepositoryCleanupDisposition, RepositoryProcessTerminalClass,
    RepositoryReceiptAssessment, RepositoryReceiptChannelId, RepositoryReceiptChannelObservation,
    RepositoryReceiptTerminalClass, RepositoryResourceExhaustionEvidence, RepositoryStopEvidence,
    RepositoryVerifierProducerId, RepositoryWorkDetail,
    correlate_personal_worker_repository_result,
};
use crate::quarry_parallel_verification_receipt::{
    MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES, QUARRY_PARALLEL_VERIFICATION_SCHEMA_VERSION,
    QuarryParallelVerificationCleanupStatus, QuarryParallelVerificationReceipt,
    QuarryParallelVerificationShardState, QuarryParallelVerificationTerminationReason,
    decode_quarry_parallel_verification_receipt,
};

pub const QUARRY_PARALLEL_VERIFICATION_PRODUCER_ID: &str = "quarry-parallel-verification-v2";
const QUARRY_TOOLCHAIN_PREFIX: &str = "verification-toolchain-v1:sha256:";
const WORK_DETAIL_DOMAIN: &[u8] = b"glaeda-quarry-parallel-work-detail-v1\0";

/// Exact bounded channel material observed by the outer execution boundary.
///
/// Byte contents remain borrowed private input and are never retained in the returned completion.
pub enum QuarryParallelVerificationCapture<'a> {
    Missing,
    Bytes(&'a [u8]),
    Overflow { bytes_observed_at_least: u64 },
}

/// Independently observed outer-attempt facts required to correlate one captured receipt.
pub struct QuarryParallelVerificationOuterObservation<'a> {
    pub request_id: ExecutionRequestId,
    pub attempt_id: PersonalWorkerJobAttemptId,
    pub attempt_generation: PersonalWorkerJobAttemptGeneration,
    pub reservation_id: ReservationId,
    pub reservation_generation: ReservationGeneration,
    pub started_at: EpochMillis,
    pub terminal_at: EpochMillis,
    pub process_terminal: RepositoryProcessTerminalClass,
    pub stop: Option<RepositoryStopEvidence>,
    pub resource_exhaustion: Option<RepositoryResourceExhaustionEvidence>,
    pub outer_cleanup: RepositoryCleanupDisposition,
    pub channel_id: RepositoryReceiptChannelId,
    pub capture: QuarryParallelVerificationCapture<'a>,
    pub aggregate_started_at: EpochMillis,
    pub aggregate_terminal_at: EpochMillis,
    pub maximum_parallelism_observed: u16,
}

/// Classify and correlate one whole Quarry verification attempt.
///
/// Empty, malformed, and overflowing captures remain typed terminal completion inputs. A valid
/// receipt must independently match the bound commit, tree, toolchain, producer contract, and
/// repository concurrency before the generic correlation kernel sees it.
///
/// # Errors
///
/// Returns a fixed-class error for invalid overflow/timeline evidence, Quarry-to-binding drift, or
/// failure of the generic outer-attempt correlation contract.
pub fn correlate_quarry_parallel_verification(
    binding: PersonalWorkerRepositoryAttemptBinding,
    observation: QuarryParallelVerificationOuterObservation<'_>,
) -> Result<PersonalWorkerRepositoryCompletionInput, QuarryParallelVerificationAdapterError> {
    validate_binding_contract(&binding)?;
    let channel_id = observation.channel_id.clone();
    let (receipt_channel, repository_receipt) = match observation.capture {
        QuarryParallelVerificationCapture::Missing => (
            RepositoryReceiptChannelObservation::Empty { channel_id },
            RepositoryReceiptAssessment::Missing,
        ),
        QuarryParallelVerificationCapture::Overflow {
            bytes_observed_at_least,
        } => {
            if bytes_observed_at_least <= binding.receipt_contract.maximum_bytes() {
                return Err(QuarryParallelVerificationAdapterError::invalid_capture());
            }
            (
                RepositoryReceiptChannelObservation::Overflow {
                    channel_id,
                    bytes_observed_at_least,
                },
                RepositoryReceiptAssessment::Malformed {
                    observed_digest: None,
                },
            )
        }
        QuarryParallelVerificationCapture::Bytes([]) => (
            RepositoryReceiptChannelObservation::Empty { channel_id },
            RepositoryReceiptAssessment::Missing,
        ),
        QuarryParallelVerificationCapture::Bytes(bytes)
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                > binding.receipt_contract.maximum_bytes() =>
        {
            let bytes_observed_at_least = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            (
                RepositoryReceiptChannelObservation::Overflow {
                    channel_id,
                    bytes_observed_at_least,
                },
                RepositoryReceiptAssessment::Malformed {
                    observed_digest: None,
                },
            )
        }
        QuarryParallelVerificationCapture::Bytes(bytes) => {
            let digest = digest(bytes)?;
            let bytes_written = u64::try_from(bytes.len())
                .map_err(|_| QuarryParallelVerificationAdapterError::invalid_capture())?;
            match decode_quarry_parallel_verification_receipt(bytes) {
                Ok(receipt) => {
                    let aggregate =
                        map_present_receipt(&binding, &observation, &receipt, digest.clone())?;
                    (
                        RepositoryReceiptChannelObservation::Captured {
                            channel_id,
                            bytes_written,
                            digest,
                        },
                        RepositoryReceiptAssessment::present(aggregate),
                    )
                }
                Err(_) => (
                    RepositoryReceiptChannelObservation::Malformed {
                        channel_id,
                        bytes_written,
                        observed_digest: Some(digest.clone()),
                    },
                    RepositoryReceiptAssessment::Malformed {
                        observed_digest: Some(digest),
                    },
                ),
            }
        }
    };

    let outer = PersonalWorkerOuterTerminalEvidence {
        request_id: observation.request_id,
        attempt_id: observation.attempt_id,
        attempt_generation: observation.attempt_generation,
        reservation_id: observation.reservation_id,
        reservation_generation: observation.reservation_generation,
        started_at: observation.started_at,
        terminal_at: observation.terminal_at,
        process_terminal: observation.process_terminal,
        stop: observation.stop,
        resource_exhaustion: observation.resource_exhaustion,
        outer_cleanup: observation.outer_cleanup,
        receipt_channel,
    };
    correlate_personal_worker_repository_result(binding, outer, repository_receipt)
        .map_err(|error| QuarryParallelVerificationAdapterError::correlation(error.kind()))
}

fn map_present_receipt(
    binding: &PersonalWorkerRepositoryAttemptBinding,
    observation: &QuarryParallelVerificationOuterObservation<'_>,
    receipt: &QuarryParallelVerificationReceipt,
    receipt_digest: Sha256Digest,
) -> Result<RepositoryAggregateReceipt, QuarryParallelVerificationAdapterError> {
    let key = receipt.plan().key();
    let source = key.source();
    let toolchain_hex = key
        .toolchain_id()
        .strip_prefix(QUARRY_TOOLCHAIN_PREFIX)
        .ok_or_else(QuarryParallelVerificationAdapterError::binding_mismatch)?;
    if source.commit() != binding.source.commit.as_str()
        || source.tree() != binding.source.tree.as_str()
        || binding
            .toolchain_envelope_digest
            .as_str()
            .strip_prefix("sha256:")
            != Some(toolchain_hex)
        || key.workers() != binding.repository_concurrency.get()
    {
        return Err(QuarryParallelVerificationAdapterError::binding_mismatch());
    }
    let aggregate_wall = observation
        .aggregate_terminal_at
        .get()
        .checked_sub(observation.aggregate_started_at.get())
        .ok_or_else(QuarryParallelVerificationAdapterError::invalid_timeline)?;
    let work_unit_count = u32::try_from(
        receipt
            .outcomes()
            .iter()
            .filter(|outcome| outcome.state() != QuarryParallelVerificationShardState::NotStarted)
            .count(),
    )
    .map_err(|_| QuarryParallelVerificationAdapterError::binding_mismatch())?;
    let maximum_parallelism = u32::from(observation.maximum_parallelism_observed);
    if aggregate_wall < receipt.result().aggregate_wall_millis()
        || maximum_parallelism > u32::from(key.workers())
        || maximum_parallelism > work_unit_count
        || (work_unit_count == 0 && maximum_parallelism != 0)
        || (work_unit_count > 0 && maximum_parallelism == 0)
    {
        return Err(QuarryParallelVerificationAdapterError::invalid_timeline());
    }

    let terminal_class = map_terminal(receipt, observation.process_terminal)?;
    let repository_cleanup = match receipt.cleanup().status() {
        QuarryParallelVerificationCleanupStatus::Passed => RepositoryCleanupDisposition::Complete,
        QuarryParallelVerificationCleanupStatus::Failed => RepositoryCleanupDisposition::Incomplete,
    };
    let producer_id = RepositoryVerifierProducerId::parse(QUARRY_PARALLEL_VERIFICATION_PRODUCER_ID)
        .map_err(|_| QuarryParallelVerificationAdapterError::binding_mismatch())?;
    Ok(RepositoryAggregateReceipt {
        request_id: binding.request_id.clone(),
        attempt_id: binding.attempt_id.clone(),
        attempt_generation: binding.attempt_generation,
        predecessor_store_revision: binding.predecessor_store_revision,
        predecessor_queue_generation: binding.predecessor_queue_generation,
        source: binding.source.clone(),
        verification_profile_id: binding.verification_profile_id.clone(),
        command: binding.command.clone(),
        toolchain_envelope_digest: binding.toolchain_envelope_digest.clone(),
        requested_limits: binding.requested_limits,
        applied_limits: binding.applied_limits,
        repository_concurrency: binding.repository_concurrency,
        reservation_id: binding.reservation_id.clone(),
        reservation_generation: binding.reservation_generation,
        cache_namespace: binding.cache_namespace.clone(),
        cache_access: binding.cache_access,
        cache_lease_acquired_at: binding.cache_lease_acquired_at,
        not_after: binding.not_after,
        producer_id,
        producer_schema_version: u32::from(QUARRY_PARALLEL_VERIFICATION_SCHEMA_VERSION),
        receipt_digest,
        aggregate_started_at: observation.aggregate_started_at,
        aggregate_terminal_at: observation.aggregate_terminal_at,
        terminal_class,
        maximum_parallelism_observed: observation.maximum_parallelism_observed,
        work_detail: RepositoryWorkDetail::DetailDigest {
            work_unit_count,
            detail_digest: domain_digest(WORK_DETAIL_DOMAIN, receipt.canonical_bytes())?,
        },
        stop: observation.stop,
        resource_exhaustion: observation.resource_exhaustion.clone(),
        repository_cleanup,
    })
}

fn validate_binding_contract(
    binding: &PersonalWorkerRepositoryAttemptBinding,
) -> Result<(), QuarryParallelVerificationAdapterError> {
    let expected_maximum_bytes = u64::try_from(MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES)
        .expect("Quarry byte bound fits u64");
    if binding.receipt_contract.producer_id().as_str() != QUARRY_PARALLEL_VERIFICATION_PRODUCER_ID
        || binding.receipt_contract.producer_schema_version()
            != u32::from(QUARRY_PARALLEL_VERIFICATION_SCHEMA_VERSION)
        || binding.receipt_contract.maximum_bytes() != expected_maximum_bytes
    {
        return Err(QuarryParallelVerificationAdapterError::binding_mismatch());
    }
    Ok(())
}

fn map_terminal(
    receipt: &QuarryParallelVerificationReceipt,
    outer: RepositoryProcessTerminalClass,
) -> Result<RepositoryReceiptTerminalClass, QuarryParallelVerificationAdapterError> {
    use QuarryParallelVerificationTerminationReason as Inner;
    let mapped = match receipt.result().termination_reason() {
        Inner::Completed => RepositoryReceiptTerminalClass::Passed,
        Inner::ShardFailure => RepositoryReceiptTerminalClass::VerificationFailed,
        Inner::Cancelled | Inner::Interrupted => RepositoryReceiptTerminalClass::Cancelled,
        Inner::CleanupFailure => RepositoryReceiptTerminalClass::DiagnosticInconclusive,
        Inner::InternalFailure => match outer {
            RepositoryProcessTerminalClass::Timeout => RepositoryReceiptTerminalClass::Timeout,
            RepositoryProcessTerminalClass::Cancelled => RepositoryReceiptTerminalClass::Cancelled,
            RepositoryProcessTerminalClass::ResourceExhausted => {
                RepositoryReceiptTerminalClass::ResourceExhausted
            }
            RepositoryProcessTerminalClass::ExitedFailure
            | RepositoryProcessTerminalClass::DiagnosticInconclusive
            | RepositoryProcessTerminalClass::RunnerLost => {
                RepositoryReceiptTerminalClass::DiagnosticInconclusive
            }
            RepositoryProcessTerminalClass::ExitedSuccess => {
                return Err(QuarryParallelVerificationAdapterError::binding_mismatch());
            }
        },
    };
    Ok(mapped)
}

fn digest(bytes: &[u8]) -> Result<Sha256Digest, QuarryParallelVerificationAdapterError> {
    domain_digest(b"", bytes)
}

fn domain_digest(
    domain: &[u8],
    bytes: &[u8],
) -> Result<Sha256Digest, QuarryParallelVerificationAdapterError> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| QuarryParallelVerificationAdapterError::binding_mismatch())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarryParallelVerificationAdapterErrorKind {
    InvalidCapture,
    BindingMismatch,
    InvalidTimeline,
    Correlation(PersonalWorkerRepositoryResultErrorKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarryParallelVerificationAdapterError {
    kind: QuarryParallelVerificationAdapterErrorKind,
    message: &'static str,
}

impl QuarryParallelVerificationAdapterError {
    #[must_use]
    pub const fn kind(self) -> QuarryParallelVerificationAdapterErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(self) -> &'static str {
        self.message
    }

    const fn invalid_capture() -> Self {
        Self {
            kind: QuarryParallelVerificationAdapterErrorKind::InvalidCapture,
            message: "Quarry receipt capture evidence is invalid",
        }
    }

    const fn binding_mismatch() -> Self {
        Self {
            kind: QuarryParallelVerificationAdapterErrorKind::BindingMismatch,
            message: "Quarry receipt does not match the bound Glaeda attempt",
        }
    }

    const fn invalid_timeline() -> Self {
        Self {
            kind: QuarryParallelVerificationAdapterErrorKind::InvalidTimeline,
            message: "Quarry receipt timeline or parallelism evidence is invalid",
        }
    }

    const fn correlation(kind: PersonalWorkerRepositoryResultErrorKind) -> Self {
        Self {
            kind: QuarryParallelVerificationAdapterErrorKind::Correlation(kind),
            message: "Quarry receipt failed outer Glaeda attempt correlation",
        }
    }
}

impl fmt::Display for QuarryParallelVerificationAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for QuarryParallelVerificationAdapterError {}
