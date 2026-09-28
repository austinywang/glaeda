//! Pure preregistration and sample binding for resident-backend comparisons.
//!
//! This module records an already-decided experiment. It grants zero backend execution,
//! lifecycle, host mutation, benchmark, placement, promotion, or default-selection authority.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::hot_execution_performance::{
    HotExecutionMode, HotExecutionPerformanceReceipt, HotExecutionResultClass, HotSandboxState,
};

pub const RESIDENT_BACKEND_COMPARISON_SCHEMA_VERSION: u8 = 1;
pub const REQUIRED_AA_NOISE_BLOCKS: usize = 4;
pub const MAX_RESIDENT_BACKEND_COMPARISON_BLOCKS: usize = 32;
pub const MAX_RESIDENT_BACKEND_SAMPLE_ORDINAL: u32 = 1_000_000;

const MAX_TOKEN_BYTES: usize = 96;
const PLAN_DIGEST_DOMAIN: &[u8] = b"glaeda.resident_backend_comparison.plan.v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResidentBackendComparisonDocumentType {
    Plan,
    Sample,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentBackendComparisonAuthority {
    ObservationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentBackendTreatmentArm {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentBackendComparisonPosition {
    First,
    Second,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentBackendSampleClass {
    StoppedCanary,
    ResidentCanary,
    ResidentTask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResidentBackendSemanticValidation {
    NotApplicable,
    Passed { evidence_digest: Sha256Digest },
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
struct ResidentBackendComparisonToken(String);

impl ResidentBackendComparisonToken {
    fn parse(value: &str) -> Result<Self, ResidentBackendComparisonError> {
        let Some(first) = value.bytes().next() else {
            return Err(invalid_token());
        };
        if value.len() > MAX_TOKEN_BYTES
            || !(first.is_ascii_lowercase() || first.is_ascii_digit())
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-' | b':')
            })
        {
            return Err(invalid_token());
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentBackendTreatmentIdentity {
    candidate_id: ResidentBackendComparisonToken,
    backend_id: ResidentBackendComparisonToken,
    backend_generation: ResidentBackendComparisonToken,
    guest_image_generation: ResidentBackendComparisonToken,
    kernel_generation: ResidentBackendComparisonToken,
    host_class: ResidentBackendComparisonToken,
    resource_profile: ResidentBackendComparisonToken,
    storage_policy_generation: ResidentBackendComparisonToken,
    network_policy_generation: ResidentBackendComparisonToken,
    cleanup_policy_generation: ResidentBackendComparisonToken,
    reconstruction_policy_generation: ResidentBackendComparisonToken,
}

impl ResidentBackendTreatmentIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        candidate_id: &str,
        backend_id: &str,
        backend_generation: &str,
        guest_image_generation: &str,
        kernel_generation: &str,
        host_class: &str,
        resource_profile: &str,
        storage_policy_generation: &str,
        network_policy_generation: &str,
        cleanup_policy_generation: &str,
        reconstruction_policy_generation: &str,
    ) -> Result<Self, ResidentBackendComparisonError> {
        Ok(Self {
            candidate_id: ResidentBackendComparisonToken::parse(candidate_id)?,
            backend_id: ResidentBackendComparisonToken::parse(backend_id)?,
            backend_generation: ResidentBackendComparisonToken::parse(backend_generation)?,
            guest_image_generation: ResidentBackendComparisonToken::parse(guest_image_generation)?,
            kernel_generation: ResidentBackendComparisonToken::parse(kernel_generation)?,
            host_class: ResidentBackendComparisonToken::parse(host_class)?,
            resource_profile: ResidentBackendComparisonToken::parse(resource_profile)?,
            storage_policy_generation: ResidentBackendComparisonToken::parse(
                storage_policy_generation,
            )?,
            network_policy_generation: ResidentBackendComparisonToken::parse(
                network_policy_generation,
            )?,
            cleanup_policy_generation: ResidentBackendComparisonToken::parse(
                cleanup_policy_generation,
            )?,
            reconstruction_policy_generation: ResidentBackendComparisonToken::parse(
                reconstruction_policy_generation,
            )?,
        })
    }

    #[must_use]
    pub fn candidate_id(&self) -> &str {
        self.candidate_id.as_str()
    }

    #[must_use]
    pub fn backend_id(&self) -> &str {
        self.backend_id.as_str()
    }

    #[must_use]
    pub fn backend_generation(&self) -> &str {
        self.backend_generation.as_str()
    }

    #[must_use]
    pub fn guest_image_generation(&self) -> &str {
        self.guest_image_generation.as_str()
    }

    #[must_use]
    pub fn kernel_generation(&self) -> &str {
        self.kernel_generation.as_str()
    }

    #[must_use]
    pub fn host_class(&self) -> &str {
        self.host_class.as_str()
    }

    #[must_use]
    pub fn resource_profile(&self) -> &str {
        self.resource_profile.as_str()
    }

    #[must_use]
    pub fn storage_policy_generation(&self) -> &str {
        self.storage_policy_generation.as_str()
    }

    #[must_use]
    pub fn network_policy_generation(&self) -> &str {
        self.network_policy_generation.as_str()
    }

    #[must_use]
    pub fn cleanup_policy_generation(&self) -> &str {
        self.cleanup_policy_generation.as_str()
    }

    #[must_use]
    pub fn reconstruction_policy_generation(&self) -> &str {
        self.reconstruction_policy_generation.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResidentBackendComparisonBlock {
    block: u16,
    first: ResidentBackendTreatmentArm,
    second: ResidentBackendTreatmentArm,
}

impl ResidentBackendComparisonBlock {
    #[must_use]
    pub const fn new(
        block: u16,
        first: ResidentBackendTreatmentArm,
        second: ResidentBackendTreatmentArm,
    ) -> Self {
        Self {
            block,
            first,
            second,
        }
    }

    #[must_use]
    pub const fn block(self) -> u16 {
        self.block
    }

    #[must_use]
    pub const fn first(self) -> ResidentBackendTreatmentArm {
        self.first
    }

    #[must_use]
    pub const fn second(self) -> ResidentBackendTreatmentArm {
        self.second
    }

    const fn arm_at(
        self,
        position: ResidentBackendComparisonPosition,
    ) -> ResidentBackendTreatmentArm {
        match position {
            ResidentBackendComparisonPosition::First => self.first,
            ResidentBackendComparisonPosition::Second => self.second,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentBackendComparisonPlan {
    document_type: ResidentBackendComparisonDocumentType,
    schema_version: u8,
    authority: ResidentBackendComparisonAuthority,
    experiment_id: ResidentBackendComparisonToken,
    glaeda_source_id: ResidentBackendComparisonToken,
    workload_id: ResidentBackendComparisonToken,
    project_id: ResidentBackendComparisonToken,
    project_source_id: ResidentBackendComparisonToken,
    validator_id: ResidentBackendComparisonToken,
    toolchain_generation: ResidentBackendComparisonToken,
    treatment_a: ResidentBackendTreatmentIdentity,
    treatment_b: ResidentBackendTreatmentIdentity,
    blocks: Vec<ResidentBackendComparisonBlock>,
    plan_digest: Sha256Digest,
}

impl ResidentBackendComparisonPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        experiment_id: &str,
        glaeda_source_id: &str,
        workload_id: &str,
        project_id: &str,
        project_source_id: &str,
        validator_id: &str,
        toolchain_generation: &str,
        treatment_a: ResidentBackendTreatmentIdentity,
        treatment_b: ResidentBackendTreatmentIdentity,
        blocks: Vec<ResidentBackendComparisonBlock>,
    ) -> Result<Self, ResidentBackendComparisonError> {
        validate_treatments(&treatment_a, &treatment_b)?;
        validate_blocks(&blocks)?;
        let mut plan = Self {
            document_type: ResidentBackendComparisonDocumentType::Plan,
            schema_version: RESIDENT_BACKEND_COMPARISON_SCHEMA_VERSION,
            authority: ResidentBackendComparisonAuthority::ObservationOnly,
            experiment_id: ResidentBackendComparisonToken::parse(experiment_id)?,
            glaeda_source_id: ResidentBackendComparisonToken::parse(glaeda_source_id)?,
            workload_id: ResidentBackendComparisonToken::parse(workload_id)?,
            project_id: ResidentBackendComparisonToken::parse(project_id)?,
            project_source_id: ResidentBackendComparisonToken::parse(project_source_id)?,
            validator_id: ResidentBackendComparisonToken::parse(validator_id)?,
            toolchain_generation: ResidentBackendComparisonToken::parse(toolchain_generation)?,
            treatment_a,
            treatment_b,
            blocks,
            plan_digest: zero_digest()?,
        };
        plan.plan_digest = derive_plan_digest(&plan)?;
        Ok(plan)
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn authority(&self) -> ResidentBackendComparisonAuthority {
        self.authority
    }

    #[must_use]
    pub fn experiment_id(&self) -> &str {
        self.experiment_id.as_str()
    }

    #[must_use]
    pub fn glaeda_source_id(&self) -> &str {
        self.glaeda_source_id.as_str()
    }

    #[must_use]
    pub fn workload_id(&self) -> &str {
        self.workload_id.as_str()
    }

    #[must_use]
    pub fn project_id(&self) -> &str {
        self.project_id.as_str()
    }

    #[must_use]
    pub fn project_source_id(&self) -> &str {
        self.project_source_id.as_str()
    }

    #[must_use]
    pub fn validator_id(&self) -> &str {
        self.validator_id.as_str()
    }

    #[must_use]
    pub fn toolchain_generation(&self) -> &str {
        self.toolchain_generation.as_str()
    }

    #[must_use]
    pub const fn treatment_a(&self) -> &ResidentBackendTreatmentIdentity {
        &self.treatment_a
    }

    #[must_use]
    pub const fn treatment_b(&self) -> &ResidentBackendTreatmentIdentity {
        &self.treatment_b
    }

    #[must_use]
    pub fn blocks(&self) -> &[ResidentBackendComparisonBlock] {
        &self.blocks
    }

    #[must_use]
    pub const fn plan_digest(&self) -> &Sha256Digest {
        &self.plan_digest
    }

    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    fn treatment(&self, arm: ResidentBackendTreatmentArm) -> &ResidentBackendTreatmentIdentity {
        match arm {
            ResidentBackendTreatmentArm::A => &self.treatment_a,
            ResidentBackendTreatmentArm::B => &self.treatment_b,
        }
    }

    fn scheduled_arm(
        &self,
        block: u16,
        position: ResidentBackendComparisonPosition,
    ) -> Result<ResidentBackendTreatmentArm, ResidentBackendComparisonError> {
        self.blocks
            .iter()
            .copied()
            .find(|candidate| candidate.block == block)
            .map(|candidate| candidate.arm_at(position))
            .ok_or_else(sample_schedule_mismatch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentBackendComparisonSample {
    document_type: ResidentBackendComparisonDocumentType,
    schema_version: u8,
    authority: ResidentBackendComparisonAuthority,
    experiment_id: ResidentBackendComparisonToken,
    block: u16,
    position: ResidentBackendComparisonPosition,
    arm: ResidentBackendTreatmentArm,
    sample_class: ResidentBackendSampleClass,
    ordinal: u32,
    plan_digest: Sha256Digest,
    treatment: ResidentBackendTreatmentIdentity,
    semantic_validation: ResidentBackendSemanticValidation,
    performance: HotExecutionPerformanceReceipt,
}

impl ResidentBackendComparisonSample {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan: &ResidentBackendComparisonPlan,
        block: u16,
        position: ResidentBackendComparisonPosition,
        sample_class: ResidentBackendSampleClass,
        ordinal: u32,
        observed_treatment: ResidentBackendTreatmentIdentity,
        semantic_validation: ResidentBackendSemanticValidation,
        performance: HotExecutionPerformanceReceipt,
    ) -> Result<Self, ResidentBackendComparisonError> {
        if !(1..=MAX_RESIDENT_BACKEND_SAMPLE_ORDINAL).contains(&ordinal) {
            return Err(invalid_sample_ordinal());
        }
        let arm = plan.scheduled_arm(block, position)?;
        validate_treatment_binding(plan, arm, &observed_treatment)?;
        validate_performance_binding(plan, arm, &performance)?;
        validate_sample_mode(sample_class, &performance)?;
        validate_semantic_validation(sample_class, &semantic_validation, &performance)?;
        Ok(Self {
            document_type: ResidentBackendComparisonDocumentType::Sample,
            schema_version: RESIDENT_BACKEND_COMPARISON_SCHEMA_VERSION,
            authority: ResidentBackendComparisonAuthority::ObservationOnly,
            experiment_id: plan.experiment_id.clone(),
            block,
            position,
            arm,
            sample_class,
            ordinal,
            plan_digest: plan.plan_digest.clone(),
            treatment: observed_treatment,
            semantic_validation,
            performance,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn authority(&self) -> ResidentBackendComparisonAuthority {
        self.authority
    }

    #[must_use]
    pub fn experiment_id(&self) -> &str {
        self.experiment_id.as_str()
    }

    #[must_use]
    pub const fn block(&self) -> u16 {
        self.block
    }

    #[must_use]
    pub const fn position(&self) -> ResidentBackendComparisonPosition {
        self.position
    }

    #[must_use]
    pub const fn arm(&self) -> ResidentBackendTreatmentArm {
        self.arm
    }

    #[must_use]
    pub const fn sample_class(&self) -> ResidentBackendSampleClass {
        self.sample_class
    }

    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    #[must_use]
    pub const fn plan_digest(&self) -> &Sha256Digest {
        &self.plan_digest
    }

    #[must_use]
    pub const fn treatment(&self) -> &ResidentBackendTreatmentIdentity {
        &self.treatment
    }

    #[must_use]
    pub const fn semantic_validation(&self) -> &ResidentBackendSemanticValidation {
        &self.semantic_validation
    }

    #[must_use]
    pub const fn performance(&self) -> &HotExecutionPerformanceReceipt {
        &self.performance
    }

    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentBackendComparisonError {
    code: &'static str,
    message: &'static str,
}

impl ResidentBackendComparisonError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ResidentBackendComparisonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ResidentBackendComparisonError {}

fn validate_treatments(
    treatment_a: &ResidentBackendTreatmentIdentity,
    treatment_b: &ResidentBackendTreatmentIdentity,
) -> Result<(), ResidentBackendComparisonError> {
    if treatment_a.backend_id == treatment_b.backend_id
        || treatment_a.candidate_id == treatment_b.candidate_id
    {
        return Err(invalid_treatment_pair());
    }
    if treatment_a.host_class != treatment_b.host_class
        || treatment_a.resource_profile != treatment_b.resource_profile
    {
        return Err(incomparable_treatment_pair());
    }
    Ok(())
}

fn validate_blocks(
    blocks: &[ResidentBackendComparisonBlock],
) -> Result<(), ResidentBackendComparisonError> {
    if blocks.len() < REQUIRED_AA_NOISE_BLOCKS + 1
        || blocks.len() > MAX_RESIDENT_BACKEND_COMPARISON_BLOCKS
    {
        return Err(invalid_block_schedule());
    }
    for (index, block) in blocks.iter().copied().enumerate() {
        let expected = u16::try_from(index + 1).map_err(|_| invalid_block_schedule())?;
        if block.block != expected {
            return Err(invalid_block_schedule());
        }
        if index < REQUIRED_AA_NOISE_BLOCKS {
            if block.first != ResidentBackendTreatmentArm::A
                || block.second != ResidentBackendTreatmentArm::A
            {
                return Err(invalid_aa_schedule());
            }
        } else if block.first == block.second {
            return Err(invalid_crossover_schedule());
        }
    }
    Ok(())
}

fn validate_treatment_binding(
    plan: &ResidentBackendComparisonPlan,
    arm: ResidentBackendTreatmentArm,
    observed: &ResidentBackendTreatmentIdentity,
) -> Result<(), ResidentBackendComparisonError> {
    if observed != plan.treatment(arm) {
        return Err(treatment_identity_mismatch());
    }
    Ok(())
}

fn validate_performance_binding(
    plan: &ResidentBackendComparisonPlan,
    arm: ResidentBackendTreatmentArm,
    performance: &HotExecutionPerformanceReceipt,
) -> Result<(), ResidentBackendComparisonError> {
    let identity = performance.identity();
    let treatment = plan.treatment(arm);
    if identity.workload_id() != plan.workload_id()
        || identity.project_id() != plan.project_id()
        || identity.source_id() != plan.project_source_id()
        || identity.candidate_id() != treatment.candidate_id()
        || identity.backend_id() != treatment.backend_id()
        || identity.host_class() != treatment.host_class()
        || identity.resource_profile() != treatment.resource_profile()
    {
        return Err(performance_identity_mismatch());
    }
    Ok(())
}

fn validate_sample_mode(
    sample_class: ResidentBackendSampleClass,
    performance: &HotExecutionPerformanceReceipt,
) -> Result<(), ResidentBackendComparisonError> {
    let expected = match sample_class {
        ResidentBackendSampleClass::StoppedCanary => (
            HotExecutionMode::ResidentAfterIdle,
            HotSandboxState::Resumed,
        ),
        ResidentBackendSampleClass::ResidentCanary => (
            HotExecutionMode::ResidentImmediate,
            HotSandboxState::ResidentHit,
        ),
        ResidentBackendSampleClass::ResidentTask => (
            HotExecutionMode::ResidentTaskLoop,
            HotSandboxState::ResidentHit,
        ),
    };
    if performance.execution_mode() != expected.0 || performance.heat().sandbox() != expected.1 {
        return Err(sample_mode_mismatch());
    }
    Ok(())
}

fn derive_plan_digest(
    plan: &ResidentBackendComparisonPlan,
) -> Result<Sha256Digest, ResidentBackendComparisonError> {
    let mut hasher = Sha256::new();
    hasher.update(PLAN_DIGEST_DOMAIN);
    hasher.update([plan.schema_version]);
    hasher.update([1]); // observation-only authority
    for token in [
        &plan.experiment_id,
        &plan.glaeda_source_id,
        &plan.workload_id,
        &plan.project_id,
        &plan.project_source_id,
        &plan.validator_id,
        &plan.toolchain_generation,
    ] {
        update_digest_token(&mut hasher, token);
    }
    update_digest_treatment(&mut hasher, &plan.treatment_a);
    update_digest_treatment(&mut hasher, &plan.treatment_b);
    hasher.update((plan.blocks.len() as u16).to_be_bytes());
    for block in &plan.blocks {
        hasher.update(block.block.to_be_bytes());
        hasher.update([digest_arm(block.first), digest_arm(block.second)]);
    }
    digest_to_sha256(&hasher.finalize())
}

fn update_digest_treatment(hasher: &mut Sha256, treatment: &ResidentBackendTreatmentIdentity) {
    for token in [
        &treatment.candidate_id,
        &treatment.backend_id,
        &treatment.backend_generation,
        &treatment.guest_image_generation,
        &treatment.kernel_generation,
        &treatment.host_class,
        &treatment.resource_profile,
        &treatment.storage_policy_generation,
        &treatment.network_policy_generation,
        &treatment.cleanup_policy_generation,
        &treatment.reconstruction_policy_generation,
    ] {
        update_digest_token(hasher, token);
    }
}

fn update_digest_token(hasher: &mut Sha256, token: &ResidentBackendComparisonToken) {
    let bytes = token.as_str().as_bytes();
    hasher.update((bytes.len() as u16).to_be_bytes());
    hasher.update(bytes);
}

const fn digest_arm(arm: ResidentBackendTreatmentArm) -> u8 {
    match arm {
        ResidentBackendTreatmentArm::A => 1,
        ResidentBackendTreatmentArm::B => 2,
    }
}

fn digest_to_sha256(bytes: &[u8]) -> Result<Sha256Digest, ResidentBackendComparisonError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| plan_digest_encoding())
}

fn zero_digest() -> Result<Sha256Digest, ResidentBackendComparisonError> {
    Sha256Digest::parse("sha256:0000000000000000000000000000000000000000000000000000000000000000")
        .map_err(|_| plan_digest_encoding())
}

fn validate_semantic_validation(
    sample_class: ResidentBackendSampleClass,
    validation: &ResidentBackendSemanticValidation,
    performance: &HotExecutionPerformanceReceipt,
) -> Result<(), ResidentBackendComparisonError> {
    match (sample_class, validation) {
        (
            ResidentBackendSampleClass::StoppedCanary | ResidentBackendSampleClass::ResidentCanary,
            ResidentBackendSemanticValidation::NotApplicable,
        ) => Ok(()),
        (
            ResidentBackendSampleClass::StoppedCanary | ResidentBackendSampleClass::ResidentCanary,
            _,
        ) => Err(canary_validation_mismatch()),
        (
            ResidentBackendSampleClass::ResidentTask,
            ResidentBackendSemanticValidation::NotApplicable,
        ) => Err(task_validation_missing()),
        (
            ResidentBackendSampleClass::ResidentTask,
            ResidentBackendSemanticValidation::Passed { .. },
        ) => {
            if performance.result() != HotExecutionResultClass::Succeeded
                || performance
                    .milestones()
                    .final_relevant_result_millis()
                    .is_none()
            {
                return Err(task_validation_result_mismatch());
            }
            Ok(())
        }
        (ResidentBackendSampleClass::ResidentTask, ResidentBackendSemanticValidation::Failed) => {
            Ok(())
        }
    }
}

const fn error(code: &'static str, message: &'static str) -> ResidentBackendComparisonError {
    ResidentBackendComparisonError { code, message }
}

const fn invalid_token() -> ResidentBackendComparisonError {
    error(
        "invalid_backend_comparison_token",
        "backend comparison identity must be bounded lowercase ASCII without path or log content",
    )
}

const fn invalid_treatment_pair() -> ResidentBackendComparisonError {
    error(
        "invalid_backend_treatment_pair",
        "backend comparison treatments require distinct candidate and backend identities",
    )
}

const fn incomparable_treatment_pair() -> ResidentBackendComparisonError {
    error(
        "incomparable_backend_treatment_pair",
        "backend comparison treatments require the same host class and resource profile",
    )
}

const fn invalid_block_schedule() -> ResidentBackendComparisonError {
    error(
        "invalid_backend_comparison_schedule",
        "backend comparison schedule must contain consecutive bounded blocks with A/A noise before crossover",
    )
}

const fn invalid_aa_schedule() -> ResidentBackendComparisonError {
    error(
        "invalid_backend_aa_schedule",
        "the first four backend comparison blocks must be A/A noise blocks",
    )
}

const fn invalid_crossover_schedule() -> ResidentBackendComparisonError {
    error(
        "invalid_backend_crossover_schedule",
        "backend comparison crossover blocks must contain one A and one B treatment",
    )
}

const fn sample_schedule_mismatch() -> ResidentBackendComparisonError {
    error(
        "backend_sample_schedule_mismatch",
        "backend comparison sample does not name one preregistered block position",
    )
}

const fn invalid_sample_ordinal() -> ResidentBackendComparisonError {
    error(
        "invalid_backend_sample_ordinal",
        "backend comparison sample ordinal is outside the bounded positive range",
    )
}

const fn treatment_identity_mismatch() -> ResidentBackendComparisonError {
    error(
        "backend_sample_treatment_mismatch",
        "observed backend treatment identity does not match the preregistered treatment",
    )
}

const fn performance_identity_mismatch() -> ResidentBackendComparisonError {
    error(
        "backend_sample_identity_mismatch",
        "performance receipt identity does not match the preregistered backend treatment",
    )
}

const fn sample_mode_mismatch() -> ResidentBackendComparisonError {
    error(
        "backend_sample_mode_mismatch",
        "backend comparison sample class does not match the observed execution mode and sandbox heat",
    )
}

const fn plan_digest_encoding() -> ResidentBackendComparisonError {
    error(
        "backend_plan_digest_encoding_failed",
        "backend comparison plan could not be represented by the canonical digest type",
    )
}

const fn canary_validation_mismatch() -> ResidentBackendComparisonError {
    error(
        "backend_canary_validation_mismatch",
        "backend canary samples cannot carry repository-task validation evidence",
    )
}

const fn task_validation_missing() -> ResidentBackendComparisonError {
    error(
        "backend_task_validation_missing",
        "resident task samples require an explicit repository semantic-validation outcome",
    )
}

const fn task_validation_result_mismatch() -> ResidentBackendComparisonError {
    error(
        "backend_task_validation_result_mismatch",
        "passed repository validation requires one successful performance receipt with a final relevant result",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hot_execution_performance::{
        HotBuildState, HotDependencyState, HotExecutionHeat, HotExecutionMilestones,
        HotExecutionMode, HotExecutionPerformanceIdentity, HotIndexServiceState,
        HotRepositoryState, HotSandboxState,
    };

    fn treatment_a() -> ResidentBackendTreatmentIdentity {
        ResidentBackendTreatmentIdentity::new(
            "resident-lima-vz",
            "lima-vz",
            "lima-2.2.0",
            "glaeda-guest-v1",
            "linux-kernel-lima-v1",
            "apple-silicon-m",
            "4cpu-8gib",
            "guest-local-v1",
            "controlled-network-v1",
            "cleanup-v1",
            "reconstruction-v1",
        )
        .unwrap()
    }

    fn treatment_b() -> ResidentBackendTreatmentIdentity {
        ResidentBackendTreatmentIdentity::new(
            "resident-apple-container",
            "apple-container-machine",
            "container-1.3.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "linux-kernel-container-v1",
            "apple-silicon-m",
            "4cpu-8gib",
            "guest-local-v1",
            "controlled-network-v1",
            "cleanup-v1",
            "reconstruction-v1",
        )
        .unwrap()
    }

    fn blocks() -> Vec<ResidentBackendComparisonBlock> {
        vec![
            ResidentBackendComparisonBlock::new(
                1,
                ResidentBackendTreatmentArm::A,
                ResidentBackendTreatmentArm::A,
            ),
            ResidentBackendComparisonBlock::new(
                2,
                ResidentBackendTreatmentArm::A,
                ResidentBackendTreatmentArm::A,
            ),
            ResidentBackendComparisonBlock::new(
                3,
                ResidentBackendTreatmentArm::A,
                ResidentBackendTreatmentArm::A,
            ),
            ResidentBackendComparisonBlock::new(
                4,
                ResidentBackendTreatmentArm::A,
                ResidentBackendTreatmentArm::A,
            ),
            ResidentBackendComparisonBlock::new(
                5,
                ResidentBackendTreatmentArm::A,
                ResidentBackendTreatmentArm::B,
            ),
            ResidentBackendComparisonBlock::new(
                6,
                ResidentBackendTreatmentArm::B,
                ResidentBackendTreatmentArm::A,
            ),
        ]
    }

    fn plan() -> ResidentBackendComparisonPlan {
        ResidentBackendComparisonPlan::new(
            "resident-backend-2026-08",
            "glaeda-191bd299",
            "quarry-pr-check",
            "quarry",
            "quarry-tree-abc123",
            "quarry-validator-v1",
            "quarry-toolchain-v1",
            treatment_a(),
            treatment_b(),
            blocks(),
        )
        .unwrap()
    }

    fn performance_with_mode(
        treatment: &ResidentBackendTreatmentIdentity,
        mode: HotExecutionMode,
        sandbox: HotSandboxState,
    ) -> HotExecutionPerformanceReceipt {
        let identity = HotExecutionPerformanceIdentity::new(
            "quarry-pr-check",
            "quarry",
            "quarry-tree-abc123",
            treatment.candidate_id(),
            treatment.backend_id(),
            treatment.host_class(),
            treatment.resource_profile(),
        )
        .unwrap();
        let milestones = HotExecutionMilestones::new(
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            Some(7),
        )
        .unwrap();
        let heat = HotExecutionHeat::new(
            sandbox,
            HotRepositoryState::ResidentHit,
            HotDependencyState::ResidentHit,
            HotBuildState::ResidentHit,
            HotIndexServiceState::ResidentHit,
        );
        HotExecutionPerformanceReceipt::new(
            identity,
            mode,
            8,
            milestones,
            heat,
            None,
            None,
            HotExecutionResultClass::Succeeded,
        )
        .unwrap()
    }

    fn performance(treatment: &ResidentBackendTreatmentIdentity) -> HotExecutionPerformanceReceipt {
        performance_with_mode(
            treatment,
            HotExecutionMode::ResidentTaskLoop,
            HotSandboxState::ResidentHit,
        )
    }

    fn validation_digest() -> Sha256Digest {
        Sha256Digest::parse(
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap()
    }

    #[test]
    fn plan_requires_four_aa_noise_blocks_before_crossover() {
        let mut schedule = blocks();
        schedule[3] = ResidentBackendComparisonBlock::new(
            4,
            ResidentBackendTreatmentArm::A,
            ResidentBackendTreatmentArm::B,
        );
        let error = ResidentBackendComparisonPlan::new(
            "experiment",
            "glaeda-source",
            "workload",
            "project",
            "source",
            "validator",
            "toolchain",
            treatment_a(),
            treatment_b(),
            schedule,
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_backend_aa_schedule");
    }

    #[test]
    fn crossover_blocks_require_one_sample_from_each_treatment() {
        let mut schedule = blocks();
        schedule[4] = ResidentBackendComparisonBlock::new(
            5,
            ResidentBackendTreatmentArm::B,
            ResidentBackendTreatmentArm::B,
        );
        let error = ResidentBackendComparisonPlan::new(
            "experiment",
            "glaeda-source",
            "workload",
            "project",
            "source",
            "validator",
            "toolchain",
            treatment_a(),
            treatment_b(),
            schedule,
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_backend_crossover_schedule");
    }

    #[test]
    fn treatments_keep_host_and_resource_profile_constant() {
        let treatment = ResidentBackendTreatmentIdentity::new(
            "other-candidate",
            "other-backend",
            "other-v1",
            "other-guest",
            "other-kernel",
            "different-host",
            "4cpu-8gib",
            "guest-local-v1",
            "controlled-network-v1",
            "cleanup-v1",
            "reconstruction-v1",
        )
        .unwrap();
        let error = ResidentBackendComparisonPlan::new(
            "experiment",
            "glaeda-source",
            "workload",
            "project",
            "source",
            "validator",
            "toolchain",
            treatment_a(),
            treatment,
            blocks(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "incomparable_backend_treatment_pair");
    }

    #[test]
    fn sample_arm_is_derived_from_frozen_schedule_and_receipt_identity() {
        let plan = plan();
        let sample = ResidentBackendComparisonSample::new(
            &plan,
            5,
            ResidentBackendComparisonPosition::Second,
            ResidentBackendSampleClass::ResidentTask,
            1,
            plan.treatment_b().clone(),
            ResidentBackendSemanticValidation::Passed {
                evidence_digest: validation_digest(),
            },
            performance(plan.treatment_b()),
        )
        .unwrap();
        assert_eq!(sample.arm(), ResidentBackendTreatmentArm::B);
        assert_eq!(sample.block(), 5);
        assert_eq!(sample.position(), ResidentBackendComparisonPosition::Second);
        assert_eq!(sample.treatment(), plan.treatment_b());
        assert_eq!(
            sample.performance().identity().backend_id(),
            "apple-container-machine"
        );
        assert_eq!(sample.plan_digest(), plan.plan_digest());
    }

    #[test]
    fn complete_plan_digest_changes_with_toolchain_schedule_and_lifecycle_policy() {
        let original = plan();
        let changed_toolchain = ResidentBackendComparisonPlan::new(
            original.experiment_id(),
            original.glaeda_source_id(),
            original.workload_id(),
            original.project_id(),
            original.project_source_id(),
            original.validator_id(),
            "quarry-toolchain-v2",
            original.treatment_a().clone(),
            original.treatment_b().clone(),
            original.blocks().to_vec(),
        )
        .unwrap();
        let mut changed_schedule_blocks = original.blocks().to_vec();
        changed_schedule_blocks[4] = ResidentBackendComparisonBlock::new(
            5,
            ResidentBackendTreatmentArm::B,
            ResidentBackendTreatmentArm::A,
        );
        let changed_schedule = ResidentBackendComparisonPlan::new(
            original.experiment_id(),
            original.glaeda_source_id(),
            original.workload_id(),
            original.project_id(),
            original.project_source_id(),
            original.validator_id(),
            original.toolchain_generation(),
            original.treatment_a().clone(),
            original.treatment_b().clone(),
            changed_schedule_blocks,
        )
        .unwrap();
        let changed_lifecycle = ResidentBackendTreatmentIdentity::new(
            original.treatment_b().candidate_id(),
            original.treatment_b().backend_id(),
            original.treatment_b().backend_generation(),
            original.treatment_b().guest_image_generation(),
            original.treatment_b().kernel_generation(),
            original.treatment_b().host_class(),
            original.treatment_b().resource_profile(),
            original.treatment_b().storage_policy_generation(),
            original.treatment_b().network_policy_generation(),
            "cleanup-v2",
            original.treatment_b().reconstruction_policy_generation(),
        )
        .unwrap();
        let changed_lifecycle = ResidentBackendComparisonPlan::new(
            original.experiment_id(),
            original.glaeda_source_id(),
            original.workload_id(),
            original.project_id(),
            original.project_source_id(),
            original.validator_id(),
            original.toolchain_generation(),
            original.treatment_a().clone(),
            changed_lifecycle,
            original.blocks().to_vec(),
        )
        .unwrap();

        assert_ne!(original.plan_digest(), changed_toolchain.plan_digest());
        assert_ne!(original.plan_digest(), changed_schedule.plan_digest());
        assert_ne!(original.plan_digest(), changed_lifecycle.plan_digest());
        assert_eq!(original.plan_digest(), plan().plan_digest());

        let json = original.render_json().unwrap();
        assert!(json.contains(original.plan_digest().as_str()));
        assert!(json.contains("\"cleanup_policy_generation\": \"cleanup-v1\""));
        assert!(json.contains("\"reconstruction_policy_generation\": \"reconstruction-v1\""));
    }

    #[test]
    fn sample_class_requires_the_corresponding_execution_mode_and_sandbox_heat() {
        let plan = plan();
        let error = ResidentBackendComparisonSample::new(
            &plan,
            1,
            ResidentBackendComparisonPosition::First,
            ResidentBackendSampleClass::StoppedCanary,
            1,
            plan.treatment_a().clone(),
            ResidentBackendSemanticValidation::NotApplicable,
            performance(plan.treatment_a()),
        )
        .unwrap_err();
        assert_eq!(error.code(), "backend_sample_mode_mismatch");

        for (sample_class, mode, sandbox) in [
            (
                ResidentBackendSampleClass::StoppedCanary,
                HotExecutionMode::ResidentAfterIdle,
                HotSandboxState::Resumed,
            ),
            (
                ResidentBackendSampleClass::ResidentCanary,
                HotExecutionMode::ResidentImmediate,
                HotSandboxState::ResidentHit,
            ),
        ] {
            ResidentBackendComparisonSample::new(
                &plan,
                1,
                ResidentBackendComparisonPosition::First,
                sample_class,
                2,
                plan.treatment_a().clone(),
                ResidentBackendSemanticValidation::NotApplicable,
                performance_with_mode(plan.treatment_a(), mode, sandbox),
            )
            .unwrap();
        }
    }

    #[test]
    fn sample_refuses_generation_drift_even_when_coarse_receipt_identity_matches() {
        let plan = plan();
        let changed_generation = ResidentBackendTreatmentIdentity::new(
            plan.treatment_b().candidate_id(),
            plan.treatment_b().backend_id(),
            "container-1.4.0",
            plan.treatment_b().guest_image_generation(),
            plan.treatment_b().kernel_generation(),
            plan.treatment_b().host_class(),
            plan.treatment_b().resource_profile(),
            plan.treatment_b().storage_policy_generation(),
            plan.treatment_b().network_policy_generation(),
            plan.treatment_b().cleanup_policy_generation(),
            plan.treatment_b().reconstruction_policy_generation(),
        )
        .unwrap();
        let error = ResidentBackendComparisonSample::new(
            &plan,
            5,
            ResidentBackendComparisonPosition::Second,
            ResidentBackendSampleClass::ResidentTask,
            1,
            changed_generation,
            ResidentBackendSemanticValidation::Failed,
            performance(plan.treatment_b()),
        )
        .unwrap_err();
        assert_eq!(error.code(), "backend_sample_treatment_mismatch");
    }

    #[test]
    fn sample_refuses_receipt_from_the_other_arm() {
        let plan = plan();
        let error = ResidentBackendComparisonSample::new(
            &plan,
            5,
            ResidentBackendComparisonPosition::Second,
            ResidentBackendSampleClass::ResidentTask,
            1,
            plan.treatment_b().clone(),
            ResidentBackendSemanticValidation::Failed,
            performance(plan.treatment_a()),
        )
        .unwrap_err();
        assert_eq!(error.code(), "backend_sample_identity_mismatch");
    }

    #[test]
    fn canary_and_task_validation_classes_remain_distinct() {
        let plan = plan();
        let canary_error = ResidentBackendComparisonSample::new(
            &plan,
            1,
            ResidentBackendComparisonPosition::First,
            ResidentBackendSampleClass::ResidentCanary,
            1,
            plan.treatment_a().clone(),
            ResidentBackendSemanticValidation::Passed {
                evidence_digest: validation_digest(),
            },
            performance_with_mode(
                plan.treatment_a(),
                HotExecutionMode::ResidentImmediate,
                HotSandboxState::ResidentHit,
            ),
        )
        .unwrap_err();
        assert_eq!(canary_error.code(), "backend_canary_validation_mismatch");

        let task_error = ResidentBackendComparisonSample::new(
            &plan,
            5,
            ResidentBackendComparisonPosition::First,
            ResidentBackendSampleClass::ResidentTask,
            2,
            plan.treatment_a().clone(),
            ResidentBackendSemanticValidation::NotApplicable,
            performance(plan.treatment_a()),
        )
        .unwrap_err();
        assert_eq!(task_error.code(), "backend_task_validation_missing");
    }

    #[test]
    fn invalid_private_or_free_form_identity_is_rejected() {
        let error = ResidentBackendTreatmentIdentity::new(
            "candidate",
            "backend",
            "generation",
            "/Users/operator/private/image",
            "kernel",
            "host",
            "resource",
            "storage",
            "network",
            "cleanup",
            "reconstruction",
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_backend_comparison_token");
    }

    #[test]
    fn public_sample_json_is_bounded_and_deterministic() {
        let plan = plan();
        let sample = ResidentBackendComparisonSample::new(
            &plan,
            6,
            ResidentBackendComparisonPosition::First,
            ResidentBackendSampleClass::ResidentTask,
            9,
            plan.treatment_b().clone(),
            ResidentBackendSemanticValidation::Passed {
                evidence_digest: validation_digest(),
            },
            performance(plan.treatment_b()),
        )
        .unwrap();
        let first = sample.render_json().unwrap();
        let second = sample.render_json().unwrap();
        assert_eq!(first, second);
        for forbidden in [
            "/Users/",
            "/home/",
            "HOME=",
            "GIT_ASKPASS",
            "private-secret",
            "containerId",
        ] {
            assert!(!first.contains(forbidden));
        }
        assert!(first.contains("\"authority\": \"observation_only\""));
        assert!(first.contains("\"sample_class\": \"resident_task\""));
        assert!(first.contains("\"backend_generation\": \"container-1.3.0\""));
        assert!(first.contains(plan.plan_digest().as_str()));
    }
}
