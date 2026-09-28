//! Pure workload-neutral pre-admission request for one declared compute workload.
//!
//! This module binds an already validated request identity and semantic workload identity to one
//! already validated generic resource envelope. It deliberately performs no resource aggregation
//! or family interpretation: workload-family adapters own derivation of `requested_resources`.
//!
//! A request grants zero capacity ownership, admission, placement, backend selection, execution,
//! persistence, retry, supersession, lifecycle, settlement, or result-publication authority.
//! `CapacityClaim` remains the separate post-admission resource-ownership boundary.

use serde::Serialize;

use crate::compute_workload::ComputeWorkloadIdentity;
use crate::execution_admission::ExecutionRequestId;
use crate::execution_capacity::CapacityAmounts;

pub const COMPUTE_EXECUTION_REQUEST_SCHEMA_VERSION: u8 = 1;

/// Exact declarative resource intent for one semantic compute workload request.
///
/// Equality covers the complete request tuple: request identity, semantic workload identity, and
/// requested resource dimension set/amounts. That makes future idempotency drift inspectable but
/// grants no replay or conflict-resolution authority by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComputeExecutionRequest {
    schema_version: u8,
    request_id: ExecutionRequestId,
    workload: ComputeWorkloadIdentity,
    requested_resources: CapacityAmounts,
}

impl ComputeExecutionRequest {
    /// Bind already validated semantic work to already validated generic resource intent.
    ///
    /// `ExecutionRequestId`, `ComputeWorkloadIdentity`, and `CapacityAmounts` own their respective
    /// validation contracts. In particular, `CapacityAmounts` preserves untracked dimensions as
    /// absent and explicitly tracked zero values as zero; this carrier never normalizes either.
    #[must_use]
    pub const fn new(
        request_id: ExecutionRequestId,
        workload: ComputeWorkloadIdentity,
        requested_resources: CapacityAmounts,
    ) -> Self {
        Self {
            schema_version: COMPUTE_EXECUTION_REQUEST_SCHEMA_VERSION,
            request_id,
            workload,
            requested_resources,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn request_id(&self) -> &ExecutionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn workload(&self) -> &ComputeWorkloadIdentity {
        &self.workload
    }

    #[must_use]
    pub const fn requested_resources(&self) -> &CapacityAmounts {
        &self.requested_resources
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::compute_workload::{
        ComputeCapabilityId, ComputeCapabilitySet, ComputeInputIdentity,
        ComputeOutputContractIdentity, ComputeSemanticGeneration, ComputeTrustClass,
        ComputeWorkloadFamilyId,
    };
    use crate::execution_admission::{ExecutionAdmissionIdentity, RunnerProfileId};
    use crate::execution_capacity::CapacityDimension;
    use crate::frontier_inference_workload::{
        FrontierHostToolPhase, FrontierHostToolPhaseKind,
        frontier_inference_synthetic_sensitivity_fixtures,
    };
    use crate::verification_profile::VerificationProfileId;
    use crate::verification_profile_registry::{
        GLAEDA_REQUIRED_COMMAND_DIGEST, GLAEDA_REQUIRED_COMMAND_ID, GLAEDA_REQUIRED_PROFILE_ID,
    };

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    const SHA256_PREFIX: &str = "sha256:";
    const HEX: &[u8; 16] = b"0123456789abcdef";

    fn request_id(value: &str) -> ExecutionRequestId {
        ExecutionRequestId::parse(value).expect("test request id must validate")
    }

    fn resources(entries: &[(CapacityDimension, u64)]) -> CapacityAmounts {
        CapacityAmounts::new(entries).expect("test resources must validate")
    }

    fn digest(bytes: &[u8]) -> Sha256Digest {
        let digest = Sha256::digest(bytes);
        let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
        value.push_str(SHA256_PREFIX);
        for byte in digest {
            value.push(HEX[(byte >> 4) as usize] as char);
            value.push(HEX[(byte & 0x0f) as usize] as char);
        }
        Sha256Digest::parse(&value).expect("SHA-256 encoder must produce a canonical digest")
    }

    fn simple_workload(family: &str) -> ComputeWorkloadIdentity {
        ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse(family).expect("test family must validate"),
            ComputeSemanticGeneration::new(1).expect("test generation must validate"),
            ComputeInputIdentity::new(digest(b"test-input")),
            ComputeTrustClass::Trusted,
            ComputeCapabilitySet::new(vec![
                ComputeCapabilityId::parse("linux.arm64").expect("test capability must validate"),
            ])
            .expect("test capability set must validate"),
            ComputeOutputContractIdentity::new(digest(b"test-output-contract")),
        )
    }

    #[derive(Serialize)]
    struct RepositoryVerificationSemanticDocument<'a> {
        repository: &'a str,
        source_commit: &'a str,
        source_tree: &'a str,
        verification_profile_id: &'a str,
        command_id: &'a str,
        command_digest: &'a str,
    }

    fn repository_verification_workload() -> ComputeWorkloadIdentity {
        let document = RepositoryVerificationSemanticDocument {
            repository: "teamleaderleo/glaeda",
            source_commit: "1111111111111111111111111111111111111111",
            source_tree: "2222222222222222222222222222222222222222",
            verification_profile_id: GLAEDA_REQUIRED_PROFILE_ID,
            command_id: GLAEDA_REQUIRED_COMMAND_ID,
            command_digest: GLAEDA_REQUIRED_COMMAND_DIGEST,
        };
        let input = serde_json::to_vec(&document)
            .expect("typed repository verification semantics must serialize");
        ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse("repository_verification.v1")
                .expect("fixed repository verification family must validate"),
            ComputeSemanticGeneration::new(1).expect("fixed semantic generation must validate"),
            ComputeInputIdentity::new(digest(&input)),
            ComputeTrustClass::Trusted,
            ComputeCapabilitySet::new(vec![
                ComputeCapabilityId::parse("rust.1.97.1")
                    .expect("fixed Rust capability must validate"),
            ])
            .expect("fixed repository capability set must validate"),
            ComputeOutputContractIdentity::new(digest(b"repository-verification-output-v1")),
        )
    }

