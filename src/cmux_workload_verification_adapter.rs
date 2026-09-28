//! Projection of validated CMUX repository-owned workload results into #547 observations.
//!
//! CMUX owns workload semantics and the `cmux-workload-result/v1` contract. This module consumes
//! that semantic result together with Glaeda's correlated CMUX observation and projects only
//! optimization-relevant stage facts into `VerificationObservation`.
//!
//! The projection is observation-only. It never executes CMUX commands, redefines CMUX pass/fail,
//! invents missing validity inputs, or grants reusable-state authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::adaptive_verification_compiler::{
    AdaptiveVerificationCompilerAuthority, AdaptiveVerificationCompilerError,
    MAX_VERIFICATION_DURATION_MILLIS, SemanticValidationResult, ValidityInput, ValidityInputKind,
    VerificationObservation, VerificationReuseClass, VerificationStage, VerificationTestScope,
};

pub const CMUX_WORKLOAD_VERIFICATION_ADAPTER_SCHEMA_VERSION: u8 = 1;
pub const MAX_CMUX_SEMANTIC_RESULT_BYTES: usize = 64 * 1024;
pub const MAX_GLAEDA_CMUX_OBSERVATION_BYTES: usize = 16 * 1024;

const CMUX_REPOSITORY: &str = "manaflow-ai/cmux";
const MAX_STAGE_TIMINGS: usize = 32;
const MAX_RUNTIME_INPUTS: usize = 32;
const MAX_ARTIFACTS: usize = 64;
const MAX_TOOLCHAIN_OBSERVATIONS: usize = 32;
const MAX_PARAMETERS: usize = 8;
const MAX_TEXT_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CmuxWorkloadVerificationAdapterDocumentType {
    CmuxWorkloadVerificationObservationBatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CmuxWorkloadVerificationObservationBatch {
    document_type: CmuxWorkloadVerificationAdapterDocumentType,
    schema_version: u8,
    authority: AdaptiveVerificationCompilerAuthority,
    run_id: String,
    workload: String,
    profile: String,
    source_tree: String,
    #[serde(skip_serializing)]
    source_commit: String,
    #[serde(skip_serializing)]
    semantic_parameters: BTreeMap<String, i64>,
    cmux_semantic_result_sha256: String,
    glaeda_cmux_observation_sha256: String,
    semantic_comparison_key: String,
    comparison_context_key: String,
    semantic_result: SemanticValidationResult,
    ignored_stages: Vec<String>,
    observations: Vec<VerificationObservation>,
}

impl CmuxWorkloadVerificationObservationBatch {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    #[must_use]
    pub fn source_tree(&self) -> &str {
        &self.source_tree
    }

    #[must_use]
    pub fn source_commit(&self) -> &str {
        &self.source_commit
    }

    #[must_use]
    pub fn semantic_parameter(&self, name: &str) -> Option<i64> {
        self.semantic_parameters.get(name).copied()
    }

    #[must_use]
    pub fn cmux_semantic_result_sha256(&self) -> &str {
        &self.cmux_semantic_result_sha256
    }

    #[must_use]
    pub fn observations(&self) -> &[VerificationObservation] {
        &self.observations
    }

    #[must_use]
    pub fn ignored_stages(&self) -> &[String] {
        &self.ignored_stages
    }

    /// Render the projection as deterministic pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed bounded model fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render the same bounded projection for human inspection.
    #[must_use]
    pub fn render_human(&self) -> String {
        let ignored = if self.ignored_stages.is_empty() {
            "none".to_owned()
        } else {
            self.ignored_stages.join(", ")
        };
        let mut output = format!(
            "cmux workload verification observation\nrun: {}\nprofile: {}\nsource tree: {}\nsemantic result: {:?}\nsemantic result digest: {}\ncomparison key: {}\ncontext key: {}\nobservations: {}\nignored stages: {}\n",
            self.run_id,
            self.profile,
            self.source_tree,
            self.semantic_result,
            self.cmux_semantic_result_sha256,
            self.semantic_comparison_key,
            self.comparison_context_key,
            self.observations.len(),
            ignored,
        );
        for observation in &self.observations {
            output.push_str(&format!(
                "  - {}: {} ms\n",
                observation.stage().as_str(),
                observation.duration_millis(),
            ));
        }
        output
    }
}

/// Project one already-correlated CMUX semantic result into deterministic #547 observations.
///
/// The existing Glaeda CMUX request adapter owns request/result correlation and result-integrity
/// validation. This function independently requires the supplied outer observation to bind the
/// exact semantic-result digest, source, profile, validator, and terminal state before projecting
/// stage data. Missing build validity inputs stay missing.
///
/// # Errors
///
/// Returns a bounded adapter error for oversized/malformed documents, identity drift, unsupported
/// result vocabulary, inconsistent correlation, or observation projection failure.
pub fn project_cmux_workload_result(
    run_id: &str,
    glaeda_observation_bytes: &[u8],
    cmux_result_bytes: &[u8],
) -> Result<CmuxWorkloadVerificationObservationBatch, CmuxWorkloadVerificationAdapterError> {
    validate_token(run_id)?;
    if glaeda_observation_bytes.is_empty()
        || glaeda_observation_bytes.len() > MAX_GLAEDA_CMUX_OBSERVATION_BYTES
    {
        return Err(error(
            "cmux_outer_observation_size",
            "Glaeda CMUX observation exceeds its bounded document range",
        ));
    }
    if cmux_result_bytes.is_empty() || cmux_result_bytes.len() > MAX_CMUX_SEMANTIC_RESULT_BYTES {
        return Err(error(
            "cmux_semantic_result_size",
            "CMUX semantic result exceeds its bounded document range",
        ));
    }

    let outer: RawGlaedaCmuxObservation = decode(glaeda_observation_bytes)?;
    let result: RawCmuxWorkloadResult = decode(cmux_result_bytes)?;
    validate_outer(&outer)?;
    validate_result(&result)?;

    let result_digest = sha256(cmux_result_bytes);
    if outer.cmux_semantic_result_sha256 != result_digest {
        return Err(error(
            "cmux_semantic_result_digest_mismatch",
            "CMUX semantic result digest differs from the correlated Glaeda observation",
        ));
    }
    if outer.source != result.source
        || outer.profile != result.profile
        || outer.cmux_semantic_validator != result.semantic_validator
    {
        return Err(error(
            "cmux_semantic_result_identity_mismatch",
            "CMUX source, profile, or validator differs from the correlated Glaeda observation",
        ));
    }

    let semantic_result = map_semantic_result(&result.result)?;
    if outer.state != outer_state(semantic_result) {
        return Err(error(
            "cmux_semantic_result_state_mismatch",
            "CMUX terminal state differs from the correlated Glaeda observation",
        ));
    }

    let profile = format!("{}@{}", result.profile.id, result.profile.generation);
    let reuse_class = map_reuse_class(&result.benchmark.state_class)?;
    let product_schema = product_schema_identity(&result);
    let runtime_input_identity = runtime_input_set_identity(&result.runtime_input_identities);
    let sdk_identity = result
        .toolchain
        .observations
        .get("macos_sdk")
        .map(|value| domain_digest("cmux-sdk-observation-v1", value.as_bytes()));
    let artifact_bytes = result
        .artifact_identities
        .iter()
        .fold(0_u64, |total, artifact| {
            total.saturating_add(artifact.bytes)
        });
    let runtime_input_bytes = result
        .runtime_input_identities
        .iter()
        .fold(0_u64, |total, input| total.saturating_add(input.bytes));

    let mut observations = Vec::new();
    let mut ignored_stages = BTreeSet::new();
    for (index, timing) in result.stage_timings.iter().enumerate() {
        let Some(stage) = mapped_stage(&timing.stage) else {
            ignored_stages.insert(timing.stage.clone());
            continue;
        };
        let sequence = u16::try_from(index + 1).map_err(|_| {
            error(
                "cmux_stage_sequence_out_of_range",
                "CMUX stage sequence exceeds the bounded observation range",
            )
        })?;
        let duration_millis = duration_millis(timing.seconds)?;
        let observation_id = observation_id(run_id, sequence, &timing.stage);

        let mut observation = VerificationObservation::new(
            &observation_id,
            run_id,
            "cmux",
            &profile,
            sequence,
            stage,
            duration_millis,
            reuse_class,
            semantic_result,
            &result.resource_summary.resource_class,
        )
        .map_err(compiler_projection_error)?
        .with_source_toolchain(Some(&result.source.tree), Some(&result.toolchain.identity))
        .map_err(compiler_projection_error)?;

        let mut validity = vec![
            ValidityInput::new(
                ValidityInputKind::Architecture,
                &result.resource_summary.architecture,
            )
            .map_err(compiler_projection_error)?,
            ValidityInput::new(ValidityInputKind::ProductSchema, &product_schema)
                .map_err(compiler_projection_error)?,
        ];
        if let Some(sdk) = sdk_identity.as_deref() {
            validity.push(
                ValidityInput::new(ValidityInputKind::Sdk, sdk)
                    .map_err(compiler_projection_error)?,
            );
        }
        observation = observation.with_validity_inputs(&validity);

        match stage {
            VerificationStage::Compile => {
                observation = observation
                    .with_bytes(None, Some(artifact_bytes), None)
                    .map_err(compiler_projection_error)?;
            }
            VerificationStage::TestExecution => {
                observation = observation
                    .with_bytes(Some(runtime_input_bytes), None, None)
                    .map_err(compiler_projection_error)?;
                let suite_identity = domain_digest(
                    "cmux-test-scope-v1",
                    result.benchmark.semantic_comparison_key.as_bytes(),
                );
                observation = observation
                    .with_test(VerificationTestScope::Full, &suite_identity, false)
                    .map_err(compiler_projection_error)?;
                if let Some(identity) = runtime_input_identity.as_deref() {
                    let consumer = consumer_contract_identity(&result);
                    observation = observation
                        .with_artifact(identity, Some(&consumer), Some(runtime_input_bytes))
                        .map_err(compiler_projection_error)?;
                }
            }
            VerificationStage::DependencyResolution
            | VerificationStage::CheckoutMaterialization
            | VerificationStage::Link
            | VerificationStage::ArtifactPackaging
            | VerificationStage::ArtifactTransfer
            | VerificationStage::Restore
            | VerificationStage::SetupToolInstallation
            | VerificationStage::Cleanup
            | VerificationStage::StaticGuard => {}
        }

        observations.push(observation);
    }

    Ok(CmuxWorkloadVerificationObservationBatch {
        document_type:
            CmuxWorkloadVerificationAdapterDocumentType::CmuxWorkloadVerificationObservationBatch,
        schema_version: CMUX_WORKLOAD_VERIFICATION_ADAPTER_SCHEMA_VERSION,
        authority: AdaptiveVerificationCompilerAuthority::ObservationOnly,
        run_id: run_id.to_owned(),
        workload: "cmux".to_owned(),
        profile,
        source_tree: result.source.tree,
        source_commit: result.source.commit,
        semantic_parameters: result.parameters,
        cmux_semantic_result_sha256: result_digest,
        glaeda_cmux_observation_sha256: sha256(glaeda_observation_bytes),
        semantic_comparison_key: result.benchmark.semantic_comparison_key,
        comparison_context_key: result.benchmark.comparison_context_key,
        semantic_result,
        ignored_stages: ignored_stages.into_iter().collect(),
        observations,
    })
}

fn mapped_stage(stage: &str) -> Option<VerificationStage> {
    match stage {
        "checkout_materialization" => Some(VerificationStage::CheckoutMaterialization),
        "dependency_preparation" | "dependency_resolution" => {
            Some(VerificationStage::DependencyResolution)
        }
        "compile" => Some(VerificationStage::Compile),
        "link" => Some(VerificationStage::Link),
        "test" | "test_execution" => Some(VerificationStage::TestExecution),
        "artifact_packaging" => Some(VerificationStage::ArtifactPackaging),
        "artifact_transfer" => Some(VerificationStage::ArtifactTransfer),
        "restore" => Some(VerificationStage::Restore),
        "setup" | "tool_installation" => Some(VerificationStage::SetupToolInstallation),
        "cleanup" => Some(VerificationStage::Cleanup),
        "static_guard" => Some(VerificationStage::StaticGuard),
        _ => None,
    }
}

fn map_reuse_class(
    state_class: &str,
) -> Result<VerificationReuseClass, CmuxWorkloadVerificationAdapterError> {
    match state_class {
        "cold" => Ok(VerificationReuseClass::Cold),
        "dependency-warm" | "compiler-warm" | "resident-hot" => Ok(VerificationReuseClass::Warm),
        "exact-product-reuse" => Ok(VerificationReuseClass::Reuse),
        _ => Err(error(
            "cmux_benchmark_state_unsupported",
            "CMUX benchmark state class is unsupported",
        )),
    }
}

fn map_semantic_result(
    result: &str,
) -> Result<SemanticValidationResult, CmuxWorkloadVerificationAdapterError> {
    match result {
        "passed" => Ok(SemanticValidationResult::Passed),
        "failed" => Ok(SemanticValidationResult::Failed),
        "timed_out" => Ok(SemanticValidationResult::TimedOut),
        "ambiguous" => Ok(SemanticValidationResult::Unknown),
        _ => Err(error(
            "cmux_semantic_result_unsupported",
            "CMUX semantic result is unsupported",
        )),
    }
}

const fn outer_state(result: SemanticValidationResult) -> &'static str {
    match result {
        SemanticValidationResult::Passed => "succeeded",
        SemanticValidationResult::Failed => "failed",
        SemanticValidationResult::TimedOut => "timed_out",
        SemanticValidationResult::Unknown => "ambiguous",
    }
}

