//! Pure product-neutral capacity candidacy and fail-closed admission for generic compute requests.
//!
//! A candidate is descriptive pre-admission evidence, not resource ownership. Only
//! `admit_compute_capacity_candidate` constructs a product-neutral `CapacityClaim`, and only after
//! the existing capacity arithmetic accepts it. This module grants zero persistence, placement,
//! backend, process/VM execution, lifecycle, release, retry, supersession, or result authority.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::ComputeExecutionRequest;
use crate::compute_workload::ComputeWorkloadIdentity;
use crate::compute_workload::supersession::WorkAttemptGeneration;
use crate::execution_capacity::{
    CapacityAdmissionDecision, CapacityAdmissionRefusalReason, CapacityAmounts, CapacityClaim,
    CapacityClaimId, CapacityDomain, CapacityDomainGeneration, CapacityDomainId,
};

pub const COMPUTE_CAPACITY_ADMISSION_SCHEMA_VERSION: u8 = 1;
pub const COMPUTE_CAPACITY_CLAIM_POLICY_GENERATION: u8 = 1;

const CLAIM_IDENTITY_DOMAIN: &[u8] = b"glaeda.compute-capacity-claim.v1\0";
const CAPACITY_CLAIM_ID_PREFIX: &str = "capacity-claim-v1-";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Serialize)]
struct ClaimIdentityDocument<'a> {
    claim_policy_generation: u8,
    compute_request_schema_version: u8,
    request_id: &'a str,
    workload: &'a ComputeWorkloadIdentity,
    attempt_generation: u64,
    capacity_domain_id: &'a str,
    capacity_domain_generation: u64,
    requested_resources: &'a CapacityAmounts,
}

/// Non-authoritative binding of one exact compute request to one attempt and capacity domain.
///
/// The claim ID and domain binding are derived internally. There is deliberately no raw claim-ID
/// constructor and no conversion from this type into `CapacityClaim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComputeCapacityClaimCandidate {
    schema_version: u8,
    claim_policy_generation: u8,
    claim_id: CapacityClaimId,
    request: ComputeExecutionRequest,
    attempt_generation: WorkAttemptGeneration,
    capacity_domain_id: CapacityDomainId,
    capacity_domain_generation: CapacityDomainGeneration,
}