    fn frontier_family_peak_resources(phases: &[FrontierHostToolPhase]) -> CapacityAmounts {
        let dimensions = [
            CapacityDimension::CpuMillis,
            CapacityDimension::MemoryBytes,
            CapacityDimension::DiskBytes,
            CapacityDimension::Pids,
        ];
        let entries = dimensions.map(|dimension| {
            let peak = phases
                .iter()
                .map(|phase| {
                    phase
                        .host_capacity_demand()
                        .amount(dimension)
                        .expect("frontier fixture phase must track all host dimensions")
                })
                .max()
                .expect("frontier fixture must contain at least one host phase");
            (dimension, peak)
        });
        CapacityAmounts::new(&entries).expect("frontier peak resource envelope must validate")
    }

    #[test]
    fn request_is_exact_workload_plus_supplied_resources() {
        let request_id = request_id("request-alpha-1");
        let workload = simple_workload("dataset_transform.v1");
        let requested_resources = resources(&[
            (CapacityDimension::CpuMillis, 2_000),
            (CapacityDimension::MemoryBytes, 2 * GIB),
        ]);
        let request = ComputeExecutionRequest::new(
            request_id.clone(),
            workload.clone(),
            requested_resources.clone(),
        );

        assert_eq!(
            request.schema_version(),
            COMPUTE_EXECUTION_REQUEST_SCHEMA_VERSION
        );
        assert_eq!(request.request_id(), &request_id);
        assert_eq!(request.workload(), &workload);
        assert_eq!(request.requested_resources(), &requested_resources);
    }

    #[test]
    fn different_resource_envelopes_do_not_change_semantic_workload_identity() {
        let request_id = request_id("request-alpha-2");
        let workload = simple_workload("dataset_transform.v1");
        let smaller = ComputeExecutionRequest::new(
            request_id.clone(),
            workload.clone(),
            resources(&[
                (CapacityDimension::CpuMillis, 2_000),
                (CapacityDimension::MemoryBytes, 2 * GIB),
            ]),
        );
        let larger = ComputeExecutionRequest::new(
            request_id,
            workload.clone(),
            resources(&[
                (CapacityDimension::CpuMillis, 4_000),
                (CapacityDimension::MemoryBytes, 4 * GIB),
            ]),
        );

        assert_eq!(smaller.workload(), &workload);
        assert_eq!(larger.workload(), &workload);
        assert_ne!(smaller, larger);
    }