fn duration_millis(seconds: f64) -> Result<u64, CmuxWorkloadVerificationAdapterError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(error(
            "cmux_stage_duration_invalid",
            "CMUX stage duration must be finite and non-negative",
        ));
    }
    let millis = (seconds * 1_000.0).round();
    if millis > MAX_VERIFICATION_DURATION_MILLIS as f64 {
        return Err(error(
            "cmux_stage_duration_out_of_range",
            "CMUX stage duration exceeds the verification observation range",
        ));
    }
    Ok(millis as u64)
}

fn product_schema_identity(result: &RawCmuxWorkloadResult) -> String {
    let material = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        result.profile.id,
        result.profile.generation,
        result.semantic_validator,
        result.expected_result_class,
        result.environment_class,
    );
    domain_digest("cmux-product-schema-v1", material.as_bytes())
}

fn consumer_contract_identity(result: &RawCmuxWorkloadResult) -> String {
    let mut material = format!("{}@{}\n", result.profile.id, result.profile.generation);
    for (name, value) in &result.parameters {
        material.push_str(&format!("{name}={value}\n"));
    }
    domain_digest("cmux-consumer-contract-v1", material.as_bytes())
}

fn runtime_input_set_identity(inputs: &[RawRuntimeInputIdentity]) -> Option<String> {
    if inputs.is_empty() {
        return None;
    }
    let mut rows = inputs
        .iter()
        .map(|input| {
            format!(
                "{}\t{}\t{}\t{}\t{}\n",
                input.name, input.class, input.identity, input.sha256, input.bytes
            )
        })
        .collect::<Vec<_>>();
    rows.sort();
    Some(domain_digest(
        "cmux-runtime-input-set-v1",
        rows.concat().as_bytes(),
    ))
}