impl ComputeCapacityClaimCandidate {
    /// Bind an already validated generic request to one exact attempt and domain.
    ///
    /// # Errors
    ///
    /// Returns a bounded error if the typed identity tuple cannot be canonically encoded.
    pub fn new(
        request: ComputeExecutionRequest,
        attempt_generation: WorkAttemptGeneration,
        domain: &CapacityDomain,
    ) -> Result<Self, ComputeCapacityAdmissionError> {
        let claim_id = derive_claim_id(&request, attempt_generation, domain)?;
        Ok(Self {
            schema_version: COMPUTE_CAPACITY_ADMISSION_SCHEMA_VERSION,
            claim_policy_generation: COMPUTE_CAPACITY_CLAIM_POLICY_GENERATION,
            claim_id,
            request,
            attempt_generation,
            capacity_domain_id: domain.id().clone(),
            capacity_domain_generation: domain.generation(),
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn claim_policy_generation(&self) -> u8 {
        self.claim_policy_generation
    }

    #[must_use]
    pub const fn claim_id(&self) -> &CapacityClaimId {
        &self.claim_id
    }

    #[must_use]
    pub const fn request(&self) -> &ComputeExecutionRequest {
        &self.request
    }

    #[must_use]
    pub const fn attempt_generation(&self) -> WorkAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn capacity_domain_id(&self) -> &CapacityDomainId {
        &self.capacity_domain_id
    }

    #[must_use]
    pub const fn capacity_domain_generation(&self) -> CapacityDomainGeneration {
        self.capacity_domain_generation
    }
}

/// Capacity-admission result for one compute candidate.
///
/// A refused decision carries no ownership-typed claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ComputeCapacityAdmissionDecision {
    Accepted {
        schema_version: u8,
        claim: CapacityClaim,
    },
    Refused {
        schema_version: u8,
        reason: CapacityAdmissionRefusalReason,
    },
}

impl ComputeCapacityAdmissionDecision {
    #[must_use]
    pub fn accepted_claim(&self) -> Option<&CapacityClaim> {
        match self {
            Self::Accepted { claim, .. } => Some(claim),
            Self::Refused { .. } => None,
        }
    }

    #[must_use]
    pub const fn refusal_reason(&self) -> Option<CapacityAdmissionRefusalReason> {
        match self {
            Self::Accepted { .. } => None,
            Self::Refused { reason, .. } => Some(*reason),
        }
    }
}

/// Submit one non-authoritative compute candidate to exact capacity admission.
///
/// Refusal precedence remains:
/// foreign domain -> stale generation -> dimension-set mismatch -> duplicate identity ->
/// arithmetic overflow -> insufficient resources.
///
/// The first three gates run before ownership-typed claim construction because `CapacityClaim`
/// intentionally rejects a mismatched dimension set at construction. All remaining arithmetic and
/// refusal semantics delegate to `execution_capacity::admit_capacity_claim`.
#[must_use]
pub fn admit_compute_capacity_candidate(
    domain: &CapacityDomain,
    existing: &[CapacityClaim],
    candidate: &ComputeCapacityClaimCandidate,
) -> ComputeCapacityAdmissionDecision {
    if candidate.capacity_domain_id() != domain.id()
        || existing
            .iter()
            .any(|claim| claim.domain_id() != domain.id())
    {
        return refused(CapacityAdmissionRefusalReason::ForeignDomain);
    }
    if candidate.capacity_domain_generation() != domain.generation()
        || existing
            .iter()
            .any(|claim| claim.domain_generation() != domain.generation())
    {
        return refused(CapacityAdmissionRefusalReason::StaleGeneration);
    }
    if candidate
        .request()
        .requested_resources()
        .tracked_dimensions()
        != domain.budget().tracked_dimensions()
        || existing.iter().any(|claim| {
            claim.resources().tracked_dimensions() != domain.budget().tracked_dimensions()
        })
    {
        return refused(CapacityAdmissionRefusalReason::DimensionSetMismatch);
    }

    let claim = match CapacityClaim::for_domain(
        candidate.claim_id().clone(),
        domain,
        candidate.request().requested_resources().clone(),
    ) {
        Ok(claim) => claim,
        Err(_) => return refused(CapacityAdmissionRefusalReason::DimensionSetMismatch),
    };

    match crate::execution_capacity::admit_capacity_claim(domain, existing, &claim) {
        CapacityAdmissionDecision::Accepted { .. } => ComputeCapacityAdmissionDecision::Accepted {
            schema_version: COMPUTE_CAPACITY_ADMISSION_SCHEMA_VERSION,
            claim,
        },
        CapacityAdmissionDecision::Refused { reason, .. } => refused(reason),
    }
}

fn derive_claim_id(
    request: &ComputeExecutionRequest,
    attempt_generation: WorkAttemptGeneration,
    domain: &CapacityDomain,
) -> Result<CapacityClaimId, ComputeCapacityAdmissionError> {
    let document = ClaimIdentityDocument {
        claim_policy_generation: COMPUTE_CAPACITY_CLAIM_POLICY_GENERATION,
        compute_request_schema_version: request.schema_version(),
        request_id: request.request_id().as_str(),
        workload: request.workload(),
        attempt_generation: attempt_generation.get(),
        capacity_domain_id: domain.id().as_str(),
        capacity_domain_generation: domain.generation().get(),
        requested_resources: request.requested_resources(),
    };
    let encoded = serde_json::to_vec(&document).map_err(|_| identity_encoding_error())?;
    let encoded_len = u64::try_from(encoded.len()).map_err(|_| identity_encoding_error())?;

    let mut hasher = Sha256::new();
    hasher.update(CLAIM_IDENTITY_DOMAIN);
    hasher.update(encoded_len.to_be_bytes());
    hasher.update(encoded);
    let digest = hasher.finalize();

    let mut value = String::with_capacity(CAPACITY_CLAIM_ID_PREFIX.len() + 64);
    value.push_str(CAPACITY_CLAIM_ID_PREFIX);
    for byte in digest {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    CapacityClaimId::parse(&value).map_err(|_| identity_encoding_error())
}

const fn refused(reason: CapacityAdmissionRefusalReason) -> ComputeCapacityAdmissionDecision {
    ComputeCapacityAdmissionDecision::Refused {
        schema_version: COMPUTE_CAPACITY_ADMISSION_SCHEMA_VERSION,
        reason,
    }
}

const fn identity_encoding_error() -> ComputeCapacityAdmissionError {
    ComputeCapacityAdmissionError {
        field: "claim_id",
        code: "compute_capacity_claim_identity_encoding_failed",
        message: "compute capacity claim identity could not be encoded canonically",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ComputeCapacityAdmissionError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl ComputeCapacityAdmissionError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ComputeCapacityAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ComputeCapacityAdmissionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::compute_workload::{
        ComputeCapabilityId, ComputeCapabilitySet, ComputeInputIdentity,
        ComputeOutputContractIdentity, ComputeSemanticGeneration, ComputeTrustClass,
        ComputeWorkloadFamilyId,
    };
    use crate::execution_admission::ExecutionRequestId;
    use crate::execution_capacity::CapacityDimension;
    use crate::frontier_inference_workload::frontier_inference_synthetic_sensitivity_fixtures;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn digest(fill: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    }

    fn workload(family: &str, fill: char) -> ComputeWorkloadIdentity {
        ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse(family).unwrap(),
            ComputeSemanticGeneration::new(1).unwrap(),
            ComputeInputIdentity::new(digest(fill)),
            ComputeTrustClass::Trusted,
            ComputeCapabilitySet::new(vec![ComputeCapabilityId::parse("linux.arm64").unwrap()])
                .unwrap(),
            ComputeOutputContractIdentity::new(digest(if fill == 'f' { 'e' } else { 'f' })),
        )
    }

    fn request(
        id: &str,
        family: &str,
        fill: char,
        resources: CapacityAmounts,
    ) -> ComputeExecutionRequest {
        ComputeExecutionRequest::new(
            ExecutionRequestId::parse(id).unwrap(),
            workload(family, fill),
            resources,
        )
    }

    fn resources(cpu: u64, memory: u64, disk: u64, pids: u64) -> CapacityAmounts {
        CapacityAmounts::new(&[
            (CapacityDimension::CpuMillis, cpu),
            (CapacityDimension::MemoryBytes, memory),
            (CapacityDimension::DiskBytes, disk),
            (CapacityDimension::Pids, pids),
        ])
        .unwrap()
    }

    fn domain(id_fill: char, generation: u64, budget: CapacityAmounts) -> CapacityDomain {
        CapacityDomain::root(
            CapacityDomainId::parse(&format!(
                "capacity-domain-v1-{}",
                id_fill.to_string().repeat(64)
            ))
            .unwrap(),
            CapacityDomainGeneration::new(generation).unwrap(),
            budget,
        )
    }

    fn main_domain(generation: u64) -> CapacityDomain {
        domain(
            'a',
            generation,
            resources(16_000, 32 * GIB, 128 * GIB, 1_024),
        )
    }

    fn attempt(value: u64) -> WorkAttemptGeneration {
        WorkAttemptGeneration::new(value).unwrap()
    }

    fn accepted(decision: ComputeCapacityAdmissionDecision) -> CapacityClaim {
        match decision {
            ComputeCapacityAdmissionDecision::Accepted { claim, .. } => claim,
            ComputeCapacityAdmissionDecision::Refused { reason, .. } => {
                panic!("expected acceptance, got {reason:?}")
            }
        }
    }

    #[test]
    fn claim_identity_binds_request_workload_attempt_domain_and_resources() {
        let first_domain = main_domain(1);
        let next_domain = main_domain(2);
        let base_request = request(
            "request-capacity-binding-1",
            "repository_verification.v1",
            'a',
            resources(4_000, 4 * GIB, 8 * GIB, 128),
        );
        let replay =
            ComputeCapacityClaimCandidate::new(base_request.clone(), attempt(1), &first_domain)
                .unwrap();
        let same =
            ComputeCapacityClaimCandidate::new(base_request.clone(), attempt(1), &first_domain)
                .unwrap();
        let other_attempt =
            ComputeCapacityClaimCandidate::new(base_request.clone(), attempt(2), &first_domain)
                .unwrap();
        let other_domain =
            ComputeCapacityClaimCandidate::new(base_request.clone(), attempt(1), &next_domain)
                .unwrap();
        let other_workload = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-binding-1",
                "frontier_inference.v1",
                'b',
                resources(4_000, 4 * GIB, 8 * GIB, 128),
            ),
            attempt(1),
            &first_domain,
        )
        .unwrap();
        let other_resources = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-binding-1",
                "repository_verification.v1",
                'a',
                resources(4_001, 4 * GIB, 8 * GIB, 128),
            ),
            attempt(1),
            &first_domain,
        )
        .unwrap();

