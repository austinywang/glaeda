//! Deterministic inner-loop verification optimization discovery.
//!
//! The reducer in this module consumes bounded, secret-free stage observations and emits only
//! candidates from a closed optimization vocabulary. It carries observation-only evidence: it
//! grants no verification, cache-publication, workflow-mutation, or execution authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::reusable_state_lifecycle::ReusableStateClass;

pub const ADAPTIVE_VERIFICATION_COMPILER_SCHEMA_VERSION: u8 = 1;
pub const MAX_VERIFICATION_OBSERVATIONS: usize = 512;
pub const MAX_OPTIMIZATION_EXPERIMENTS: usize = 64;
pub const MAX_VERIFICATION_DURATION_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
pub const MAX_VERIFICATION_BYTES: u64 = 1 << 50;

const MAX_LABEL_BYTES: usize = 128;
const MAX_OBSERVATION_SAMPLE_COUNT: u16 = 1_000;
const MIN_REPETITIONS: usize = 3;
const MIN_COMPILE_MILLIS: u64 = 10_000;
const MIN_DEPENDENCY_MILLIS: u64 = 2_000;
const MIN_TOOL_SETUP_MILLIS: u64 = 2_000;
const MIN_LARGE_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MIN_SERIAL_GUARD_MILLIS: u64 = 30_000;
const MIN_IRRELEVANT_LANE_MILLIS: u64 = 5_000;
const FOCUSED_COMPILE_RATIO: u64 = 5;
const MAX_SPLIT_REQUIRED_BASIS_POINTS: u64 = 8_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AdaptiveVerificationCompilerDocumentType {
    AdaptiveVerificationCompilerReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveVerificationCompilerAuthority {
    ObservationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStage {
    CheckoutMaterialization,
    DependencyResolution,
    Compile,
    Link,
    TestExecution,
    ArtifactPackaging,
    ArtifactTransfer,
    Restore,
    SetupToolInstallation,
    Cleanup,
    StaticGuard,
}

impl VerificationStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CheckoutMaterialization => "checkout_materialization",
            Self::DependencyResolution => "dependency_resolution",
            Self::Compile => "compile",
            Self::Link => "link",
            Self::TestExecution => "test_execution",
            Self::ArtifactPackaging => "artifact_packaging",
            Self::ArtifactTransfer => "artifact_transfer",
            Self::Restore => "restore",
            Self::SetupToolInstallation => "setup_tool_installation",
            Self::Cleanup => "cleanup",
            Self::StaticGuard => "static_guard",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationReuseClass {
    Cold,
    Warm,
    Reuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticValidationResult {
    Passed,
    Failed,
    TimedOut,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationTestScope {
    Focused,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationClass {
    ReuseExactCompiledProduct,
    RetainLocalImmutableArtifact,
    ReuseDependencyGeneration,
    BakePreparedTool,
    SplitConsumerArtifact,
    RunTestWithoutRebuild,
    MoveStaticGuardEarlier,
    ParallelizeIndependentChecks,
    IsolateFlakyOrHangingSuite,
    SkipIrrelevantPlatformLane,
}

impl OptimizationClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReuseExactCompiledProduct => "reuse_exact_compiled_product",
            Self::RetainLocalImmutableArtifact => "retain_local_immutable_artifact",
            Self::ReuseDependencyGeneration => "reuse_dependency_generation",
            Self::BakePreparedTool => "bake_prepared_tool",
            Self::SplitConsumerArtifact => "split_consumer_artifact",
            Self::RunTestWithoutRebuild => "run_test_without_rebuild",
            Self::MoveStaticGuardEarlier => "move_static_guard_earlier",
            Self::ParallelizeIndependentChecks => "parallelize_independent_checks",
            Self::IsolateFlakyOrHangingSuite => "isolate_flaky_or_hanging_suite",
            Self::SkipIrrelevantPlatformLane => "skip_irrelevant_platform_lane",
        }
    }

    const fn requires_exact_validity(self) -> bool {
        matches!(
            self,
            Self::ReuseExactCompiledProduct
                | Self::RetainLocalImmutableArtifact
                | Self::ReuseDependencyGeneration
                | Self::BakePreparedTool
                | Self::SplitConsumerArtifact
                | Self::RunTestWithoutRebuild
        )
    }

    /// Map reusable-work recommendations into the shared #21 reusable-state family vocabulary.
    ///
    /// This mapping grants no publication or consumption authority. A physical generation still
    /// starts in `reusable_state_lifecycle` under a reviewed publisher and its full identity
    /// contract.
    #[must_use]
    pub const fn reusable_state_class(self) -> Option<ReusableStateClass> {
        match self {
            Self::ReuseExactCompiledProduct
            | Self::RetainLocalImmutableArtifact
            | Self::SplitConsumerArtifact
            | Self::RunTestWithoutRebuild => Some(ReusableStateClass::ImmutableCompiledProduct),
            Self::ReuseDependencyGeneration | Self::BakePreparedTool => {
                Some(ReusableStateClass::PreparedDependencyGeneration)
            }
            Self::MoveStaticGuardEarlier
            | Self::ParallelizeIndependentChecks
            | Self::IsolateFlakyOrHangingSuite
            | Self::SkipIrrelevantPlatformLane => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationLifecycle {
    Observed,
    Candidate,
    Experimenting,
    Accepted,
    Preferred,
    Demoted,
    Retired,
}

impl OptimizationLifecycle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Candidate => "candidate",
            Self::Experimenting => "experimenting",
            Self::Accepted => "accepted",
            Self::Preferred => "preferred",
            Self::Demoted => "demoted",
            Self::Retired => "retired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationConfidence {
    Advisory,
    ExperimentRequired,
    PartialControlledEvidence,
    AcceptedEvidence,
    PreferredEvidence,
    RejectedEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationNextAction {
    StateExactValidityInputs,
    RunControlledAb,
    GatherCompatibleTrials,
    HandOffToReusableStateLifecycle,
    EmitRepositoryRecommendation,
    InvestigateRegression,
    None,
}

impl OptimizationNextAction {
    const fn human(self) -> &'static str {
        match self {
            Self::StateExactValidityInputs => "state the missing exact validity inputs",
            Self::RunControlledAb => "run a controlled A/B comparison",
            Self::GatherCompatibleTrials => "gather additional compatible controlled trials",
            Self::HandOffToReusableStateLifecycle => {
                "hand the exact candidate to the shared reusable-state lifecycle"
            }
            Self::EmitRepositoryRecommendation => {
                "emit the repository-side recommendation; keep workflow mutation manual"
            }
            Self::InvestigateRegression => "fall back and investigate the measured regression",
            Self::None => "no further action",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidityInputKind {
    SourceTree,
    Toolchain,
    Sdk,
    Architecture,
    Lockfile,
    BuildConfiguration,
    CompilerFlags,
    ProductSchema,
    ArtifactIdentity,
    ToolIdentity,
    ConsumerContract,
}

impl ValidityInputKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceTree => "source_tree",
            Self::Toolchain => "toolchain",
            Self::Sdk => "sdk",
            Self::Architecture => "architecture",
            Self::Lockfile => "lockfile",
            Self::BuildConfiguration => "build_configuration",
            Self::CompilerFlags => "compiler_flags",
            Self::ProductSchema => "product_schema",
            Self::ArtifactIdentity => "artifact_identity",
            Self::ToolIdentity => "tool_identity",
            Self::ConsumerContract => "consumer_contract",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ValidityInput {
    kind: ValidityInputKind,
    identity: String,
}

impl ValidityInput {
    /// Construct one compact exact input identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for an empty, oversized, or unsupported identity token.
    pub fn new(
        kind: ValidityInputKind,
        identity: &str,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(identity)?;
        Ok(Self {
            kind,
            identity: identity.to_owned(),
        })
    }

    #[must_use]
    pub const fn kind(&self) -> ValidityInputKind {
        self.kind
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidityFingerprintStatus {
    Exact,
    Advisory,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidityFingerprint {
    status: ValidityFingerprintStatus,
    inputs: Vec<ValidityInput>,
    missing_inputs: Vec<ValidityInputKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fingerprint: Option<String>,
}

impl ValidityFingerprint {
    #[must_use]
    pub const fn status(&self) -> ValidityFingerprintStatus {
        self.status
    }

    #[must_use]
    pub fn inputs(&self) -> &[ValidityInput] {
        &self.inputs
    }

    #[must_use]
    pub fn missing_inputs(&self) -> &[ValidityInputKind] {
        &self.missing_inputs
    }

    #[must_use]
    pub fn fingerprint(&self) -> Option<&str> {
        self.fingerprint.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationObservation {
    observation_id: String,
    run_id: String,
    workload: String,
    profile: String,
    sequence: u16,
    sample_count: u16,
    stage: VerificationStage,
    duration_millis: u64,
    reuse_class: VerificationReuseClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_read: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_written: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_transferred: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_transfer_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toolchain_identity: Option<String>,
    semantic_validation: SemanticValidationResult,
    resource_profile: String,
    validity_inputs: Vec<ValidityInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suite_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    consumer_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_consumer_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_scope: Option<VerificationTestScope>,
    rebuilt_before_test: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    independence_group: Option<String>,
    independent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform_lane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform_relevant: Option<bool>,
}

impl VerificationObservation {
    /// Construct one bounded stage observation.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for invalid identities or out-of-range duration.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        observation_id: &str,
        run_id: &str,
        workload: &str,
        profile: &str,
        sequence: u16,
        stage: VerificationStage,
        duration_millis: u64,
        reuse_class: VerificationReuseClass,
        semantic_validation: SemanticValidationResult,
        resource_profile: &str,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        for label in [observation_id, run_id, workload, profile, resource_profile] {
            validate_label(label)?;
        }
        validate_duration(duration_millis)?;
        Ok(Self {
            observation_id: observation_id.to_owned(),
            run_id: run_id.to_owned(),
            workload: workload.to_owned(),
            profile: profile.to_owned(),
            sequence,
            sample_count: 1,
            stage,
            duration_millis,
            reuse_class,
            bytes_read: None,
            bytes_written: None,
            bytes_transferred: None,
            artifact_transfer_backend: None,
            source_identity: None,
            toolchain_identity: None,
            semantic_validation,
            resource_profile: resource_profile.to_owned(),
            validity_inputs: Vec::new(),
            artifact_identity: None,
            tool_identity: None,
            suite_identity: None,
            consumer_identity: None,
            required_consumer_bytes: None,
            test_scope: None,
            rebuilt_before_test: false,
            independence_group: None,
            independent: false,
            platform_lane: None,
            platform_relevant: None,
        })
    }

    /// Mark this record as a bounded aggregate over comparable samples.
    ///
    /// A count larger than one means `duration_millis` and byte fields are representative
    /// summaries supplied by the observation owner; it never manufactures per-run samples.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for zero or excessive sample count.
    pub fn with_sample_count(
        mut self,
        sample_count: u16,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        if sample_count == 0 || sample_count > MAX_OBSERVATION_SAMPLE_COUNT {
            return Err(error(
                "verification_sample_count_out_of_range",
                "verification sample count exceeds the bounded observation range",
            ));
        }
        self.sample_count = sample_count;
        Ok(self)
    }

    /// Attach bounded byte counters.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error when a byte count exceeds the receipt range.
    pub fn with_bytes(
        mut self,
        bytes_read: Option<u64>,
        bytes_written: Option<u64>,
        bytes_transferred: Option<u64>,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        for value in [bytes_read, bytes_written, bytes_transferred]
            .into_iter()
            .flatten()
        {
            validate_bytes(value)?;
        }
        self.bytes_read = bytes_read;
        self.bytes_written = bytes_written;
        self.bytes_transferred = bytes_transferred;
        Ok(self)
    }

    /// Attach the observed artifact-transfer path, such as a local broker or remote store.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error when the transfer identity is invalid.
    pub fn with_artifact_transfer_backend(
        mut self,
        backend: &str,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(backend)?;
        self.artifact_transfer_backend = Some(backend.to_owned());
        Ok(self)
    }

    /// Attach exact source and toolchain identity and include them in the reusable validity set.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error when an identity is invalid.
    pub fn with_source_toolchain(
        mut self,
        source_identity: Option<&str>,
        toolchain_identity: Option<&str>,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        if let Some(identity) = source_identity {
            validate_label(identity)?;
            self.source_identity = Some(identity.to_owned());
            self.push_validity(ValidityInput::new(ValidityInputKind::SourceTree, identity)?);
        }
        if let Some(identity) = toolchain_identity {
            validate_label(identity)?;
            self.toolchain_identity = Some(identity.to_owned());
            self.push_validity(ValidityInput::new(ValidityInputKind::Toolchain, identity)?);
        }
        Ok(self)
    }

    /// Attach additional exact validity inputs.
    pub fn with_validity_inputs(mut self, inputs: &[ValidityInput]) -> Self {
        for input in inputs {
            self.push_validity(input.clone());
        }
        self
    }

    /// Attach immutable artifact/consumer evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for invalid identities or byte counts.
    pub fn with_artifact(
        mut self,
        artifact_identity: &str,
        consumer_identity: Option<&str>,
        required_consumer_bytes: Option<u64>,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(artifact_identity)?;
        self.artifact_identity = Some(artifact_identity.to_owned());
        self.push_validity(ValidityInput::new(
            ValidityInputKind::ArtifactIdentity,
            artifact_identity,
        )?);
        if let Some(identity) = consumer_identity {
            validate_label(identity)?;
            self.consumer_identity = Some(identity.to_owned());
            self.push_validity(ValidityInput::new(
                ValidityInputKind::ConsumerContract,
                identity,
            )?);
        }
        if let Some(bytes) = required_consumer_bytes {
            validate_bytes(bytes)?;
            self.required_consumer_bytes = Some(bytes);
        }
        Ok(self)
    }

    /// Attach reviewed static-tool identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for invalid tool identity.
    pub fn with_tool(
        mut self,
        tool_identity: &str,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(tool_identity)?;
        self.tool_identity = Some(tool_identity.to_owned());
        self.push_validity(ValidityInput::new(
            ValidityInputKind::ToolIdentity,
            tool_identity,
        )?);
        Ok(self)
    }

    /// Attach test-scope evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for an invalid suite identity.
    pub fn with_test(
        mut self,
        scope: VerificationTestScope,
        suite_identity: &str,
        rebuilt_before_test: bool,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(suite_identity)?;
        self.test_scope = Some(scope);
        self.suite_identity = Some(suite_identity.to_owned());
        self.rebuilt_before_test = rebuilt_before_test;
        Ok(self)
    }

    /// Mark one guard as independently runnable with its peers.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for an invalid independence group.
    pub fn with_independence(
        mut self,
        group: &str,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(group)?;
        self.independence_group = Some(group.to_owned());
        self.independent = true;
        Ok(self)
    }

    /// Attach platform-lane relevance evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for an invalid platform-lane identity.
    pub fn with_platform(
        mut self,
        lane: &str,
        relevant: bool,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(lane)?;
        self.platform_lane = Some(lane.to_owned());
        self.platform_relevant = Some(relevant);
        Ok(self)
    }

    fn push_validity(&mut self, input: ValidityInput) {
        if !self.validity_inputs.contains(&input) {
            self.validity_inputs.push(input);
            self.validity_inputs.sort();
        }
    }

    #[must_use]
    pub fn observation_id(&self) -> &str {
        &self.observation_id
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub const fn stage(&self) -> VerificationStage {
        self.stage
    }

    #[must_use]
    pub const fn duration_millis(&self) -> u64 {
        self.duration_millis
    }

    #[must_use]
    pub fn validity_inputs(&self) -> &[ValidityInput] {
        &self.validity_inputs
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptimizationExperiment {
    experiment_id: String,
    workload: String,
    profile: String,
    class: OptimizationClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bound_candidate_id: Option<String>,
    baseline_elapsed_millis: u64,
    candidate_elapsed_millis: u64,
    restore_transfer_overhead_millis: u64,
    publication_overhead_millis: u64,
    invalidation_reset_cost_millis: u64,
    storage_bytes: u64,
    semantic_result_match: bool,
    candidate_succeeded: bool,
    reset_observed: bool,
    noise_millis: u64,
    controlled: bool,
    evidence_ref: String,
}

impl OptimizationExperiment {
    /// Construct one bounded comparison result.
    ///
    /// Extra overhead fields contain costs outside `candidate_elapsed_millis`. When the candidate
    /// arm already includes restore/transfer/publication, callers leave the corresponding field at
    /// zero.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for invalid identities or out-of-range observations.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        experiment_id: &str,
        workload: &str,
        profile: &str,
        class: OptimizationClass,
        subject_identity: Option<&str>,
        baseline_elapsed_millis: u64,
        candidate_elapsed_millis: u64,
        restore_transfer_overhead_millis: u64,
        publication_overhead_millis: u64,
        invalidation_reset_cost_millis: u64,
        storage_bytes: u64,
        semantic_result_match: bool,
        candidate_succeeded: bool,
        reset_observed: bool,
        noise_millis: u64,
        controlled: bool,
        evidence_ref: &str,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        for label in [experiment_id, workload, profile, evidence_ref] {
            validate_label(label)?;
        }
        if let Some(subject) = subject_identity {
            validate_label(subject)?;
        }
        for duration in [
            baseline_elapsed_millis,
            candidate_elapsed_millis,
            restore_transfer_overhead_millis,
            publication_overhead_millis,
            invalidation_reset_cost_millis,
            noise_millis,
        ] {
            validate_duration(duration)?;
        }
        validate_bytes(storage_bytes)?;
        Ok(Self {
            experiment_id: experiment_id.to_owned(),
            workload: workload.to_owned(),
            profile: profile.to_owned(),
            class,
            subject_identity: subject_identity.map(str::to_owned),
            bound_candidate_id: None,
            baseline_elapsed_millis,
            candidate_elapsed_millis,
            restore_transfer_overhead_millis,
            publication_overhead_millis,
            invalidation_reset_cost_millis,
            storage_bytes,
            semantic_result_match,
            candidate_succeeded,
            reset_observed,
            noise_millis,
            controlled,
            evidence_ref: evidence_ref.to_owned(),
        })
    }

    /// Bind this trial to one exact deterministic candidate identity.
    ///
    /// Unbound trials remain retained receipt evidence but carry zero lifecycle-promotion authority.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for an invalid candidate identity.
    pub fn bind_candidate(
        mut self,
        candidate_id: &str,
    ) -> Result<Self, AdaptiveVerificationCompilerError> {
        validate_label(candidate_id)?;
        if !candidate_id.starts_with("avc-") {
            return Err(error(
                "verification_candidate_binding_invalid",
                "experiment candidate binding must use an adaptive compiler candidate identity",
            ));
        }
        self.bound_candidate_id = Some(candidate_id.to_owned());
        Ok(self)
    }

    #[must_use]
    pub fn bound_candidate_id(&self) -> Option<&str> {
        self.bound_candidate_id.as_deref()
    }

    fn net_gain_millis(&self) -> i64 {
        let baseline = i128::from(self.baseline_elapsed_millis);
        let cost = i128::from(self.candidate_elapsed_millis)
            + i128::from(self.restore_transfer_overhead_millis)
            + i128::from(self.publication_overhead_millis)
            + i128::from(self.invalidation_reset_cost_millis);
        clamp_i128_to_i64(baseline - cost)
    }

    fn is_compatible_success(&self) -> bool {
        self.semantic_result_match && self.candidate_succeeded && !self.reset_observed
    }

    fn clears_noise(&self) -> bool {
        self.net_gain_millis() > i64::try_from(self.noise_millis).unwrap_or(i64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateEvidence {
    observation_id: String,
    run_id: String,
    stage: VerificationStage,
    duration_millis: u64,
    sample_count: u16,
    reuse_class: VerificationReuseClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_read: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_written: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_transferred: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_transfer_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toolchain_identity: Option<String>,
    resource_profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suite_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    consumer_identity: Option<String>,
    semantic_validation: SemanticValidationResult,
}

impl CandidateEvidence {
    fn from_observation(observation: &VerificationObservation) -> Self {
        Self {
            observation_id: observation.observation_id.clone(),
            run_id: observation.run_id.clone(),
            stage: observation.stage,
            duration_millis: observation.duration_millis,
            sample_count: observation.sample_count,
            reuse_class: observation.reuse_class,
            bytes_read: observation.bytes_read,
            bytes_written: observation.bytes_written,
            bytes_transferred: observation.bytes_transferred,
            artifact_transfer_backend: observation.artifact_transfer_backend.clone(),
            source_identity: observation.source_identity.clone(),
            toolchain_identity: observation.toolchain_identity.clone(),
            resource_profile: observation.resource_profile.clone(),
            artifact_identity: observation.artifact_identity.clone(),
            tool_identity: observation.tool_identity.clone(),
            suite_identity: observation.suite_identity.clone(),
            consumer_identity: observation.consumer_identity.clone(),
            semantic_validation: observation.semantic_validation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UtilityInputKind {
    RestoreTransferOverhead,
    PublicationOverhead,
    InvalidationResetCost,
    StorageCost,
}

impl UtilityInputKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RestoreTransferOverhead => "restore_transfer_overhead",
            Self::PublicationOverhead => "publication_overhead",
            Self::InvalidationResetCost => "invalidation_reset_cost",
            Self::StorageCost => "storage_cost",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptimizationUtility {
    avoided_work_millis: u64,
    restore_transfer_overhead_millis: u64,
    publication_overhead_millis: u64,
    invalidation_reset_cost_millis: u64,
    estimated_net_saved_millis: i64,
    storage_bytes: u64,
    hit_frequency_basis_points: u16,
    missing_cost_inputs: Vec<UtilityInputKind>,
}

impl OptimizationUtility {
    #[must_use]
    pub const fn estimated_net_saved_millis(&self) -> i64 {
        self.estimated_net_saved_millis
    }

    #[must_use]
    pub const fn storage_bytes(&self) -> u64 {
        self.storage_bytes
    }

    #[must_use]
    pub const fn hit_frequency_basis_points(&self) -> u16 {
        self.hit_frequency_basis_points
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptimizationExperimentSummary {
    trials: usize,
    controlled_trials: usize,
    compatible_successes: usize,
    semantic_disagreements: usize,
    failures_or_resets: usize,
    regressions_or_noise: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    median_baseline_millis: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    median_candidate_millis: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    median_net_gain_millis: Option<i64>,
    experiment_ids: Vec<String>,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptimizationCandidate {
    candidate_id: String,
    class: OptimizationClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reusable_state_class: Option<ReusableStateClass>,
    lifecycle: OptimizationLifecycle,
    confidence: OptimizationConfidence,
    validity: ValidityFingerprint,
    evidence: Vec<CandidateEvidence>,
    utility: OptimizationUtility,
    experiments: OptimizationExperimentSummary,
    next_action: OptimizationNextAction,
}

impl OptimizationCandidate {
    #[must_use]
    pub fn candidate_id(&self) -> &str {
        &self.candidate_id
    }

    #[must_use]
    pub fn subject_identity(&self) -> Option<&str> {
        self.subject_identity.as_deref()
    }

    #[must_use]
    pub const fn class(&self) -> OptimizationClass {
        self.class
    }

    #[must_use]
    pub const fn reusable_state_class(&self) -> Option<ReusableStateClass> {
        self.reusable_state_class
    }

    #[must_use]
    pub const fn lifecycle(&self) -> OptimizationLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub const fn confidence(&self) -> OptimizationConfidence {
        self.confidence
    }

    #[must_use]
    pub const fn validity(&self) -> &ValidityFingerprint {
        &self.validity
    }

    #[must_use]
    pub fn evidence(&self) -> &[CandidateEvidence] {
        &self.evidence
    }

    #[must_use]
    pub const fn utility(&self) -> &OptimizationUtility {
        &self.utility
    }

    /// Explicitly retire a candidate while preserving its evidence and experiment history.
    #[must_use]
    pub fn retired(mut self) -> Self {
        self.lifecycle = OptimizationLifecycle::Retired;
        self.next_action = OptimizationNextAction::None;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdaptiveVerificationCompilerReceipt {
    document_type: AdaptiveVerificationCompilerDocumentType,
    schema_version: u8,
    authority: AdaptiveVerificationCompilerAuthority,
    workload: String,
    profile: String,
    observation_count: usize,
    experiment_count: usize,
    observations: Vec<VerificationObservation>,
    experiments: Vec<OptimizationExperiment>,
    candidates: Vec<OptimizationCandidate>,
}

impl AdaptiveVerificationCompilerReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn authority(&self) -> AdaptiveVerificationCompilerAuthority {
        self.authority
    }

    #[must_use]
    pub fn candidates(&self) -> &[OptimizationCandidate] {
        &self.candidates
    }

    #[must_use]
    pub fn observations(&self) -> &[VerificationObservation] {
        &self.observations
    }

    #[must_use]
    pub fn experiments(&self) -> &[OptimizationExperiment] {
        &self.experiments
    }

    /// Render the typed receipt as deterministic pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed model fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render a compact human explanation from the same typed candidate model.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut output = format!(
            "adaptive verification compiler\nworkload: {}\nprofile: {}\nobservations: {}\nexperiments: {}\ncandidates: {}\n",
            self.workload,
            self.profile,
            self.observation_count,
            self.experiment_count,
            self.candidates.len()
        );
        for candidate in &self.candidates {
            output.push_str(&format!(
                "\ncandidate: {}\nid: {}\nstate: {}\nconfidence: {:?}\n",
                candidate.class.as_str(),
                candidate.candidate_id,
                candidate.lifecycle.as_str(),
                candidate.confidence
            ));
            match candidate.validity.status {
                ValidityFingerprintStatus::Exact => {
                    output.push_str(&format!(
                        "validity: exact ({})\n",
                        candidate
                            .validity
                            .fingerprint
                            .as_deref()
                            .unwrap_or("missing")
                    ));
                }
                ValidityFingerprintStatus::Advisory => {
                    let missing = candidate
                        .validity
                        .missing_inputs
                        .iter()
                        .map(|kind| kind.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    output.push_str(&format!("validity: advisory; missing: {missing}\n"));
                }
                ValidityFingerprintStatus::NotRequired => {
                    output.push_str("validity: no reusable-artifact contract required\n");
                }
            }
            if let Some(state_class) = candidate.reusable_state_class {
                output.push_str(&format!("reusable state class: {}\n", state_class.as_str()));
            }
            output.push_str(&format!(
                "evidence: {} observations\navoided work: {} ms\nrestore/transfer overhead: {} ms\npublication overhead: {} ms\ninvalidation/reset cost: {} ms\nestimated net: {} ms\nstorage: {} bytes\nhit frequency: {} bp\ntrials: {} ({} controlled, {} compatible successes)\nnext action: {}\n",
                candidate.evidence.len(),
                candidate.utility.avoided_work_millis,
                candidate.utility.restore_transfer_overhead_millis,
                candidate.utility.publication_overhead_millis,
                candidate.utility.invalidation_reset_cost_millis,
                candidate.utility.estimated_net_saved_millis,
                candidate.utility.storage_bytes,
                candidate.utility.hit_frequency_basis_points,
                candidate.experiments.trials,
                candidate.experiments.controlled_trials,
                candidate.experiments.compatible_successes,
                candidate.next_action.human(),
            ));
            if !candidate.utility.missing_cost_inputs.is_empty() {
                let missing = candidate
                    .utility
                    .missing_cost_inputs
                    .iter()
                    .map(|input| input.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                output.push_str(&format!("missing utility inputs: {missing}\n"));
            }
            for evidence in &candidate.evidence {
                let transfer = evidence.artifact_transfer_backend.as_deref().unwrap_or("-");
                output.push_str(&format!(
                    "  - {} / {} / {} / {} ms / {} samples / {:?} / transfer {}\n",
                    evidence.run_id,
                    evidence.observation_id,
                    evidence.stage.as_str(),
                    evidence.duration_millis,
                    evidence.sample_count,
                    evidence.reuse_class,
                    transfer
                ));
            }
        }
        output
    }
}

/// Deterministically derive bounded verification optimization candidates.
///
/// This is an observation-only reducer. It never changes workflow YAML, publishes artifacts, selects
/// execution authority, or treats a performance result as verification authority.
///
/// # Errors
///
/// Returns a bounded public error for invalid workload/profile identity, oversized evidence, duplicate
/// observation/experiment identity, or mixed workload/profile inputs.
pub fn compile_verification_optimizations(
    workload: &str,
    profile: &str,
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
) -> Result<AdaptiveVerificationCompilerReceipt, AdaptiveVerificationCompilerError> {
    validate_label(workload)?;
    validate_label(profile)?;
    if observations.len() > MAX_VERIFICATION_OBSERVATIONS {
        return Err(error(
            "verification_observation_limit",
            "verification observation count exceeds the bounded reducer limit",
        ));
    }
    if experiments.len() > MAX_OPTIMIZATION_EXPERIMENTS {
        return Err(error(
            "verification_experiment_limit",
            "verification experiment count exceeds the bounded reducer limit",
        ));
    }

    let mut observation_ids = BTreeSet::new();
    for observation in observations {
        if observation.workload != workload || observation.profile != profile {
            return Err(error(
                "verification_observation_scope_mismatch",
                "every observation must match the requested workload and profile",
            ));
        }
        if !observation_ids.insert(observation.observation_id.as_str()) {
            return Err(error(
                "duplicate_verification_observation",
                "verification observation identifiers must be unique",
            ));
        }
    }

    let mut experiment_ids = BTreeSet::new();
    for experiment in experiments {
        if experiment.workload != workload || experiment.profile != profile {
            return Err(error(
                "verification_experiment_scope_mismatch",
                "every experiment must match the requested workload and profile",
            ));
        }
        if !experiment_ids.insert(experiment.experiment_id.as_str()) {
            return Err(error(
                "duplicate_verification_experiment",
                "verification experiment identifiers must be unique",
            ));
        }
    }

    let mut candidates = Vec::new();
    discover_repeated_compile(observations, experiments, &mut candidates);
    discover_dependency_reuse(observations, experiments, &mut candidates);
    discover_prepared_tools(observations, experiments, &mut candidates);
    discover_artifact_transport(observations, experiments, &mut candidates);
    discover_focused_rebuilds(observations, experiments, &mut candidates);
    discover_parallel_guards(observations, experiments, &mut candidates);
    discover_late_static_guards(observations, experiments, &mut candidates);
    discover_hanging_suites(observations, experiments, &mut candidates);
    discover_irrelevant_platform_lanes(observations, experiments, &mut candidates);

    candidates.sort_by(|left, right| {
        left.class
            .cmp(&right.class)
            .then_with(|| left.candidate_id.cmp(&right.candidate_id))
    });
    candidates.dedup_by(|left, right| left.candidate_id == right.candidate_id);

    let mut retained_observations = observations.to_vec();
    retained_observations.sort_by(|left, right| {
        left.run_id
            .cmp(&right.run_id)
            .then_with(|| left.sequence.cmp(&right.sequence))
            .then_with(|| left.observation_id.cmp(&right.observation_id))
    });
    let mut retained_experiments = experiments.to_vec();
    retained_experiments.sort_by(|left, right| left.experiment_id.cmp(&right.experiment_id));

    Ok(AdaptiveVerificationCompilerReceipt {
        document_type:
            AdaptiveVerificationCompilerDocumentType::AdaptiveVerificationCompilerReceipt,
        schema_version: ADAPTIVE_VERIFICATION_COMPILER_SCHEMA_VERSION,
        authority: AdaptiveVerificationCompilerAuthority::ObservationOnly,
        workload: workload.to_owned(),
        profile: profile.to_owned(),
        observation_count: observations.len(),
        experiment_count: experiments.len(),
        observations: retained_observations,
        experiments: retained_experiments,
        candidates,
    })
}

fn discover_repeated_compile(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let required = validity_requirements(OptimizationClass::ReuseExactCompiledProduct);
    for group in grouped(
        observations.iter().filter(|observation| {
            observation.stage == VerificationStage::Compile
                && observation.duration_millis >= MIN_COMPILE_MILLIS
        }),
        |observation| {
            grouping_key(
                observation,
                required,
                observation.source_identity.as_deref(),
            )
        },
    ) {
        if observation_sample_count(&group) >= MIN_REPETITIONS {
            let subject = validity_subject(&group, ValidityInputKind::ProductSchema);
            candidates.push(build_candidate(
                OptimizationClass::ReuseExactCompiledProduct,
                subject,
                group,
                observations,
                experiments,
            ));
        }
    }
}

fn discover_dependency_reuse(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let required = validity_requirements(OptimizationClass::ReuseDependencyGeneration);
    for group in grouped(
        observations.iter().filter(|observation| {
            observation.stage == VerificationStage::DependencyResolution
                && observation.duration_millis >= MIN_DEPENDENCY_MILLIS
        }),
        |observation| grouping_key(observation, required, None),
    ) {
        if observation_sample_count(&group) >= MIN_REPETITIONS {
            candidates.push(build_candidate(
                OptimizationClass::ReuseDependencyGeneration,
                validity_subject(&group, ValidityInputKind::Lockfile),
                group,
                observations,
                experiments,
            ));
        }
    }
}

fn discover_prepared_tools(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let required = validity_requirements(OptimizationClass::BakePreparedTool);
    for group in grouped(
        observations.iter().filter(|observation| {
            observation.stage == VerificationStage::SetupToolInstallation
                && observation.duration_millis >= MIN_TOOL_SETUP_MILLIS
                && observation.tool_identity.is_some()
        }),
        |observation| grouping_key(observation, required, observation.tool_identity.as_deref()),
    ) {
        if observation_sample_count(&group) >= MIN_REPETITIONS {
            let subject =
                common_subject(&group, |observation| observation.tool_identity.as_deref());
            candidates.push(build_candidate(
                OptimizationClass::BakePreparedTool,
                subject,
                group,
                observations,
                experiments,
            ));
        }
    }
}

fn discover_artifact_transport(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let groups = grouped(
        observations.iter().filter(|observation| {
            observation.stage == VerificationStage::ArtifactTransfer
                && observation.bytes_transferred.unwrap_or(0) >= MIN_LARGE_ARTIFACT_BYTES
                && observation.artifact_identity.is_some()
        }),
        |observation| observation.artifact_identity.clone().unwrap_or_default(),
    );

    for group in groups {
        if observation_sample_count(&group) < MIN_REPETITIONS {
            continue;
        }
        let subject = common_subject(&group, |observation| {
            observation.artifact_identity.as_deref()
        });
        candidates.push(build_candidate(
            OptimizationClass::RetainLocalImmutableArtifact,
            subject.clone(),
            group.clone(),
            observations,
            experiments,
        ));

        let ratio_evidence = group
            .iter()
            .filter_map(|observation| {
                let transferred = observation.bytes_transferred?;
                let required = observation.required_consumer_bytes?;
                if transferred == 0 {
                    return None;
                }
                Some((
                    required.saturating_mul(10_000) / transferred,
                    usize::from(observation.sample_count),
                ))
            })
            .collect::<Vec<_>>();
        let ratio_sample_count = ratio_evidence
            .iter()
            .map(|(_, sample_count)| *sample_count)
            .sum::<usize>();
        if ratio_sample_count >= MIN_REPETITIONS {
            let mut ratios = ratio_evidence
                .iter()
                .map(|(ratio, _)| *ratio)
                .collect::<Vec<_>>();
            ratios.sort_unstable();
            if median_u64(&ratios) <= MAX_SPLIT_REQUIRED_BASIS_POINTS {
                candidates.push(build_candidate(
                    OptimizationClass::SplitConsumerArtifact,
                    subject,
                    group,
                    observations,
                    experiments,
                ));
            }
        }
    }
}

fn discover_focused_rebuilds(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let mut by_run: BTreeMap<&str, Vec<&VerificationObservation>> = BTreeMap::new();
    for observation in observations {
        by_run
            .entry(observation.run_id.as_str())
            .or_default()
            .push(observation);
    }

    let mut by_key: BTreeMap<String, Vec<&VerificationObservation>> = BTreeMap::new();
    for run in by_run.values_mut() {
        run.sort_by_key(|observation| observation.sequence);
        for test in run.iter().copied().filter(|observation| {
            observation.stage == VerificationStage::TestExecution
                && observation.test_scope == Some(VerificationTestScope::Focused)
                && observation.rebuilt_before_test
        }) {
            let Some(compile) = run
                .iter()
                .copied()
                .filter(|observation| {
                    observation.stage == VerificationStage::Compile
                        && observation.sequence < test.sequence
                })
                .max_by_key(|observation| observation.sequence)
            else {
                continue;
            };
            if compile.duration_millis < MIN_COMPILE_MILLIS
                || compile.duration_millis
                    < test.duration_millis.saturating_mul(FOCUSED_COMPILE_RATIO)
            {
                continue;
            }
            let key = grouping_key(
                compile,
                validity_requirements(OptimizationClass::RunTestWithoutRebuild),
                test.suite_identity.as_deref(),
            );
            let entry = by_key.entry(key).or_default();
            entry.push(compile);
            entry.push(test);
        }
    }

    for evidence in by_key.into_values() {
        let run_count = evidence
            .iter()
            .map(|observation| observation.run_id.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if run_count >= MIN_REPETITIONS {
            let subject = common_subject(&evidence, |observation| {
                observation.suite_identity.as_deref()
            });
            candidates.push(build_candidate(
                OptimizationClass::RunTestWithoutRebuild,
                subject,
                evidence,
                observations,
                experiments,
            ));
        }
    }
}

fn discover_parallel_guards(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let mut groups: BTreeMap<String, Vec<&VerificationObservation>> = BTreeMap::new();
    for observation in observations.iter().filter(|observation| {
        observation.stage == VerificationStage::StaticGuard && observation.independent
    }) {
        if let Some(group) = observation.independence_group.as_deref() {
            groups
                .entry(group.to_owned())
                .or_default()
                .push(observation);
        }
    }

    for (subject, evidence) in groups {
        let mut by_run: BTreeMap<&str, Vec<&VerificationObservation>> = BTreeMap::new();
        for observation in &evidence {
            by_run
                .entry(observation.run_id.as_str())
                .or_default()
                .push(*observation);
        }
        let qualifying_runs = by_run
            .values()
            .filter(|run| {
                run.len() >= 2
                    && run
                        .iter()
                        .map(|observation| observation.duration_millis)
                        .sum::<u64>()
                        >= MIN_SERIAL_GUARD_MILLIS
            })
            .count();
        if qualifying_runs >= MIN_REPETITIONS {
            candidates.push(build_candidate(
                OptimizationClass::ParallelizeIndependentChecks,
                Some(subject),
                evidence,
                observations,
                experiments,
            ));
        }
    }
}

fn discover_late_static_guards(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let mut by_run: BTreeMap<&str, Vec<&VerificationObservation>> = BTreeMap::new();
    for observation in observations {
        by_run
            .entry(observation.run_id.as_str())
            .or_default()
            .push(observation);
    }

    let mut groups: BTreeMap<String, Vec<&VerificationObservation>> = BTreeMap::new();
    for run in by_run.values_mut() {
        run.sort_by_key(|observation| observation.sequence);
        for guard in run.iter().copied().filter(|observation| {
            observation.stage == VerificationStage::StaticGuard
                && matches!(
                    observation.semantic_validation,
                    SemanticValidationResult::Failed | SemanticValidationResult::TimedOut
                )
        }) {
            let preceding = run
                .iter()
                .copied()
                .filter(|observation| observation.sequence < guard.sequence)
                .filter(|observation| {
                    matches!(
                        observation.stage,
                        VerificationStage::Compile
                            | VerificationStage::DependencyResolution
                            | VerificationStage::ArtifactTransfer
                            | VerificationStage::TestExecution
                    )
                })
                .collect::<Vec<_>>();
            let wasted = preceding
                .iter()
                .map(|observation| observation.duration_millis)
                .sum::<u64>();
            if wasted < MIN_SERIAL_GUARD_MILLIS {
                continue;
            }
            let Some(subject) = guard
                .suite_identity
                .as_deref()
                .or(guard.independence_group.as_deref())
            else {
                continue;
            };
            let entry = groups.entry(subject.to_owned()).or_default();
            entry.extend(preceding);
            entry.push(guard);
        }
    }

    for (subject, evidence) in groups {
        let failed_runs = evidence
            .iter()
            .filter(|observation| {
                observation.stage == VerificationStage::StaticGuard
                    && matches!(
                        observation.semantic_validation,
                        SemanticValidationResult::Failed | SemanticValidationResult::TimedOut
                    )
            })
            .map(|observation| observation.run_id.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if failed_runs >= 2 {
            candidates.push(build_candidate(
                OptimizationClass::MoveStaticGuardEarlier,
                Some(subject),
                evidence,
                observations,
                experiments,
            ));
        }
    }
}

fn discover_hanging_suites(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let mut by_suite: BTreeMap<String, Vec<&VerificationObservation>> = BTreeMap::new();
    for observation in observations.iter().filter(|observation| {
        observation.stage == VerificationStage::TestExecution
            && observation.suite_identity.is_some()
    }) {
        by_suite
            .entry(observation.suite_identity.clone().unwrap_or_default())
            .or_default()
            .push(observation);
    }
    for (suite, evidence) in by_suite {
        let timeout_count = evidence
            .iter()
            .filter(|observation| {
                observation.semantic_validation == SemanticValidationResult::TimedOut
            })
            .map(|observation| usize::from(observation.sample_count))
            .sum::<usize>();
        if observation_sample_count(&evidence) >= MIN_REPETITIONS && timeout_count >= 2 {
            candidates.push(build_candidate(
                OptimizationClass::IsolateFlakyOrHangingSuite,
                Some(suite),
                evidence,
                observations,
                experiments,
            ));
        }
    }
}

fn discover_irrelevant_platform_lanes(
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
    candidates: &mut Vec<OptimizationCandidate>,
) {
    let mut by_lane: BTreeMap<String, Vec<&VerificationObservation>> = BTreeMap::new();
    for observation in observations.iter().filter(|observation| {
        observation.platform_relevant == Some(false)
            && observation.duration_millis >= MIN_IRRELEVANT_LANE_MILLIS
            && observation.platform_lane.is_some()
    }) {
        by_lane
            .entry(observation.platform_lane.clone().unwrap_or_default())
            .or_default()
            .push(observation);
    }
    for (lane, evidence) in by_lane {
        if observation_sample_count(&evidence) >= MIN_REPETITIONS {
            candidates.push(build_candidate(
                OptimizationClass::SkipIrrelevantPlatformLane,
                Some(lane),
                evidence,
                observations,
                experiments,
            ));
        }
    }
}

fn build_candidate(
    class: OptimizationClass,
    subject_identity: Option<String>,
    mut evidence: Vec<&VerificationObservation>,
    observations: &[VerificationObservation],
    experiments: &[OptimizationExperiment],
) -> OptimizationCandidate {
    evidence.sort_by(|left, right| {
        left.run_id
            .cmp(&right.run_id)
            .then_with(|| left.sequence.cmp(&right.sequence))
            .then_with(|| left.observation_id.cmp(&right.observation_id))
    });
    evidence.dedup_by(|left, right| left.observation_id == right.observation_id);

    let validity = build_validity(class, &evidence);
    let candidate_id = candidate_id(
        class,
        subject_identity.as_deref(),
        validity.fingerprint.as_deref(),
        &evidence,
    );
    let matched_experiments = experiments
        .iter()
        .filter(|experiment| {
            experiment.class == class
                && experiment.bound_candidate_id.as_deref() == Some(candidate_id.as_str())
                && experiment
                    .subject_identity
                    .as_deref()
                    .is_none_or(|subject| subject_identity.as_deref() == Some(subject))
        })
        .collect::<Vec<_>>();
    let experiment_summary = summarize_experiments(&matched_experiments);
    let (lifecycle, confidence, next_action) =
        decide_lifecycle(class, &validity, &matched_experiments);
    let utility = estimate_utility(class, subject_identity.as_deref(), &evidence, observations);

    OptimizationCandidate {
        candidate_id,
        class,
        subject_identity,
        reusable_state_class: class.reusable_state_class(),
        lifecycle,
        confidence,
        validity,
        evidence: evidence
            .iter()
            .map(|observation| CandidateEvidence::from_observation(observation))
            .collect(),
        utility,
        experiments: experiment_summary,
        next_action,
    }
}

fn build_validity(
    class: OptimizationClass,
    evidence: &[&VerificationObservation],
) -> ValidityFingerprint {
    let mut inputs = evidence
        .iter()
        .flat_map(|observation| observation.validity_inputs.iter().cloned())
        .collect::<Vec<_>>();
    inputs.sort();
    inputs.dedup();

    if !class.requires_exact_validity() {
        return ValidityFingerprint {
            status: ValidityFingerprintStatus::NotRequired,
            inputs,
            missing_inputs: Vec::new(),
            fingerprint: None,
        };
    }

    let required = validity_requirements(class);
    let present = inputs
        .iter()
        .map(ValidityInput::kind)
        .collect::<BTreeSet<_>>();
    let missing_inputs = required
        .iter()
        .copied()
        .filter(|kind| !present.contains(kind))
        .collect::<Vec<_>>();

    if !missing_inputs.is_empty() {
        return ValidityFingerprint {
            status: ValidityFingerprintStatus::Advisory,
            inputs,
            missing_inputs,
            fingerprint: None,
        };
    }

    let mut hasher = Sha256::new();
    for input in &inputs {
        hasher.update(input.kind.as_str().as_bytes());
        hasher.update(b"=");
        hasher.update(input.identity.as_bytes());
        hasher.update(b"\n");
    }
    let digest = format!("{:x}", hasher.finalize());
    ValidityFingerprint {
        status: ValidityFingerprintStatus::Exact,
        inputs,
        missing_inputs,
        fingerprint: Some(format!("sha256:{digest}")),
    }
}

fn validity_requirements(class: OptimizationClass) -> &'static [ValidityInputKind] {
    use ValidityInputKind::{
        Architecture, ArtifactIdentity, BuildConfiguration, CompilerFlags, ConsumerContract,
        Lockfile, ProductSchema, Sdk, SourceTree, ToolIdentity, Toolchain,
    };
    match class {
        OptimizationClass::ReuseExactCompiledProduct | OptimizationClass::RunTestWithoutRebuild => {
            &[
                SourceTree,
                Toolchain,
                Sdk,
                Architecture,
                BuildConfiguration,
                CompilerFlags,
                ProductSchema,
            ]
        }
        OptimizationClass::RetainLocalImmutableArtifact => &[ArtifactIdentity, ProductSchema],
        OptimizationClass::ReuseDependencyGeneration => &[Lockfile, Toolchain, Architecture],
        OptimizationClass::BakePreparedTool => &[ToolIdentity, Toolchain, Architecture],
        OptimizationClass::SplitConsumerArtifact => {
            &[ArtifactIdentity, ProductSchema, ConsumerContract]
        }
        OptimizationClass::MoveStaticGuardEarlier
        | OptimizationClass::ParallelizeIndependentChecks
        | OptimizationClass::IsolateFlakyOrHangingSuite
        | OptimizationClass::SkipIrrelevantPlatformLane => &[],
    }
}

fn decide_lifecycle(
    class: OptimizationClass,
    validity: &ValidityFingerprint,
    experiments: &[&OptimizationExperiment],
) -> (
    OptimizationLifecycle,
    OptimizationConfidence,
    OptimizationNextAction,
) {
    if class.requires_exact_validity() && validity.status != ValidityFingerprintStatus::Exact {
        return (
            OptimizationLifecycle::Observed,
            OptimizationConfidence::Advisory,
            OptimizationNextAction::StateExactValidityInputs,
        );
    }
    if experiments.is_empty() {
        return (
            OptimizationLifecycle::Candidate,
            OptimizationConfidence::ExperimentRequired,
            OptimizationNextAction::RunControlledAb,
        );
    }

    let semantic_disagreement = experiments
        .iter()
        .any(|experiment| !experiment.semantic_result_match);
    let failed_or_reset = experiments
        .iter()
        .any(|experiment| !experiment.candidate_succeeded || experiment.reset_observed);
    let controlled_regression = experiments.iter().any(|experiment| {
        experiment.controlled && experiment.is_compatible_success() && !experiment.clears_noise()
    });
    if semantic_disagreement || failed_or_reset || controlled_regression {
        return (
            OptimizationLifecycle::Demoted,
            OptimizationConfidence::RejectedEvidence,
            OptimizationNextAction::InvestigateRegression,
        );
    }

    let successful_controlled = experiments
        .iter()
        .filter(|experiment| {
            experiment.controlled && experiment.is_compatible_success() && experiment.clears_noise()
        })
        .count();
    if successful_controlled >= 3 {
        let next_action = if class.requires_exact_validity() {
            OptimizationNextAction::HandOffToReusableStateLifecycle
        } else {
            OptimizationNextAction::EmitRepositoryRecommendation
        };
        (
            OptimizationLifecycle::Preferred,
            OptimizationConfidence::PreferredEvidence,
            next_action,
        )
    } else if successful_controlled >= 2 {
        (
            OptimizationLifecycle::Accepted,
            OptimizationConfidence::AcceptedEvidence,
            OptimizationNextAction::GatherCompatibleTrials,
        )
    } else {
        (
            OptimizationLifecycle::Experimenting,
            OptimizationConfidence::PartialControlledEvidence,
            OptimizationNextAction::GatherCompatibleTrials,
        )
    }
}

fn summarize_experiments(experiments: &[&OptimizationExperiment]) -> OptimizationExperimentSummary {
    let mut baselines = experiments
        .iter()
        .map(|experiment| experiment.baseline_elapsed_millis)
        .collect::<Vec<_>>();
    let mut candidates = experiments
        .iter()
        .map(|experiment| experiment.candidate_elapsed_millis)
        .collect::<Vec<_>>();
    let mut gains = experiments
        .iter()
        .map(|experiment| experiment.net_gain_millis())
        .collect::<Vec<_>>();
    baselines.sort_unstable();
    candidates.sort_unstable();
    gains.sort_unstable();

    OptimizationExperimentSummary {
        trials: experiments.len(),
        controlled_trials: experiments
            .iter()
            .filter(|experiment| experiment.controlled)
            .count(),
        compatible_successes: experiments
            .iter()
            .filter(|experiment| experiment.is_compatible_success())
            .count(),
        semantic_disagreements: experiments
            .iter()
            .filter(|experiment| !experiment.semantic_result_match)
            .count(),
        failures_or_resets: experiments
            .iter()
            .filter(|experiment| !experiment.candidate_succeeded || experiment.reset_observed)
            .count(),
        regressions_or_noise: experiments
            .iter()
            .filter(|experiment| {
                experiment.controlled
                    && experiment.is_compatible_success()
                    && !experiment.clears_noise()
            })
            .count(),
        median_baseline_millis: median_option_u64(&baselines),
        median_candidate_millis: median_option_u64(&candidates),
        median_net_gain_millis: median_option_i64(&gains),
        experiment_ids: experiments
            .iter()
            .map(|experiment| experiment.experiment_id.clone())
            .collect(),
        evidence_refs: experiments
            .iter()
            .map(|experiment| experiment.evidence_ref.clone())
            .collect(),
    }
}

fn estimate_utility(
    class: OptimizationClass,
    subject_identity: Option<&str>,
    evidence: &[&VerificationObservation],
    observations: &[VerificationObservation],
) -> OptimizationUtility {
    let avoided_work_millis = match class {
        OptimizationClass::SplitConsumerArtifact => {
            median_run_sum(evidence.iter().filter_map(|observation| {
                let transferred = observation.bytes_transferred?;
                let required = observation.required_consumer_bytes?;
                if transferred == 0 || required >= transferred {
                    return Some((observation.run_id.as_str(), 0));
                }
                let avoided_bytes = transferred - required;
                let saved = observation.duration_millis.saturating_mul(avoided_bytes) / transferred;
                Some((observation.run_id.as_str(), saved))
            }))
            .unwrap_or(0)
        }
        OptimizationClass::ParallelizeIndependentChecks => {
            let mut savings = estimated_parallel_guard_savings(evidence);
            savings.sort_unstable();
            median_option_u64(&savings).unwrap_or(0)
        }
        OptimizationClass::MoveStaticGuardEarlier => {
            let mut savings = estimated_late_guard_savings(evidence);
            savings.sort_unstable();
            median_option_u64(&savings).unwrap_or(0)
        }
        _ => median_run_sum(
            evidence
                .iter()
                .filter(|observation| primary_avoided_stage(class, observation.stage))
                .map(|observation| (observation.run_id.as_str(), observation.duration_millis)),
        )
        .unwrap_or(0),
    };

    let related = observations
        .iter()
        .filter(|observation| {
            subject_identity.is_none_or(|subject| observation_matches_subject(observation, subject))
        })
        .collect::<Vec<_>>();

    let restore_transfer = related
        .iter()
        .filter(|observation| {
            matches!(
                observation.stage,
                VerificationStage::Restore | VerificationStage::ArtifactTransfer
            ) && !primary_avoided_stage(class, observation.stage)
        })
        .map(|observation| (observation.run_id.as_str(), observation.duration_millis))
        .collect::<Vec<_>>();
    let restore_transfer_observed = !restore_transfer.is_empty();
    let restore_transfer_overhead_millis =
        median_run_sum(restore_transfer.into_iter()).unwrap_or(0);

    let publication = related
        .iter()
        .filter(|observation| observation.stage == VerificationStage::ArtifactPackaging)
        .map(|observation| (observation.run_id.as_str(), observation.duration_millis))
        .collect::<Vec<_>>();
    let publication_observed = !publication.is_empty();
    let publication_overhead_millis = median_run_sum(publication.into_iter()).unwrap_or(0);

    let reset = related
        .iter()
        .filter(|observation| {
            observation.stage == VerificationStage::Cleanup
                && matches!(
                    observation.semantic_validation,
                    SemanticValidationResult::Failed | SemanticValidationResult::TimedOut
                )
        })
        .map(|observation| (observation.run_id.as_str(), observation.duration_millis))
        .collect::<Vec<_>>();
    let reset_observed = !reset.is_empty();
    let invalidation_reset_cost_millis = median_run_sum(reset.into_iter()).unwrap_or(0);

    let storage_bytes = related
        .iter()
        .filter_map(|observation| observation.bytes_written.or(observation.bytes_transferred))
        .max()
        .unwrap_or(0);
    let storage_observed = storage_bytes > 0;

    let restore_hit_evidence = if class == OptimizationClass::RetainLocalImmutableArtifact {
        related
            .iter()
            .filter(|observation| observation.stage == VerificationStage::Restore)
            .copied()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let relevant_for_hit = if restore_hit_evidence.is_empty() {
        related
            .iter()
            .filter(|observation| primary_avoided_stage(class, observation.stage))
            .copied()
            .collect::<Vec<_>>()
    } else {
        restore_hit_evidence
    };
    let hit_count = relevant_for_hit
        .iter()
        .filter(|observation| observation.reuse_class == VerificationReuseClass::Reuse)
        .map(|observation| usize::from(observation.sample_count))
        .sum::<usize>();
    let lookup_count = relevant_for_hit
        .iter()
        .map(|observation| usize::from(observation.sample_count))
        .sum::<usize>();
    let hit_frequency_basis_points = if lookup_count == 0 {
        0
    } else {
        u16::try_from(
            hit_count
                .saturating_mul(10_000)
                .checked_div(lookup_count)
                .unwrap_or(0),
        )
        .unwrap_or(10_000)
    };

    let net = i128::from(avoided_work_millis)
        - i128::from(restore_transfer_overhead_millis)
        - i128::from(publication_overhead_millis)
        - i128::from(invalidation_reset_cost_millis);
    let mut missing_cost_inputs = Vec::new();
    if !restore_transfer_observed {
        missing_cost_inputs.push(UtilityInputKind::RestoreTransferOverhead);
    }
    if !publication_observed {
        missing_cost_inputs.push(UtilityInputKind::PublicationOverhead);
    }
    if !reset_observed {
        missing_cost_inputs.push(UtilityInputKind::InvalidationResetCost);
    }
    if !storage_observed {
        missing_cost_inputs.push(UtilityInputKind::StorageCost);
    }

    OptimizationUtility {
        avoided_work_millis,
        restore_transfer_overhead_millis,
        publication_overhead_millis,
        invalidation_reset_cost_millis,
        estimated_net_saved_millis: clamp_i128_to_i64(net),
        storage_bytes,
        hit_frequency_basis_points,
        missing_cost_inputs,
    }
}

fn primary_avoided_stage(class: OptimizationClass, stage: VerificationStage) -> bool {
    match class {
        OptimizationClass::ReuseExactCompiledProduct | OptimizationClass::RunTestWithoutRebuild => {
            matches!(stage, VerificationStage::Compile | VerificationStage::Link)
        }
        OptimizationClass::RetainLocalImmutableArtifact
        | OptimizationClass::SplitConsumerArtifact => stage == VerificationStage::ArtifactTransfer,
        OptimizationClass::ReuseDependencyGeneration => {
            stage == VerificationStage::DependencyResolution
        }
        OptimizationClass::BakePreparedTool => stage == VerificationStage::SetupToolInstallation,
        OptimizationClass::MoveStaticGuardEarlier
        | OptimizationClass::ParallelizeIndependentChecks => {
            stage == VerificationStage::StaticGuard
        }
        OptimizationClass::IsolateFlakyOrHangingSuite => stage == VerificationStage::TestExecution,
        OptimizationClass::SkipIrrelevantPlatformLane => true,
    }
}

fn median_run_sum<'a, I>(values: I) -> Option<u64>
where
    I: Iterator<Item = (&'a str, u64)>,
{
    let mut by_run: BTreeMap<&str, u64> = BTreeMap::new();
    for (run_id, value) in values {
        let total = by_run.entry(run_id).or_default();
        *total = total.saturating_add(value);
    }
    let mut totals = by_run.into_values().collect::<Vec<_>>();
    totals.sort_unstable();
    median_option_u64(&totals)
}

fn estimated_parallel_guard_savings(evidence: &[&VerificationObservation]) -> Vec<u64> {
    let mut by_run: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for observation in evidence {
        by_run
            .entry(observation.run_id.as_str())
            .or_default()
            .push(observation.duration_millis);
    }
    by_run
        .into_values()
        .filter_map(|durations| {
            let total = durations.iter().sum::<u64>();
            let max = durations.iter().copied().max()?;
            Some(total.saturating_sub(max))
        })
        .collect()
}

fn estimated_late_guard_savings(evidence: &[&VerificationObservation]) -> Vec<u64> {
    let mut by_run: BTreeMap<&str, Vec<&VerificationObservation>> = BTreeMap::new();
    for observation in evidence {
        by_run
            .entry(observation.run_id.as_str())
            .or_default()
            .push(*observation);
    }
    by_run
        .into_values()
        .filter_map(|run| {
            let guard_sequence = run
                .iter()
                .filter(|observation| observation.stage == VerificationStage::StaticGuard)
                .map(|observation| observation.sequence)
                .min()?;
            Some(
                run.iter()
                    .filter(|observation| observation.sequence < guard_sequence)
                    .map(|observation| observation.duration_millis)
                    .sum(),
            )
        })
        .collect()
}

fn observation_sample_count(evidence: &[&VerificationObservation]) -> usize {
    evidence
        .iter()
        .map(|observation| usize::from(observation.sample_count))
        .sum()
}

fn observation_matches_subject(observation: &VerificationObservation, subject: &str) -> bool {
    observation.artifact_identity.as_deref() == Some(subject)
        || observation.tool_identity.as_deref() == Some(subject)
        || observation.suite_identity.as_deref() == Some(subject)
        || observation.independence_group.as_deref() == Some(subject)
        || observation.platform_lane.as_deref() == Some(subject)
        || observation.source_identity.as_deref() == Some(subject)
        || observation
            .validity_inputs
            .iter()
            .any(|input| input.identity == subject)
}

fn grouped<'a, I, F>(observations: I, mut key: F) -> Vec<Vec<&'a VerificationObservation>>
where
    I: Iterator<Item = &'a VerificationObservation>,
    F: FnMut(&VerificationObservation) -> String,
{
    let mut groups: BTreeMap<String, Vec<&VerificationObservation>> = BTreeMap::new();
    for observation in observations {
        groups
            .entry(key(observation))
            .or_default()
            .push(observation);
    }
    groups.into_values().collect()
}

fn grouping_key(
    observation: &VerificationObservation,
    required: &[ValidityInputKind],
    extra: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    for kind in required {
        let mut values = observation
            .validity_inputs
            .iter()
            .filter(|input| input.kind == *kind)
            .map(|input| input.identity.as_str())
            .collect::<Vec<_>>();
        values.sort_unstable();
        if values.is_empty() {
            parts.push(format!("{}=<missing>", kind.as_str()));
        } else {
            parts.push(format!("{}={}", kind.as_str(), values.join("+")));
        }
    }
    if let Some(extra) = extra {
        parts.push(format!("subject={extra}"));
    }
    parts.join("|")
}

fn common_subject<'a, F>(
    evidence: &'a [&'a VerificationObservation],
    mut subject: F,
) -> Option<String>
where
    F: FnMut(&'a VerificationObservation) -> Option<&'a str>,
{
    let values = evidence
        .iter()
        .filter_map(|observation| subject(observation))
        .collect::<BTreeSet<_>>();
    if values.len() == 1 {
        values.into_iter().next().map(str::to_owned)
    } else {
        None
    }
}

fn validity_subject(
    evidence: &[&VerificationObservation],
    kind: ValidityInputKind,
) -> Option<String> {
    let values = evidence
        .iter()
        .flat_map(|observation| observation.validity_inputs.iter())
        .filter(|input| input.kind == kind)
        .map(|input| input.identity.as_str())
        .collect::<BTreeSet<_>>();
    if values.len() == 1 {
        values.into_iter().next().map(str::to_owned)
    } else {
        None
    }
}

fn candidate_id(
    class: OptimizationClass,
    subject: Option<&str>,
    validity_fingerprint: Option<&str>,
    evidence: &[&VerificationObservation],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(class.as_str().as_bytes());
    hasher.update(b"\n");
    if let Some(subject) = subject {
        hasher.update(subject.as_bytes());
    }
    hasher.update(b"\n");
    if let Some(fingerprint) = validity_fingerprint {
        hasher.update(fingerprint.as_bytes());
    } else if !class.requires_exact_validity() {
        if let Some(subject) = subject {
            hasher.update(b"subject=");
            hasher.update(subject.as_bytes());
        } else {
            for observation in evidence {
                hasher.update(observation.observation_id.as_bytes());
                hasher.update(b"\n");
            }
        }
    } else {
        for observation in evidence {
            hasher.update(observation.observation_id.as_bytes());
            hasher.update(b"\n");
        }
    }
    let digest = format!("{:x}", hasher.finalize());
    format!("avc-{}-{}", class.as_str(), &digest[..16])
}

fn median_u64(sorted: &[u64]) -> u64 {
    sorted[sorted.len() / 2]
}

fn median_option_u64(sorted: &[u64]) -> Option<u64> {
    (!sorted.is_empty()).then(|| median_u64(sorted))
}

fn median_option_i64(sorted: &[i64]) -> Option<i64> {
    (!sorted.is_empty()).then(|| sorted[sorted.len() / 2])
}

fn clamp_i128_to_i64(value: i128) -> i64 {
    if value > i128::from(i64::MAX) {
        i64::MAX
    } else if value < i128::from(i64::MIN) {
        i64::MIN
    } else {
        value as i64
    }
}

fn validate_label(value: &str) -> Result<(), AdaptiveVerificationCompilerError> {
    if value.is_empty()
        || value.len() > MAX_LABEL_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:@+".contains(&byte))
    {
        return Err(error(
            "verification_identity_invalid",
            "verification identities must be compact ASCII tokens",
        ));
    }
    Ok(())
}

fn validate_duration(value: u64) -> Result<(), AdaptiveVerificationCompilerError> {
    if value > MAX_VERIFICATION_DURATION_MILLIS {
        return Err(error(
            "verification_duration_out_of_range",
            "verification duration exceeds the bounded observation range",
        ));
    }
    Ok(())
}

fn validate_bytes(value: u64) -> Result<(), AdaptiveVerificationCompilerError> {
    if value > MAX_VERIFICATION_BYTES {
        return Err(error(
            "verification_bytes_out_of_range",
            "verification byte count exceeds the bounded observation range",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdaptiveVerificationCompilerError {
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for AdaptiveVerificationCompilerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.problem)
    }
}

impl std::error::Error for AdaptiveVerificationCompilerError {}

const fn error(code: &'static str, problem: &'static str) -> AdaptiveVerificationCompilerError {
    AdaptiveVerificationCompilerError { code, problem }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validity(
        extra: &[(ValidityInputKind, &str)],
    ) -> Result<Vec<ValidityInput>, AdaptiveVerificationCompilerError> {
        extra
            .iter()
            .map(|(kind, identity)| ValidityInput::new(*kind, identity))
            .collect()
    }

    fn compile_observation(id: &str, run: &str, duration_millis: u64) -> VerificationObservation {
        VerificationObservation::new(
            id,
            run,
            "cmux-ci",
            "macos-full",
            2,
            VerificationStage::Compile,
            duration_millis,
            VerificationReuseClass::Cold,
            SemanticValidationResult::Passed,
            "macos-arm64-6x",
        )
        .unwrap()
        .with_source_toolchain(Some("tree-a"), Some("xcode-26.6"))
        .unwrap()
        .with_validity_inputs(
            &validity(&[
                (ValidityInputKind::Sdk, "macos-26.6"),
                (ValidityInputKind::Architecture, "arm64"),
                (ValidityInputKind::BuildConfiguration, "debug"),
                (ValidityInputKind::CompilerFlags, "flags-a"),
                (ValidityInputKind::ProductSchema, "app-host-v2"),
            ])
            .unwrap(),
        )
    }

    fn rejected_resource_reuse_observation() -> VerificationObservation {
        VerificationObservation::new(
            "cmux-12985-resource-skip",
            "cmux-12985-matched-noop",
            "cmux-ci",
            "macos-full",
            1,
            VerificationStage::Compile,
            66_200,
            VerificationReuseClass::Reuse,
            SemanticValidationResult::Passed,
            "macos-arm64-local",
        )
        .unwrap()
        .with_sample_count(4)
        .unwrap()
        .with_source_toolchain(Some("resource-tree-a"), Some("xcode-27"))
        .unwrap()
        .with_validity_inputs(
            &validity(&[
                (ValidityInputKind::Sdk, "macos-26.5"),
                (ValidityInputKind::Architecture, "arm64"),
                (ValidityInputKind::BuildConfiguration, "debug"),
                (ValidityInputKind::CompilerFlags, "resource-flags-a"),
                (ValidityInputKind::ProductSchema, "bundled-resources-v1"),
            ])
            .unwrap(),
        )
    }

    fn release_compile_observation(
        id: &str,
        run: &str,
        duration_millis: u64,
    ) -> VerificationObservation {
        VerificationObservation::new(
            id,
            run,
            "cmux-ci",
            "macos-full",
            11,
            VerificationStage::Compile,
            duration_millis,
            VerificationReuseClass::Cold,
            SemanticValidationResult::Passed,
            "macos-arm64-6x",
        )
        .unwrap()
        .with_source_toolchain(Some("tree-a"), Some("xcode-26.6"))
        .unwrap()
        .with_validity_inputs(
            &validity(&[
                (ValidityInputKind::Sdk, "macos-26.6"),
                (ValidityInputKind::Architecture, "arm64"),
                (ValidityInputKind::BuildConfiguration, "release"),
                (ValidityInputKind::CompilerFlags, "release-flags-a"),
                (ValidityInputKind::ProductSchema, "release-product-v1"),
            ])
            .unwrap(),
        )
    }

    fn transfer_observation(
        id: &str,
        run: &str,
        sequence: u16,
        consumer: &str,
        backend: &str,
    ) -> VerificationObservation {
        VerificationObservation::new(
            id,
            run,
            "cmux-ci",
            "macos-full",
            sequence,
            VerificationStage::ArtifactTransfer,
            16_000,
            VerificationReuseClass::Cold,
            SemanticValidationResult::Passed,
            "macos-arm64-6x",
        )
        .unwrap()
        .with_bytes(None, None, Some(829_000_000))
        .unwrap()
        .with_artifact_transfer_backend(backend)
        .unwrap()
        .with_artifact("app-host-product-a", Some(consumer), Some(300_000_000))
        .unwrap()
        .with_validity_inputs(
            &validity(&[(ValidityInputKind::ProductSchema, "app-host-v2")]).unwrap(),
        )
    }

    fn cmux_case_observations() -> Vec<VerificationObservation> {
        let mut observations = vec![rejected_resource_reuse_observation()];

        for (run, duration) in [
            ("run-1", 1_208_000),
            ("run-2", 1_115_000),
            ("run-3", 1_080_000),
        ] {
            observations.push(compile_observation(
                &format!("{run}-compile"),
                run,
                duration,
            ));
            observations.push(release_compile_observation(
                &format!("{run}-release-compile"),
                run,
                duration.saturating_add(240_000),
            ));
            observations.push(
                VerificationObservation::new(
                    &format!("{run}-package"),
                    run,
                    "cmux-ci",
                    "macos-full",
                    3,
                    VerificationStage::ArtifactPackaging,
                    8_900,
                    VerificationReuseClass::Cold,
                    SemanticValidationResult::Passed,
                    "macos-arm64-6x",
                )
                .unwrap()
                .with_bytes(None, Some(829_000_000), None)
                .unwrap()
                .with_artifact("app-host-product-a", None, None)
                .unwrap()
                .with_validity_inputs(
                    &validity(&[(ValidityInputKind::ProductSchema, "app-host-v2")]).unwrap(),
                ),
            );
            let transfer_backend = if run == "run-2" {
                "r2"
            } else {
                "github-artifact"
            };
            for shard in 1..=6 {
                observations.push(transfer_observation(
                    &format!("{run}-{transfer_backend}-transfer-{shard}"),
                    run,
                    3 + shard,
                    &format!("shard-{shard}"),
                    transfer_backend,
                ));
            }
            observations.push(
                VerificationObservation::new(
                    &format!("{run}-restore"),
                    run,
                    "cmux-ci",
                    "macos-full",
                    10,
                    VerificationStage::Restore,
                    10_000,
                    VerificationReuseClass::Reuse,
                    SemanticValidationResult::Passed,
                    "macos-arm64-6x",
                )
                .unwrap()
                .with_artifact("app-host-product-a", None, None)
                .unwrap()
                .with_validity_inputs(
                    &validity(&[(ValidityInputKind::ProductSchema, "app-host-v2")]).unwrap(),
                ),
            );

            for guard in 0..3 {
                observations.push(
                    VerificationObservation::new(
                        &format!("{run}-guard-{guard}"),
                        run,
                        "cmux-ci",
                        "macos-full",
                        u16::try_from(20 + guard).unwrap(),
                        VerificationStage::StaticGuard,
                        45_000 + u64::try_from(guard).unwrap() * 5_000,
                        VerificationReuseClass::Warm,
                        SemanticValidationResult::Passed,
                        "linux-guard",
                    )
                    .unwrap()
                    .with_independence("workflow-guards")
                    .unwrap(),
                );
            }

            observations.push(
                VerificationObservation::new(
                    &format!("{run}-irrelevant-macos"),
                    run,
                    "cmux-ci",
                    "macos-full",
                    30,
                    VerificationStage::CheckoutMaterialization,
                    29_000,
                    VerificationReuseClass::Warm,
                    SemanticValidationResult::Passed,
                    "macos-arm64-6x",
                )
                .unwrap()
                .with_platform("macos", false)
                .unwrap(),
            );
        }

        for run in ["focus-1", "focus-2", "focus-3"] {
            let mut compile = compile_observation(&format!("{run}-compile"), run, 1_080_000);
            compile.sequence = 1;
            observations.push(compile);
            observations.push(
                VerificationObservation::new(
                    &format!("{run}-test"),
                    run,
                    "cmux-ci",
                    "macos-full",
                    2,
                    VerificationStage::TestExecution,
                    30_000,
                    VerificationReuseClass::Warm,
                    SemanticValidationResult::Passed,
                    "macos-arm64-6x",
                )
                .unwrap()
                .with_test(VerificationTestScope::Focused, "focused-regressions", true)
                .unwrap(),
            );
        }

        for (index, result) in [
            SemanticValidationResult::TimedOut,
            SemanticValidationResult::Passed,
            SemanticValidationResult::TimedOut,
        ]
        .into_iter()
        .enumerate()
        {
            observations.push(
                VerificationObservation::new(
                    &format!("hang-{index}"),
                    &format!("hang-run-{index}"),
                    "cmux-ci",
                    "macos-full",
                    1,
                    VerificationStage::TestExecution,
                    900_000,
                    VerificationReuseClass::Warm,
                    result,
                    "macos-arm64-6x",
                )
                .unwrap()
                .with_test(VerificationTestScope::Full, "remote-tmux-suite", false)
                .unwrap(),
            );
        }

        observations
    }

    #[test]
    fn cmux_evidence_rediscovers_generic_optimization_classes() {
        let receipt = compile_verification_optimizations(
            "cmux-ci",
            "macos-full",
            &cmux_case_observations(),
            &[],
        )
        .unwrap();
        let classes = receipt
            .candidates()
            .iter()
            .map(OptimizationCandidate::class)
            .collect::<BTreeSet<_>>();

        assert!(classes.contains(&OptimizationClass::ReuseExactCompiledProduct));
        assert!(classes.contains(&OptimizationClass::RetainLocalImmutableArtifact));
        assert!(classes.contains(&OptimizationClass::SplitConsumerArtifact));
        assert!(classes.contains(&OptimizationClass::RunTestWithoutRebuild));
        assert!(classes.contains(&OptimizationClass::ParallelizeIndependentChecks));
        assert!(classes.contains(&OptimizationClass::IsolateFlakyOrHangingSuite));
        assert!(classes.contains(&OptimizationClass::SkipIrrelevantPlatformLane));
        assert!(
            receipt
                .candidates()
                .iter()
                .filter(
                    |candidate| candidate.class() == OptimizationClass::ReuseExactCompiledProduct
                )
                .count()
                >= 2
        );
        assert!(
            receipt
                .candidates()
                .iter()
                .filter(
                    |candidate| candidate.class() == OptimizationClass::ReuseExactCompiledProduct
                )
                .all(|candidate| {
                    candidate.reusable_state_class()
                        == Some(ReusableStateClass::ImmutableCompiledProduct)
                })
        );

        let transfer_backends = receipt
            .observations()
            .iter()
            .filter_map(|observation| observation.artifact_transfer_backend.as_deref())
            .collect::<BTreeSet<_>>();
        assert!(transfer_backends.contains("github-artifact"));
        assert!(transfer_backends.contains("r2"));
    }

    #[test]
    fn receipt_retains_bounded_evidence_deterministically() {
        let observations = cmux_case_observations();
        let first = compile_verification_optimizations("cmux-ci", "macos-full", &observations, &[])
            .unwrap();
        let mut reversed = observations.clone();
        reversed.reverse();
        let second =
            compile_verification_optimizations("cmux-ci", "macos-full", &reversed, &[]).unwrap();

        assert_eq!(first.observations().len(), observations.len());
        assert_eq!(first.experiments().len(), 0);
        assert_eq!(first.render_json().unwrap(), second.render_json().unwrap());
    }

    #[test]
    fn incomplete_reuse_validity_stays_advisory() {
        let observations = (0..3)
            .map(|index| {
                VerificationObservation::new(
                    &format!("compile-{index}"),
                    &format!("run-{index}"),
                    "project",
                    "profile",
                    1,
                    VerificationStage::Compile,
                    60_000,
                    VerificationReuseClass::Cold,
                    SemanticValidationResult::Passed,
                    "runner",
                )
                .unwrap()
                .with_source_toolchain(Some("tree-a"), Some("toolchain-a"))
                .unwrap()
            })
            .collect::<Vec<_>>();
        let receipt =
            compile_verification_optimizations("project", "profile", &observations, &[]).unwrap();
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
        assert!(candidate.validity().fingerprint().is_none());
        assert!(
            candidate
                .validity()
                .missing_inputs()
                .contains(&ValidityInputKind::Sdk)
        );
    }

    #[test]
    fn multiple_compatible_controlled_trials_promote_and_regression_demotes() {
        let observations = [
            compile_observation("compile-1", "run-1", 120_000),
            compile_observation("compile-2", "run-2", 121_000),
            compile_observation("compile-3", "run-3", 119_000),
        ];
        let proposal =
            compile_verification_optimizations("cmux-ci", "macos-full", &observations, &[])
                .unwrap();
        let candidate_id = proposal
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::ReuseExactCompiledProduct)
            .unwrap()
            .candidate_id()
            .to_owned();

        let accepted_trials = [
            OptimizationExperiment::new(
                "trial-1",
                "cmux-ci",
                "macos-full",
                OptimizationClass::ReuseExactCompiledProduct,
                Some("app-host-v2"),
                120_000,
                40_000,
                8_000,
                2_000,
                0,
                829_000_000,
                true,
                true,
                false,
                5_000,
                true,
                "receipt-a",
            )
            .unwrap()
            .bind_candidate(&candidate_id)
            .unwrap(),
            OptimizationExperiment::new(
                "trial-2",
                "cmux-ci",
                "macos-full",
                OptimizationClass::ReuseExactCompiledProduct,
                Some("app-host-v2"),
                118_000,
                42_000,
                8_000,
                2_000,
                0,
                829_000_000,
                true,
                true,
                false,
                5_000,
                true,
                "receipt-b",
            )
            .unwrap()
            .bind_candidate(&candidate_id)
            .unwrap(),
        ];
        let accepted = compile_verification_optimizations(
            "cmux-ci",
            "macos-full",
            &observations,
            &accepted_trials,
        )
        .unwrap();
        let candidate = accepted
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::ReuseExactCompiledProduct)
            .unwrap();
        assert_eq!(candidate.lifecycle(), OptimizationLifecycle::Accepted);

        let regression = OptimizationExperiment::new(
            "trial-regression",
            "cmux-ci",
            "macos-full",
            OptimizationClass::ReuseExactCompiledProduct,
            Some("app-host-v2"),
            120_000,
            121_000,
            0,
            0,
            0,
            829_000_000,
            true,
            true,
            false,
            2_000,
            true,
            "receipt-c",
        )
        .unwrap()
        .bind_candidate(&candidate_id)
        .unwrap();
        let demoted = compile_verification_optimizations(
            "cmux-ci",
            "macos-full",
            &observations,
            &[regression],
        )
        .unwrap();
        let candidate = demoted
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::ReuseExactCompiledProduct)
            .unwrap();
        assert_eq!(candidate.lifecycle(), OptimizationLifecycle::Demoted);
    }

    #[test]
    fn cmux_controlled_restore_is_experimenting_and_measured_resource_reuse_is_rejected() {
        let observations = cmux_case_observations();
        let proposal =
            compile_verification_optimizations("cmux-ci", "macos-full", &observations, &[])
                .unwrap();
        let app_host_candidate_id = proposal
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ReuseExactCompiledProduct
                    && candidate.subject_identity() == Some("app-host-v2")
            })
            .unwrap()
            .candidate_id()
            .to_owned();
        let resource_candidate_id = proposal
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ReuseExactCompiledProduct
                    && candidate.subject_identity() == Some("bundled-resources-v1")
            })
            .unwrap()
            .candidate_id()
            .to_owned();

        let experiments = [
            OptimizationExperiment::new(
                "cmux-13091-exact-restore",
                "cmux-ci",
                "macos-full",
                OptimizationClass::ReuseExactCompiledProduct,
                Some("app-host-v2"),
                2_481_000,
                392_000,
                0,
                0,
                0,
                1_000_000_000,
                true,
                true,
                false,
                30_000,
                true,
                "cmux-13091-35481043093",
            )
            .unwrap()
            .bind_candidate(&app_host_candidate_id)
            .unwrap(),
            OptimizationExperiment::new(
                "cmux-12985-resource-skip-regression",
                "cmux-ci",
                "macos-full",
                OptimizationClass::ReuseExactCompiledProduct,
                Some("bundled-resources-v1"),
                11_200,
                66_200,
                0,
                0,
                0,
                0,
                true,
                true,
                false,
                1_000,
                true,
                "cmux-12985-5746554064",
            )
            .unwrap()
            .bind_candidate(&resource_candidate_id)
            .unwrap(),
        ];
        let receipt = compile_verification_optimizations(
            "cmux-ci",
            "macos-full",
            &observations,
            &experiments,
        )
        .unwrap();
        let reuse = receipt
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ReuseExactCompiledProduct
                    && candidate.subject_identity() == Some("app-host-v2")
            })
            .unwrap();
        assert_eq!(reuse.lifecycle(), OptimizationLifecycle::Experimenting);

        let rejected = receipt
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ReuseExactCompiledProduct
                    && candidate.subject_identity() == Some("bundled-resources-v1")
            })
            .unwrap();
        assert_eq!(rejected.lifecycle(), OptimizationLifecycle::Demoted);
        assert_eq!(
            rejected.confidence(),
            OptimizationConfidence::RejectedEvidence
        );

        let split = receipt
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::SplitConsumerArtifact)
            .unwrap();
        assert_eq!(split.lifecycle(), OptimizationLifecycle::Candidate);
    }

    #[test]
    fn unbound_experiment_has_zero_promotion_authority() {
        let observations = cmux_case_observations();
        let unbound = OptimizationExperiment::new(
            "unbound-trial",
            "cmux-ci",
            "macos-full",
            OptimizationClass::ReuseExactCompiledProduct,
            None,
            2_000_000,
            1,
            0,
            0,
            0,
            0,
            true,
            true,
            false,
            1,
            true,
            "unbound-receipt",
        )
        .unwrap();

        let receipt =
            compile_verification_optimizations("cmux-ci", "macos-full", &observations, &[unbound])
                .unwrap();

        assert_eq!(receipt.experiments().len(), 1);
        assert!(receipt.experiments()[0].bound_candidate_id().is_none());
        assert!(
            receipt
                .candidates()
                .iter()
                .filter(
                    |candidate| candidate.class() == OptimizationClass::ReuseExactCompiledProduct
                )
                .all(|candidate| candidate.lifecycle() == OptimizationLifecycle::Candidate)
        );
    }

    #[test]
    fn repository_candidate_identity_survives_additional_evidence() {
        let observations = cmux_case_observations();
        let first = compile_verification_optimizations("cmux-ci", "macos-full", &observations, &[])
            .unwrap();
        let candidate = first
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ParallelizeIndependentChecks
                    && candidate.subject_identity() == Some("workflow-guards")
            })
            .unwrap();
        let candidate_id = candidate.candidate_id().to_owned();

        let experiment = OptimizationExperiment::new(
            "parallel-guards-controlled",
            "cmux-ci",
            "macos-full",
            OptimizationClass::ParallelizeIndependentChecks,
            Some("workflow-guards"),
            150_000,
            60_000,
            0,
            0,
            0,
            0,
            true,
            true,
            false,
            10_000,
            true,
            "parallel-guards-receipt",
        )
        .unwrap()
        .bind_candidate(&candidate_id)
        .unwrap();

        let mut extended = observations;
        extended.push(
            VerificationObservation::new(
                "run-4-guard-extra",
                "run-4",
                "cmux-ci",
                "macos-full",
                20,
                VerificationStage::StaticGuard,
                45_000,
                VerificationReuseClass::Warm,
                SemanticValidationResult::Passed,
                "linux-guard",
            )
            .unwrap()
            .with_independence("workflow-guards")
            .unwrap(),
        );

        let second =
            compile_verification_optimizations("cmux-ci", "macos-full", &extended, &[experiment])
                .unwrap();
        let candidate = second
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ParallelizeIndependentChecks
                    && candidate.subject_identity() == Some("workflow-guards")
            })
            .unwrap();

        assert_eq!(candidate.candidate_id(), candidate_id);
        assert_eq!(candidate.lifecycle(), OptimizationLifecycle::Experimenting);
    }

    #[test]
    fn contradictory_experiment_subject_has_zero_promotion_authority() {
        let observations = cmux_case_observations();
        let first = compile_verification_optimizations("cmux-ci", "macos-full", &observations, &[])
            .unwrap();
        let candidate = first
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ParallelizeIndependentChecks
                    && candidate.subject_identity() == Some("workflow-guards")
            })
            .unwrap();

        let experiment = OptimizationExperiment::new(
            "wrong-subject",
            "cmux-ci",
            "macos-full",
            OptimizationClass::ParallelizeIndependentChecks,
            Some("other-guards"),
            150_000,
            60_000,
            0,
            0,
            0,
            0,
            true,
            true,
            false,
            10_000,
            true,
            "wrong-subject-receipt",
        )
        .unwrap()
        .bind_candidate(candidate.candidate_id())
        .unwrap();

        let receipt = compile_verification_optimizations(
            "cmux-ci",
            "macos-full",
            &observations,
            &[experiment],
        )
        .unwrap();
        let candidate = receipt
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.class() == OptimizationClass::ParallelizeIndependentChecks
                    && candidate.subject_identity() == Some("workflow-guards")
            })
            .unwrap();

        assert_eq!(candidate.lifecycle(), OptimizationLifecycle::Candidate);
    }

    #[test]
    fn anonymous_late_guards_do_not_coalesce() {
        let mut observations = Vec::new();
        for run in ["guard-a", "guard-b"] {
            observations.push(
                VerificationObservation::new(
                    &format!("{run}-compile"),
                    run,
                    "project",
                    "profile",
                    1,
                    VerificationStage::Compile,
                    60_000,
                    VerificationReuseClass::Cold,
                    SemanticValidationResult::Passed,
                    "linux",
                )
                .unwrap(),
            );
            observations.push(
                VerificationObservation::new(
                    &format!("{run}-guard"),
                    run,
                    "project",
                    "profile",
                    2,
                    VerificationStage::StaticGuard,
                    1_000,
                    VerificationReuseClass::Warm,
                    SemanticValidationResult::Failed,
                    "linux",
                )
                .unwrap(),
            );
        }

        let receipt =
            compile_verification_optimizations("project", "profile", &observations, &[]).unwrap();

        assert!(
            receipt
                .candidates()
                .iter()
                .all(|candidate| candidate.class() != OptimizationClass::MoveStaticGuardEarlier)
        );
    }

    #[test]
    fn aggregated_transfer_samples_count_toward_split_candidate_evidence() {
        let transfer = VerificationObservation::new(
            "aggregate-transfer",
            "aggregate-run",
            "project",
            "profile",
            1,
            VerificationStage::ArtifactTransfer,
            60_000,
            VerificationReuseClass::Cold,
            SemanticValidationResult::Passed,
            "macos-arm64",
        )
        .unwrap()
        .with_sample_count(6)
        .unwrap()
        .with_bytes(None, None, Some(850_000_000))
        .unwrap()
        .with_artifact(
            "app-host-product",
            Some("app-host-shards"),
            Some(300_000_000),
        )
        .unwrap()
        .with_validity_inputs(&[ValidityInput::new(
            ValidityInputKind::ProductSchema,
            "app-host-v2",
        )
        .unwrap()]);

        let receipt =
            compile_verification_optimizations("project", "profile", &[transfer], &[]).unwrap();

        let split = receipt
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::SplitConsumerArtifact)
            .unwrap();
        assert_eq!(split.lifecycle(), OptimizationLifecycle::Candidate);
        assert_eq!(split.evidence()[0].sample_count, 6);
    }

    #[test]
    fn aggregated_timeout_samples_count_toward_hanging_suite_evidence() {
        let timeout = VerificationObservation::new(
            "timeouts",
            "aggregate-timeouts",
            "project",
            "profile",
            1,
            VerificationStage::TestExecution,
            60_000,
            VerificationReuseClass::Warm,
            SemanticValidationResult::TimedOut,
            "linux",
        )
        .unwrap()
        .with_sample_count(3)
        .unwrap()
        .with_test(VerificationTestScope::Full, "flaky-suite", false)
        .unwrap();

        let receipt =
            compile_verification_optimizations("project", "profile", &[timeout], &[]).unwrap();

        assert!(receipt.candidates().iter().any(|candidate| {
            candidate.class() == OptimizationClass::IsolateFlakyOrHangingSuite
                && candidate.subject_identity() == Some("flaky-suite")
        }));
    }

    #[test]
    fn human_and_json_are_derived_from_the_same_receipt() {
        let receipt = compile_verification_optimizations(
            "cmux-ci",
            "macos-full",
            &cmux_case_observations(),
            &[],
        )
        .unwrap();
        let human = receipt.render_human();
        let json = receipt.render_json().unwrap();

        assert!(human.contains("candidate: reuse_exact_compiled_product"));
        assert!(human.contains("restore/transfer overhead:"));
        assert!(human.contains("missing utility inputs:"));
        assert!(human.contains("reusable state class: immutable_compiled_product"));
        assert!(json.contains("\"document_type\": \"adaptive_verification_compiler_receipt\""));
        assert!(json.contains("\"validity\""));
        assert!(json.contains("\"reusable_state_class\": \"immutable_compiled_product\""));
        assert!(json.contains("\"missing_cost_inputs\""));
    }

    #[test]
    fn mixed_scope_and_duplicate_observations_fail_closed() {
        let foreign = VerificationObservation::new(
            "obs",
            "run",
            "other",
            "profile",
            1,
            VerificationStage::Compile,
            10_000,
            VerificationReuseClass::Cold,
            SemanticValidationResult::Passed,
            "runner",
        )
        .unwrap();
        let mismatch =
            compile_verification_optimizations("project", "profile", &[foreign], &[]).unwrap_err();
        assert_eq!(mismatch.code, "verification_observation_scope_mismatch");

        let observation = VerificationObservation::new(
            "same",
            "run",
            "project",
            "profile",
            1,
            VerificationStage::Compile,
            10_000,
            VerificationReuseClass::Cold,
            SemanticValidationResult::Passed,
            "runner",
        )
        .unwrap();
        let duplicate = compile_verification_optimizations(
            "project",
            "profile",
            &[observation.clone(), observation],
            &[],
        )
        .unwrap_err();
        assert_eq!(duplicate.code, "duplicate_verification_observation");
    }
}