fn observation_id(run_id: &str, sequence: u16, stage: &str) -> String {
    let material = format!("{run_id}\n{sequence}\n{stage}\n");
    format!(
        "cmux-{}",
        &domain_digest("cmux-verification-observation-v1", material.as_bytes())[7..]
    )
}

fn domain_digest(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn decode<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
) -> Result<T, CmuxWorkloadVerificationAdapterError> {
    serde_json::from_slice(bytes).map_err(|_| {
        error(
            "cmux_observation_json_invalid",
            "CMUX observation input is not valid closed-schema JSON",
        )
    })
}

fn validate_outer(
    outer: &RawGlaedaCmuxObservation,
) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if outer.document_type != "glaeda-cmux-workload-observation" || outer.schema_version != 1 {
        return Err(error(
            "cmux_outer_observation_unsupported",
            "Glaeda CMUX observation contract is unsupported",
        ));
    }
    validate_source(&outer.source)?;
    validate_profile(&outer.profile)?;
    validate_digest(&outer.request_sha256)?;
    validate_digest(&outer.execution_binding_sha256)?;
    validate_digest(&outer.cmux_semantic_result_sha256)?;
    validate_text(&outer.external_request_ref)?;
    validate_text(&outer.cmux_semantic_validator)?;
    if !matches!(
        outer.state.as_str(),
        "succeeded" | "failed" | "timed_out" | "ambiguous"
    ) {
        return Err(error(
            "cmux_outer_state_invalid",
            "Glaeda CMUX observation terminal state is invalid",
        ));
    }
    if outer.authority.authorizes_execution
        || outer.authority.authorizes_host_selection
        || outer.authority.authorizes_resource_override
        || outer.authority.authorizes_redispatch
    {
        return Err(error(
            "cmux_outer_authority_invalid",
            "Glaeda CMUX observation unexpectedly carries execution authority",
        ));
    }
    if let Some(correlation) = &outer.correlation {
        validate_text(&correlation.work_ref)?;
    }
    Ok(())
}