        assert_eq!(replay.claim_id(), same.claim_id());
        assert_ne!(replay.claim_id(), other_attempt.claim_id());
        assert_ne!(replay.claim_id(), other_domain.claim_id());
        assert_ne!(replay.claim_id(), other_workload.claim_id());
        assert_ne!(replay.claim_id(), other_resources.claim_id());
    }

    #[test]
    fn tracked_zero_and_untracked_dimensions_remain_distinct() {
        let domain = domain(
            'b',
            1,
            CapacityAmounts::new(&[
                (CapacityDimension::CpuMillis, 4_000),
                (CapacityDimension::MemoryBytes, 8 * GIB),
                (CapacityDimension::DiskBytes, 16 * GIB),
            ])
            .unwrap(),
        );
        let untracked = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-zero-1",
                "dataset_transform.v1",
                'c',
                CapacityAmounts::new(&[
                    (CapacityDimension::CpuMillis, 1_000),
                    (CapacityDimension::MemoryBytes, GIB),
                ])
                .unwrap(),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();
        let tracked_zero = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-zero-1",
                "dataset_transform.v1",
                'c',
                CapacityAmounts::new(&[
                    (CapacityDimension::CpuMillis, 1_000),
                    (CapacityDimension::MemoryBytes, GIB),
                    (CapacityDimension::DiskBytes, 0),
                ])
                .unwrap(),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();

        assert_ne!(untracked.claim_id(), tracked_zero.claim_id());
        assert_eq!(
            admit_compute_capacity_candidate(&domain, &[], &untracked).refusal_reason(),
            Some(CapacityAdmissionRefusalReason::DimensionSetMismatch)
        );
        assert!(
            admit_compute_capacity_candidate(&domain, &[], &tracked_zero)
                .accepted_claim()
                .is_some()
        );
    }

    #[test]
    fn accepted_claim_exactly_matches_the_request_and_refusal_exposes_no_claim() {
        let domain = main_domain(1);
        let requested = resources(4_000, 4 * GIB, 8 * GIB, 128);
        let candidate = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-accepted-1",
                "repository_verification.v1",
                'd',
                requested.clone(),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();

        let claim = accepted(admit_compute_capacity_candidate(&domain, &[], &candidate));
        assert_eq!(claim.id(), candidate.claim_id());
        assert_eq!(claim.resources(), &requested);

        let too_large = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-refused-1",
                "repository_verification.v1",
                'e',
                resources(17_000, 4 * GIB, 8 * GIB, 128),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();
        let refused = admit_compute_capacity_candidate(&domain, &[], &too_large);
        assert_eq!(
            refused.refusal_reason(),
            Some(CapacityAdmissionRefusalReason::InsufficientResources)
        );
        assert!(refused.accepted_claim().is_none());
    }

    #[test]
    fn existing_refusal_order_and_duplicate_identity_are_preserved() {
        let domain = main_domain(1);
        let dimension_mismatch = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-precedence-1",
                "repository_verification.v1",
                '1',
                CapacityAmounts::new(&[
                    (CapacityDimension::CpuMillis, 4_000),
                    (CapacityDimension::MemoryBytes, 4 * GIB),
                ])
                .unwrap(),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();
        let foreign_domain = self::domain('c', 1, resources(16_000, 32 * GIB, 128 * GIB, 1_024));
        let foreign = CapacityClaim::for_domain(
            CapacityClaimId::parse(&format!("capacity-claim-v1-{}", "d".repeat(64))).unwrap(),
            &foreign_domain,
            resources(1, 1, 1, 1),
        )
        .unwrap();
        assert_eq!(
            admit_compute_capacity_candidate(&domain, &[foreign], &dimension_mismatch)
                .refusal_reason(),
            Some(CapacityAdmissionRefusalReason::ForeignDomain)
        );

        let stale_domain = main_domain(2);
        let stale = CapacityClaim::for_domain(
            CapacityClaimId::parse(&format!("capacity-claim-v1-{}", "e".repeat(64))).unwrap(),
            &stale_domain,
            resources(1, 1, 1, 1),
        )
        .unwrap();
        assert_eq!(
            admit_compute_capacity_candidate(&domain, &[stale], &dimension_mismatch)
                .refusal_reason(),
            Some(CapacityAdmissionRefusalReason::StaleGeneration)
        );
        assert_eq!(
            admit_compute_capacity_candidate(&domain, &[], &dimension_mismatch).refusal_reason(),
            Some(CapacityAdmissionRefusalReason::DimensionSetMismatch)
        );

        let exact = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-duplicate-1",
                "repository_verification.v1",
                '2',
                resources(4_000, 4 * GIB, 8 * GIB, 128),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();
        let existing = accepted(admit_compute_capacity_candidate(&domain, &[], &exact));
        assert_eq!(
            admit_compute_capacity_candidate(&domain, &[existing], &exact).refusal_reason(),
            Some(CapacityAdmissionRefusalReason::DuplicateClaimIdentity)
        );
    }

    #[test]
    fn repository_and_frontier_requests_share_the_same_boundary() {
        let domain = main_domain(1);
        let repository = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-repository-1",
                "repository_verification.v1",
                '3',
                resources(4_000, 4 * GIB, 8 * GIB, 128),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();

        let fixture = frontier_inference_synthetic_sensitivity_fixtures()
            .into_iter()
            .next()
            .unwrap();
        let dimensions = [
            CapacityDimension::CpuMillis,
            CapacityDimension::MemoryBytes,
            CapacityDimension::DiskBytes,
            CapacityDimension::Pids,
        ];
        let entries = dimensions.map(|dimension| {
            let maximum = fixture
                .receipt()
                .host_tool_phases()
                .iter()
                .map(|phase| phase.host_capacity_demand().amount(dimension).unwrap())
                .max()
                .unwrap();
            (dimension, maximum)
        });
        let frontier = ComputeCapacityClaimCandidate::new(
            ComputeExecutionRequest::new(
                ExecutionRequestId::parse("request-capacity-frontier-1").unwrap(),
                fixture.receipt().workload().clone(),
                CapacityAmounts::new(&entries).unwrap(),
            ),
            attempt(1),
            &domain,
        )
        .unwrap();

        let repository_claim =
            accepted(admit_compute_capacity_candidate(&domain, &[], &repository));
        let frontier_claim = accepted(admit_compute_capacity_candidate(&domain, &[], &frontier));

        assert_eq!(
            repository_claim.resources(),
            repository.request().requested_resources()
        );
        assert_eq!(
            frontier_claim.resources(),
            frontier.request().requested_resources()
        );
        assert_ne!(repository_claim.id(), frontier_claim.id());
    }

    #[test]
    fn candidate_json_has_no_execution_or_private_surface() {
        let domain = main_domain(1);
        let candidate = ComputeCapacityClaimCandidate::new(
            request(
                "request-capacity-json-1",
                "frontier_inference.v1",
                '4',
                resources(4_000, 4 * GIB, 8 * GIB, 128),
            ),
            attempt(7),
            &domain,
        )
        .unwrap();
        let json = serde_json::to_string(&candidate).unwrap();

        assert!(json.contains("capacity-claim-v1-"));
        assert!(json.contains("\"attempt_generation\":7"));
        for forbidden in [
            "/tmp/",
            "command",
            "environment",
            "credential",
            "token",
            "backend",
            "process",
        ] {
            assert!(!json.contains(forbidden));
        }
    }
}