    #[test]
    fn tracked_zero_and_untracked_dimensions_survive_exactly() {
        let request = ComputeExecutionRequest::new(
            request_id("request-zero-1"),
            simple_workload("dataset_transform.v1"),
            resources(&[
                (CapacityDimension::CpuMillis, 0),
                (CapacityDimension::MemoryBytes, 512 * MIB),
            ]),
        );

        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::CpuMillis),
            Some(0)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::DiskBytes),
            None
        );

        let value = serde_json::to_value(&request).expect("request JSON must serialize");
        let amounts = value["requested_resources"]["amounts"]
            .as_object()
            .expect("capacity amounts must serialize as an object");
        assert_eq!(
            amounts.get("cpu_millis").and_then(|value| value.as_u64()),
            Some(0)
        );
        assert!(!amounts.contains_key("disk_bytes"));
    }

    #[test]
    fn invalid_components_refuse_before_request_construction() {
        assert!(ExecutionRequestId::parse("/tmp/request").is_err());
        assert!(CapacityAmounts::new(&[]).is_err());
        assert!(ComputeWorkloadFamilyId::parse("/tmp/family").is_err());
    }

    #[test]
    fn verification_runner_policy_stays_outside_generic_semantic_identity() {
        let request_id = request_id("request-verification-1");
        let workload = repository_verification_workload();
        let verification_profile = VerificationProfileId::parse(GLAEDA_REQUIRED_PROFILE_ID)
            .expect("canonical verification profile id must validate");
        let interactive = ExecutionAdmissionIdentity::new(
            request_id.clone(),
            verification_profile.clone(),
            RunnerProfileId::parse("interactive").expect("runner profile must validate"),
        );
        let work = ExecutionAdmissionIdentity::new(
            request_id.clone(),
            verification_profile,
            RunnerProfileId::parse("work").expect("runner profile must validate"),
        );
        let request = ComputeExecutionRequest::new(
            request_id,
            workload.clone(),
            resources(&[
                (CapacityDimension::CpuMillis, 4_000),
                (CapacityDimension::MemoryBytes, 4 * GIB),
                (CapacityDimension::Pids, 128),
            ]),
        );

        assert_ne!(interactive, work);
        assert_eq!(request.workload(), &workload);

        let json = serde_json::to_string(&request).expect("generic request JSON must serialize");
        assert!(!json.contains(GLAEDA_REQUIRED_PROFILE_ID));
        assert!(!json.contains("runner_profile_id"));
        assert!(!json.contains("verification_profile_id"));
    }

    #[test]
    fn frontier_family_chooses_peak_envelope_before_generic_request_construction() {
        let fixture = frontier_inference_synthetic_sensitivity_fixtures()
            .into_iter()
            .next()
            .expect("frontier synthetic fixture must exist");
        let peak = frontier_family_peak_resources(fixture.receipt().host_tool_phases());
        let request = ComputeExecutionRequest::new(
            request_id("request-frontier-1"),
            fixture.receipt().workload().clone(),
            peak,
        );

        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::CpuMillis),
            Some(4_000)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::MemoryBytes),
            Some(4 * GIB)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::DiskBytes),
            Some(8 * GIB)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::Pids),
            Some(128)
        );

        let summed_cpu: u64 = fixture
            .receipt()
            .host_tool_phases()
            .iter()
            .map(|phase| {
                phase
                    .host_capacity_demand()
                    .amount(CapacityDimension::CpuMillis)
                    .expect("frontier fixture CPU must be tracked")
            })
            .sum();
        assert_eq!(summed_cpu, 10_000);
        assert_ne!(
            request
                .requested_resources()
                .amount(CapacityDimension::CpuMillis),
            Some(summed_cpu)
        );

        let json = serde_json::to_string(&request).expect("frontier request JSON must serialize");
        assert!(json.contains("frontier_inference.v1"));
        for forbidden in [
            "verification_profile_id",
            "runner_profile_id",
            "repository",
            "pull_request",
            "test_name",
        ] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn sequential_phase_fixture_uses_family_peak_instead_of_sum() {
        let phase_a = FrontierHostToolPhase::new(
            FrontierHostToolPhaseKind::BuildTest,
            100,
            resources(&[
                (CapacityDimension::CpuMillis, 2_000),
                (CapacityDimension::MemoryBytes, GIB),
                (CapacityDimension::DiskBytes, 0),
                (CapacityDimension::Pids, 32),
            ]),
        )
        .expect("first sequential phase must validate");
        let phase_b = FrontierHostToolPhase::new(
            FrontierHostToolPhaseKind::IndexDataTransform,
            100,
            resources(&[
                (CapacityDimension::CpuMillis, 2_000),
                (CapacityDimension::MemoryBytes, GIB),
                (CapacityDimension::DiskBytes, 0),
                (CapacityDimension::Pids, 32),
            ]),
        )
        .expect("second sequential phase must validate");
        let peak = frontier_family_peak_resources(&[phase_a, phase_b]);
        let request = ComputeExecutionRequest::new(
            request_id("request-frontier-sequential-1"),
            simple_workload("frontier_inference.v1"),
            peak,
        );

        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::CpuMillis),
            Some(2_000)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::MemoryBytes),
            Some(GIB)
        );
    }

    #[test]
    fn public_request_contains_no_attempt_claim_or_execution_authority() {
        let request = ComputeExecutionRequest::new(
            request_id("request-public-1"),
            simple_workload("dataset_transform.v1"),
            resources(&[
                (CapacityDimension::CpuMillis, 1_000),
                (CapacityDimension::MemoryBytes, GIB),
            ]),
        );
        let json = serde_json::to_string(&request).expect("request JSON must serialize");
        let debug = format!("{request:?}");

        for surface in [&json, &debug] {
            for forbidden in [
                "capacity_claim",
                "claim_id",
                "capacity_domain",
                "reservation",
                "attempt_generation",
                "runner_profile",
                "verification_profile",
                "backend",
                "program",
                "argv",
                "environment",
                "credential",
                "token",
                "path",
            ] {
                assert!(!surface.contains(forbidden));
            }
        }
    }
}