fn validate_result(
    result: &RawCmuxWorkloadResult,
) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if result.document_type != "cmux-workload-result" || result.schema_version != 1 {
        return Err(error(
            "cmux_semantic_result_unsupported",
            "CMUX semantic result contract is unsupported",
        ));
    }
    validate_source(&result.source)?;
    validate_profile(&result.profile)?;
    validate_text(&result.semantic_validator)?;
    validate_text(&result.environment_class)?;
    validate_text(&result.expected_result_class)?;
    validate_text(&result.network_class)?;
    validate_text(&result.timeout_class)?;
    if result.parameters.len() > MAX_PARAMETERS {
        return Err(error(
            "cmux_parameters_out_of_range",
            "CMUX semantic parameter count exceeds its bounded range",
        ));
    }
    for name in result.parameters.keys() {
        validate_token(name)?;
    }

    if result.runtime_input_identities.len() > MAX_RUNTIME_INPUTS
        || result.artifact_identities.len() > MAX_ARTIFACTS
        || result.stage_timings.is_empty()
        || result.stage_timings.len() > MAX_STAGE_TIMINGS
        || result.toolchain.observations.len() > MAX_TOOLCHAIN_OBSERVATIONS
    {
        return Err(error(
            "cmux_semantic_evidence_out_of_range",
            "CMUX semantic evidence exceeds its bounded collection range",
        ));
    }

    if result.ended_at_unix_millis < result.started_at_unix_millis {
        return Err(error(
            "cmux_result_interval_invalid",
            "CMUX result end time precedes its start time",
        ));
    }
    let elapsed_millis = result.ended_at_unix_millis - result.started_at_unix_millis;

    let mut runtime_names = BTreeSet::new();
    for input in &result.runtime_input_identities {
        validate_token(&input.name)?;
        validate_text(&input.class)?;
        if !matches!(
            input.identity.as_str(),
            "file-sha256" | "parent-tree-sha256"
        ) {
            return Err(error(
                "cmux_runtime_input_identity_invalid",
                "CMUX runtime input identity kind is unsupported",
            ));
        }
        validate_digest(&input.sha256)?;
        if !runtime_names.insert(input.name.as_str()) {
            return Err(error(
                "cmux_runtime_input_duplicate",
                "CMUX runtime input names must be unique",
            ));
        }
    }
    for artifact in &result.artifact_identities {
        validate_text(&artifact.class)?;
        if artifact.path_class != "repository_output" {
            return Err(error(
                "cmux_artifact_path_class_invalid",
                "CMUX artifact path class is unsupported",
            ));
        }
        validate_digest(&artifact.sha256)?;
    }
    for class in &result.validation.missing_required_artifact_classes {
        validate_text(class)?;
    }
    for timing in &result.stage_timings {
        validate_stage(&timing.stage)?;
        let stage_millis = duration_millis(timing.seconds)?;
        if stage_millis > elapsed_millis {
            return Err(error(
                "cmux_stage_duration_outside_result_interval",
                "CMUX stage duration exceeds the observed result interval",
            ));
        }
    }
    validate_text(&result.resource_summary.resource_class)?;
    validate_token(&result.resource_summary.architecture)?;
    if result.resource_summary.cpu_count == 0 {
        return Err(error(
            "cmux_resource_profile_invalid",
            "CMUX resource profile CPU count must be positive",
        ));
    }
    validate_digest(&result.toolchain.identity)?;
    for (name, value) in &result.toolchain.observations {
        validate_token(name)?;
        validate_text(value)?;
    }
    validate_digest(&result.benchmark.semantic_comparison_key)?;
    validate_digest(&result.benchmark.comparison_context_key)?;
    map_reuse_class(&result.benchmark.state_class)?;
    map_semantic_result(&result.result)?;

    if !matches!(
        result.cleanup.state.as_str(),
        "complete" | "forced" | "incomplete"
    ) {
        return Err(error(
            "cmux_cleanup_state_invalid",
            "CMUX cleanup state is invalid",
        ));
    }
    match result.result.as_str() {
        "passed" => {
            if result.exit_code != 0
                || !result
                    .validation
                    .missing_required_artifact_classes
                    .is_empty()
                || result.cleanup.state != "complete"
                || !result.cleanup.process_group_settled
            {
                return Err(error(
                    "cmux_passed_result_inconsistent",
                    "passed CMUX result lacks successful validation and cleanup evidence",
                ));
            }
        }
        "ambiguous" => {
            if result.cleanup.process_group_settled {
                return Err(error(
                    "cmux_ambiguous_result_inconsistent",
                    "ambiguous CMUX result unexpectedly reports settled processes",
                ));
            }
        }
        "failed" | "timed_out" => {}
        _ => unreachable!(),
    }

    Ok(())
}

fn validate_source(source: &RawSource) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if source.repository != CMUX_REPOSITORY || !is_oid(&source.commit) || !is_oid(&source.tree) {
        return Err(error(
            "cmux_source_identity_invalid",
            "CMUX source identity is invalid",
        ));
    }
    Ok(())
}

fn validate_profile(profile: &RawProfile) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if profile.generation == 0
        || !profile.id.starts_with("cmux.")
        || profile.id.len() > 96
        || !profile.id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
    {
        return Err(error(
            "cmux_profile_identity_invalid",
            "CMUX profile identity is invalid",
        ));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(error(
            "cmux_digest_invalid",
            "CMUX digest identity is invalid",
        ));
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.contains('\0') {
        return Err(error(
            "cmux_text_out_of_range",
            "CMUX textual evidence exceeds its bounded range",
        ));
    }
    Ok(())
}

fn validate_token(value: &str) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@' | b'+')
        })
    {
        return Err(error(
            "cmux_token_invalid",
            "CMUX token evidence is invalid",
        ));
    }
    Ok(())
}

fn validate_stage(value: &str) -> Result<(), CmuxWorkloadVerificationAdapterError> {
    if value.is_empty()
        || value.len() > 48
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(error(
            "cmux_stage_invalid",
            "CMUX stage identity is invalid",
        ));
    }
    Ok(())
}

fn is_oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn compiler_projection_error(
    _error: AdaptiveVerificationCompilerError,
) -> CmuxWorkloadVerificationAdapterError {
    error(
        "cmux_projection_invalid",
        "CMUX semantic evidence cannot be represented by the bounded verification observation model",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CmuxWorkloadVerificationAdapterError {
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for CmuxWorkloadVerificationAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.problem)
    }
}

impl std::error::Error for CmuxWorkloadVerificationAdapterError {}

const fn error(code: &'static str, problem: &'static str) -> CmuxWorkloadVerificationAdapterError {
    CmuxWorkloadVerificationAdapterError { code, problem }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    repository: String,
    commit: String,
    tree: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    id: String,
    generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCorrelation {
    work_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawOuterAuthority {
    authorizes_execution: bool,
    authorizes_host_selection: bool,
    authorizes_resource_override: bool,
    authorizes_redispatch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawGlaedaCmuxObservation {
    document_type: String,
    schema_version: u8,
    external_request_ref: String,
    request_sha256: String,
    execution_binding_sha256: String,
    source: RawSource,
    profile: RawProfile,
    state: String,
    cmux_semantic_result_sha256: String,
    cmux_semantic_validator: String,
    authority: RawOuterAuthority,
    #[serde(default)]
    correlation: Option<RawCorrelation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimeInputIdentity {
    name: String,
    class: String,
    identity: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawArtifactIdentity {
    class: String,
    path_class: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawValidation {
    missing_required_artifact_classes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawStageTiming {
    stage: String,
    seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawResourceSummary {
    resource_class: String,
    cpu_count: u32,
    memory_bytes: Option<u64>,
    architecture: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawToolchain {
    identity: String,
    observations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawBenchmark {
    state_class: String,
    semantic_comparison_key: String,
    comparison_context_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCleanup {
    state: String,
    process_group_settled: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCmuxWorkloadResult {
    document_type: String,
    schema_version: u8,
    source: RawSource,
    profile: RawProfile,
    semantic_validator: String,
    environment_class: String,
    expected_result_class: String,
    result: String,
    parameters: BTreeMap<String, i64>,
    runtime_input_identities: Vec<RawRuntimeInputIdentity>,
    artifact_identities: Vec<RawArtifactIdentity>,
    validation: RawValidation,
    stage_timings: Vec<RawStageTiming>,
    resource_summary: RawResourceSummary,
    toolchain: RawToolchain,
    benchmark: RawBenchmark,
    network_class: String,
    timeout_class: String,
    cleanup: RawCleanup,
    exit_code: i32,
    started_at_unix_millis: u64,
    ended_at_unix_millis: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive_verification_compiler::{
        OptimizationClass, OptimizationLifecycle, ValidityFingerprintStatus,
        compile_verification_optimizations,
    };

    fn digest(seed: char) -> String {
        format!("sha256:{}", seed.to_string().repeat(64))
    }

    fn source() -> RawSource {
        RawSource {
            repository: CMUX_REPOSITORY.to_owned(),
            commit: "1".repeat(40),
            tree: "2".repeat(40),
        }
    }

    fn toolchain() -> RawToolchain {
        let mut observations = BTreeMap::new();
        observations.insert("xcode".to_owned(), "Xcode 26.6".to_owned());
        observations.insert("macos_sdk".to_owned(), "26.6".to_owned());
        RawToolchain {
            identity: digest('3'),
            observations,
        }
    }

    fn compile_result(run_result: &str, state_class: &str) -> RawCmuxWorkloadResult {
        RawCmuxWorkloadResult {
            document_type: "cmux-workload-result".to_owned(),
            schema_version: 1,
            source: source(),
            profile: RawProfile {
                id: "cmux.macos.compile-admission".to_owned(),
                generation: 1,
            },
            semantic_validator: "cmux.compile-admission/v1".to_owned(),
            environment_class: "isolated-build".to_owned(),
            expected_result_class: "cmux.app-host-test-product/v1".to_owned(),
            result: run_result.to_owned(),
            parameters: BTreeMap::new(),
            runtime_input_identities: Vec::new(),
            artifact_identities: vec![RawArtifactIdentity {
                class: "cmux.app-host-xctestrun/v1".to_owned(),
                path_class: "repository_output".to_owned(),
                sha256: digest('4'),
                bytes: 850_000_000,
            }],
            validation: RawValidation {
                missing_required_artifact_classes: Vec::new(),
            },
            stage_timings: vec![
                RawStageTiming {
                    stage: "setup".to_owned(),
                    seconds: 42.0,
                },
                RawStageTiming {
                    stage: "dependency_preparation".to_owned(),
                    seconds: 56.0,
                },
                RawStageTiming {
                    stage: "compile".to_owned(),
                    seconds: 343.6,
                },
                RawStageTiming {
                    stage: "validation".to_owned(),
                    seconds: 0.2,
                },
            ],
            resource_summary: RawResourceSummary {
                resource_class: "cmux-macos-compile-large".to_owned(),
                cpu_count: 6,
                memory_bytes: Some(32_u64 << 30),
                architecture: "arm64".to_owned(),
            },
            toolchain: toolchain(),
            benchmark: RawBenchmark {
                state_class: state_class.to_owned(),
                semantic_comparison_key: digest('5'),
                comparison_context_key: digest('6'),
            },
            network_class: "dependency-fetch".to_owned(),
            timeout_class: "macos-long".to_owned(),
            cleanup: RawCleanup {
                state: "complete".to_owned(),
                process_group_settled: true,
            },
            exit_code: if run_result == "passed" { 0 } else { 1 },
            started_at_unix_millis: 1_000,
            ended_at_unix_millis: 500_000,
        }
    }

    fn shard_result() -> RawCmuxWorkloadResult {
        let mut parameters = BTreeMap::new();
        parameters.insert("shard".to_owned(), 3);
        RawCmuxWorkloadResult {
            document_type: "cmux-workload-result".to_owned(),
            schema_version: 1,
            source: source(),
            profile: RawProfile {
                id: "cmux.macos.app-host-test-shard".to_owned(),
                generation: 1,
            },
            semantic_validator: "cmux.app-host-test-shard/v1".to_owned(),
            environment_class: "isolated-console-test".to_owned(),
            expected_result_class: "cmux.app-host-test-shard-result/v1".to_owned(),
            result: "passed".to_owned(),
            parameters,
            runtime_input_identities: vec![RawRuntimeInputIdentity {
                name: "app_host_xctestrun".to_owned(),
                class: "cmux.app-host-product-tree/v1".to_owned(),
                identity: "parent-tree-sha256".to_owned(),
                sha256: digest('7'),
                bytes: 850_000_000,
            }],
            artifact_identities: Vec::new(),
            validation: RawValidation {
                missing_required_artifact_classes: Vec::new(),
            },
            stage_timings: vec![
                RawStageTiming {
                    stage: "setup".to_owned(),
                    seconds: 12.0,
                },
                RawStageTiming {
                    stage: "test".to_owned(),
                    seconds: 180.0,
                },
            ],
            resource_summary: RawResourceSummary {
                resource_class: "cmux-macos-app-host-test".to_owned(),
                cpu_count: 6,
                memory_bytes: Some(32_u64 << 30),
                architecture: "arm64".to_owned(),
            },
            toolchain: toolchain(),
            benchmark: RawBenchmark {
                state_class: "exact-product-reuse".to_owned(),
                semantic_comparison_key: digest('8'),
                comparison_context_key: digest('9'),
            },
            network_class: "repository-test".to_owned(),
            timeout_class: "macos-long".to_owned(),
            cleanup: RawCleanup {
                state: "complete".to_owned(),
                process_group_settled: true,
            },
            exit_code: 0,
            started_at_unix_millis: 1_000,
            ended_at_unix_millis: 193_000,
        }
    }

    fn outer_for(result_bytes: &[u8], result: &RawCmuxWorkloadResult) -> RawGlaedaCmuxObservation {
        RawGlaedaCmuxObservation {
            document_type: "glaeda-cmux-workload-observation".to_owned(),
            schema_version: 1,
            external_request_ref: "cmux-workload-fixture".to_owned(),
            request_sha256: digest('a'),
            execution_binding_sha256: digest('b'),
            source: result.source.clone(),
            profile: result.profile.clone(),
            state: match result.result.as_str() {
                "passed" => "succeeded",
                "failed" => "failed",
                "timed_out" => "timed_out",
                _ => "ambiguous",
            }
            .to_owned(),
            cmux_semantic_result_sha256: sha256(result_bytes),
            cmux_semantic_validator: result.semantic_validator.clone(),
            authority: RawOuterAuthority {
                authorizes_execution: false,
                authorizes_host_selection: false,
                authorizes_resource_override: false,
                authorizes_redispatch: false,
            },
            correlation: Some(RawCorrelation {
                work_ref: "cmux-work-fixture".to_owned(),
            }),
        }
    }

    fn project(
        result: &RawCmuxWorkloadResult,
        run_id: &str,
    ) -> CmuxWorkloadVerificationObservationBatch {
        let result_bytes = serde_json::to_vec(result).unwrap();
        let outer = outer_for(&result_bytes, result);
        let outer_bytes = serde_json::to_vec(&outer).unwrap();
        project_cmux_workload_result(run_id, &outer_bytes, &result_bytes).unwrap()
    }

    #[test]
    fn compile_profile_projects_named_stages_and_keeps_unknown_validation_stage_visible() {
        let batch = project(&compile_result("passed", "cold"), "cmux-run-1");
        let stages = batch
            .observations()
            .iter()
            .map(VerificationObservation::stage)
            .collect::<Vec<_>>();

        assert_eq!(
            stages,
            vec![
                VerificationStage::SetupToolInstallation,
                VerificationStage::DependencyResolution,
                VerificationStage::Compile,
            ]
        );
        assert_eq!(batch.ignored_stages(), &["validation".to_owned()]);
        assert_eq!(batch.observations()[2].duration_millis(), 343_600);
    }

    #[test]
    fn result_interval_bounds_projected_stage_durations() {
        let mut reversed = compile_result("passed", "cold");
        reversed.ended_at_unix_millis = reversed.started_at_unix_millis - 1;
        assert_eq!(
            validate_result(&reversed).unwrap_err().code,
            "cmux_result_interval_invalid"
        );

        let mut too_short = compile_result("passed", "cold");
        too_short.ended_at_unix_millis = too_short.started_at_unix_millis + 1_000;
        assert_eq!(
            validate_result(&too_short).unwrap_err().code,
            "cmux_stage_duration_outside_result_interval"
        );
    }

    #[test]
    fn repeated_stage_names_get_distinct_observation_ids() {
        let mut result = compile_result("passed", "cold");
        result.stage_timings.insert(
            3,
            RawStageTiming {
                stage: "compile".to_owned(),
                seconds: 2.5,
            },
        );
        let bytes = serde_json::to_vec(&result).unwrap();
        let outer = outer_for(&bytes, &result);
        let projected = project_cmux_workload_result(
            "cmux-run-duplicate-stage",
            &serde_json::to_vec(&outer).unwrap(),
            &bytes,
        )
        .unwrap();

        let compile_ids = projected
            .observations()
            .iter()
            .filter(|observation| observation.stage() == VerificationStage::Compile)
            .map(VerificationObservation::observation_id)
            .collect::<Vec<_>>();

        assert_eq!(compile_ids.len(), 2);
        assert_ne!(compile_ids[0], compile_ids[1]);
        assert!(compile_ids.iter().all(|identity| identity.len() == 69));
    }

    #[test]
    fn repeated_compile_receipts_discover_reuse_but_missing_config_and_flags_keep_it_advisory() {
        let mut observations = Vec::new();
        for index in 0..3 {
            let batch = project(
                &compile_result("passed", "cold"),
                &format!("cmux-compile-run-{index}"),
            );
            observations.extend_from_slice(batch.observations());
        }

        let receipt = compile_verification_optimizations(
            "cmux",
            "cmux.macos.compile-admission@1",
            &observations,
            &[],
        )
        .unwrap();
        let candidate = receipt
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::ReuseExactCompiledProduct)
            .unwrap();

        assert_eq!(candidate.lifecycle(), OptimizationLifecycle::Observed);
        assert_eq!(
            candidate.validity().status(),
            ValidityFingerprintStatus::Advisory
        );
        assert!(
            candidate
                .validity()
                .missing_inputs()
                .contains(&ValidityInputKind::BuildConfiguration)
        );
        assert!(
            candidate
                .validity()
                .missing_inputs()
                .contains(&ValidityInputKind::CompilerFlags)
        );
    }

    #[test]
    fn exact_product_test_shard_projects_reuse_without_claiming_a_rebuild() {
        let batch = project(&shard_result(), "cmux-shard-run");
        let json: serde_json::Value = serde_json::from_str(&batch.render_json().unwrap()).unwrap();
        let test = json["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|observation| observation["stage"] == "test_execution")
            .unwrap();

        assert_eq!(test["reuse_class"], "reuse");
        assert_eq!(test["bytes_read"], 850_000_000_u64);
        assert_eq!(test["rebuilt_before_test"], false);
        assert!(
            test["artifact_identity"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn digest_or_identity_drift_is_rejected() {
        let result = compile_result("passed", "cold");
        let result_bytes = serde_json::to_vec(&result).unwrap();
        let mut outer = outer_for(&result_bytes, &result);
        outer.cmux_semantic_result_sha256 = digest('f');
        let err = project_cmux_workload_result(
            "cmux-run",
            &serde_json::to_vec(&outer).unwrap(),
            &result_bytes,
        )
        .unwrap_err();
        assert_eq!(err.code, "cmux_semantic_result_digest_mismatch");

        let mut outer = outer_for(&result_bytes, &result);
        outer.profile.id = "cmux.ci.guard".to_owned();
        let err = project_cmux_workload_result(
            "cmux-run",
            &serde_json::to_vec(&outer).unwrap(),
            &result_bytes,
        )
        .unwrap_err();
        assert_eq!(err.code, "cmux_semantic_result_identity_mismatch");
    }

    #[test]
    fn passed_result_requires_complete_validation_and_cleanup() {
        let mut result = compile_result("passed", "cold");
        result
            .validation
            .missing_required_artifact_classes
            .push("cmux.debug-cli/v1".to_owned());
        let result_bytes = serde_json::to_vec(&result).unwrap();
        let outer = outer_for(&result_bytes, &result);
        let err = project_cmux_workload_result(
            "cmux-run",
            &serde_json::to_vec(&outer).unwrap(),
            &result_bytes,
        )
        .unwrap_err();
        assert_eq!(err.code, "cmux_passed_result_inconsistent");
    }

    #[test]
    fn human_and_json_render_from_the_same_projection() {
        let batch = project(&compile_result("passed", "compiler-warm"), "cmux-run");
        let human = batch.render_human();
        let json = batch.render_json().unwrap();

        assert!(human.contains("cmux.macos.compile-admission@1"));
        assert!(human.contains("compile: 343600 ms"));
        assert!(human.contains("ignored stages: validation"));
        assert!(json.contains("\"authority\": \"observation_only\""));
        assert!(json.contains("\"dependency_resolution\""));
    }
}
