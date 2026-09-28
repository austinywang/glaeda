use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

use crate::reusable_state_lifecycle::ReusableStateIntegrityState;

pub const FAILURE_DIAGNOSTIC_SCHEMA_VERSION: u8 = 1;
pub const MAX_FAILURE_STEP_BYTES: usize = 96;
pub const MAX_TEST_SELECTOR_BYTES: usize = 160;
pub const MAX_CANONICAL_EVIDENCE_REF_BYTES: usize = 240;
pub const MAX_FAILURE_SIGNATURES: usize = 16;
pub const MAX_FAILURE_HISTORY_INCIDENTS: usize = 10_000;
pub const MAX_PREFLIGHT_OUTCOMES: usize = 10_000;
pub const MAX_FAILURE_ELAPSED_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;
pub const MIN_PREFLIGHT_OCCURRENCES: u64 = 3;
pub const PREFLIGHT_EARLIER_RATIO: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    SourceCompileError,
    TestAssertion,
    TestTimeout,
    TestHostCrash,
    ProcessSettlement,
    NetworkDns,
    NetworkProvider,
    DependencyResolution,
    CacheCorruption,
    ArtifactMissing,
    ArtifactCorruption,
    ToolchainMismatch,
    ResourceExhaustion,
    RunnerCancellation,
    WorkflowRouting,
    CredentialAuth,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticConfidence {
    Exact,
    Known,
    Likely,
    ProbeRequired,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureExitClass {
    NonZero,
    TimedOut,
    Cancelled,
    Signalled,
    InfrastructureRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePhase {
    Checkout,
    SourceCompile,
    Test,
    TestHost,
    ProcessSettlement,
    DependencyResolution,
    Cache,
    Artifact,
    Package,
    Toolchain,
    Preflight,
    WorkflowRouting,
    Credential,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceScope {
    BranchSpecific,
    Mainline,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCommandStatus {
    Executed,
    NotExecuted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownFailureSignature {
    DnsResolutionFailed,
    NetworkProviderUnavailable,
    SwiftWarningBudgetExceeded,
    TestAssertionFailed,
    AppHostCrashOrRestart,
    DetachedChildPipeHeldOpen,
    IndividualTestTimeout,
    ArtifactMissing,
    ArtifactChecksumMismatch,
    SwiftPmCacheCollision,
    SwiftPmCacheCorruption,
    DependencyResolutionFailure,
    PackagedHelperMissing,
    ModuleImportMissing,
    ToolchainMismatch,
    ResourceExhaustion,
    WorkflowRoutingFailure,
    CredentialAuthFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSettlementResult {
    Settled,
    DescendantPipeHeldOpen,
    HostCrashedOrRestarted,
    TimedOut,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePressureSummary {
    Normal,
    MemoryExhausted,
    DiskExhausted,
    ProcessExhausted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactCacheReceipt {
    Present,
    ArtifactMissing,
    ArtifactCorrupt,
    CacheCollision,
    CacheCorrupt,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkDiagnosticClass {
    DnsResolutionFailed,
    ProviderUnavailable,
    Healthy,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestVerdict {
    AssertionFailed,
    TimedOut,
    HostCrashed,
    KnownFlakyOnMain,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolchainIdentityResult {
    Match,
    Mismatch,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrerequisiteVerdict {
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureEvidence {
    exit_class: FailureExitClass,
    phase: FailurePhase,
    source_scope: SourceScope,
    failed_step: String,
    elapsed_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_selector: Option<String>,
    signatures: Vec<KnownFailureSignature>,
    process_settlement: ProcessSettlementResult,
    resource_pressure: ResourcePressureSummary,
    artifact_cache: ArtifactCacheReceipt,
    network: NetworkDiagnosticClass,
    test_verdict: TestVerdict,
    toolchain: ToolchainIdentityResult,
    prerequisite: PrerequisiteVerdict,
    repository_command_status: RepositoryCommandStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical_evidence_ref: Option<String>,
}

impl FailureEvidence {
    /// Create one bounded failure-evidence packet.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/oversized step label or an elapsed time beyond the accepted
    /// diagnostic window.
    pub fn new(
        exit_class: FailureExitClass,
        phase: FailurePhase,
        failed_step: impl Into<String>,
        elapsed_millis: u64,
    ) -> Result<Self, FailureDiagnosticError> {
        let failed_step = failed_step.into();
        validate_text(&failed_step, MAX_FAILURE_STEP_BYTES, "failed step")?;
        if elapsed_millis > MAX_FAILURE_ELAPSED_MILLIS {
            return Err(error(
                "failure elapsed time exceeds the accepted diagnostic window",
            ));
        }
        Ok(Self {
            exit_class,
            phase,
            source_scope: SourceScope::Unknown,
            failed_step,
            elapsed_millis,
            test_selector: None,
            signatures: Vec::new(),
            process_settlement: ProcessSettlementResult::Unknown,
            resource_pressure: ResourcePressureSummary::Unknown,
            artifact_cache: ArtifactCacheReceipt::Unknown,
            network: NetworkDiagnosticClass::Unknown,
            test_verdict: TestVerdict::Unknown,
            toolchain: ToolchainIdentityResult::Unknown,
            prerequisite: PrerequisiteVerdict::Unknown,
            repository_command_status: RepositoryCommandStatus::Unknown,
            canonical_evidence_ref: None,
        })
    }

    #[must_use]
    pub fn with_source_scope(mut self, value: SourceScope) -> Self {
        self.source_scope = value;
        self
    }

    /// Add one reviewed stderr/log signature identifier without retaining the source log.
    ///
    /// # Errors
    ///
    /// Returns an error after the bounded signature limit or for duplicate signatures.
    pub fn with_signature(
        mut self,
        value: KnownFailureSignature,
    ) -> Result<Self, FailureDiagnosticError> {
        if self.signatures.len() >= MAX_FAILURE_SIGNATURES {
            return Err(error("failure signatures exceed the accepted item limit"));
        }
        if self.signatures.contains(&value) {
            return Err(error("failure signatures contain a duplicate item"));
        }
        self.signatures.push(value);
        self.signatures.sort_unstable();
        Ok(self)
    }

    #[must_use]
    pub fn with_process_settlement(mut self, value: ProcessSettlementResult) -> Self {
        self.process_settlement = value;
        self
    }

    #[must_use]
    pub fn with_resource_pressure(mut self, value: ResourcePressureSummary) -> Self {
        self.resource_pressure = value;
        self
    }

    #[must_use]
    pub fn with_artifact_cache(mut self, value: ArtifactCacheReceipt) -> Self {
        self.artifact_cache = value;
        self
    }

    /// Consume #21's typed reusable-state integrity result without interpreting cache paths or logs.
    #[must_use]
    pub fn with_reusable_state_integrity(mut self, value: ReusableStateIntegrityState) -> Self {
        self.artifact_cache = match value {
            ReusableStateIntegrityState::Verified => ArtifactCacheReceipt::Present,
            ReusableStateIntegrityState::Truncated | ReusableStateIntegrityState::Corrupt => {
                ArtifactCacheReceipt::CacheCorrupt
            }
        };
        self
    }

    #[must_use]
    pub fn with_network(mut self, value: NetworkDiagnosticClass) -> Self {
        self.network = value;
        self
    }

    #[must_use]
    pub fn with_test_verdict(mut self, value: TestVerdict) -> Self {
        self.test_verdict = value;
        self
    }

    /// Attach one bounded test selector without retaining test output.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, control-bearing selector text.
    pub fn with_test_selector(
        mut self,
        value: impl Into<String>,
    ) -> Result<Self, FailureDiagnosticError> {
        let value = value.into();
        validate_text(&value, MAX_TEST_SELECTOR_BYTES, "test selector")?;
        self.test_selector = Some(value);
        Ok(self)
    }

    #[must_use]
    pub fn with_toolchain(mut self, value: ToolchainIdentityResult) -> Self {
        self.toolchain = value;
        self
    }

    #[must_use]
    pub fn with_prerequisite(mut self, value: PrerequisiteVerdict) -> Self {
        self.prerequisite = value;
        self
    }

    #[must_use]
    pub fn with_repository_command_status(mut self, value: RepositoryCommandStatus) -> Self {
        self.repository_command_status = value;
        self
    }

    /// Retain a bounded canonical log/artifact reference for deeper inspection.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, control-bearing reference text.
    pub fn with_canonical_evidence_ref(
        mut self,
        value: impl Into<String>,
    ) -> Result<Self, FailureDiagnosticError> {
        let value = value.into();
        validate_text(
            &value,
            MAX_CANONICAL_EVIDENCE_REF_BYTES,
            "canonical evidence reference",
        )?;
        self.canonical_evidence_ref = Some(value);
        Ok(self)
    }

    #[must_use]
    pub const fn exit_class(&self) -> FailureExitClass {
        self.exit_class
    }

    #[must_use]
    pub const fn phase(&self) -> FailurePhase {
        self.phase
    }

    #[must_use]
    pub const fn source_scope(&self) -> SourceScope {
        self.source_scope
    }

    #[must_use]
    pub fn failed_step(&self) -> &str {
        &self.failed_step
    }

    #[must_use]
    pub const fn elapsed_millis(&self) -> u64 {
        self.elapsed_millis
    }

    #[must_use]
    pub fn test_selector(&self) -> Option<&str> {
        self.test_selector.as_deref()
    }

    #[must_use]
    pub fn signatures(&self) -> &[KnownFailureSignature] {
        &self.signatures
    }

    #[must_use]
    pub const fn process_settlement(&self) -> ProcessSettlementResult {
        self.process_settlement
    }

    #[must_use]
    pub const fn resource_pressure(&self) -> ResourcePressureSummary {
        self.resource_pressure
    }

    #[must_use]
    pub const fn artifact_cache(&self) -> ArtifactCacheReceipt {
        self.artifact_cache
    }

    #[must_use]
    pub const fn network(&self) -> NetworkDiagnosticClass {
        self.network
    }

    #[must_use]
    pub const fn test_verdict(&self) -> TestVerdict {
        self.test_verdict
    }

    #[must_use]
    pub const fn toolchain(&self) -> ToolchainIdentityResult {
        self.toolchain
    }

    #[must_use]
    pub const fn prerequisite(&self) -> PrerequisiteVerdict {
        self.prerequisite
    }

    #[must_use]
    pub const fn repository_command_status(&self) -> RepositoryCommandStatus {
        self.repository_command_status
    }

    #[must_use]
    pub fn canonical_evidence_ref(&self) -> Option<&str> {
        self.canonical_evidence_ref.as_deref()
    }

    fn has_signature(&self, value: KnownFailureSignature) -> bool {
        self.signatures.binary_search(&value).is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticProbe {
    ResolveAndHttpsHead,
    ProviderMetadataLookup,
    ResolverCacheIdentity,
    UnfinishedSelectorProcessTree,
    ExactToolchainIdentity,
    ResourcePressureSnapshot,
    WorkflowDependencyStatus,
    ArtifactChecksum,
    CredentialIdentityStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticProbeResult {
    DnsFailureConfirmed,
    DnsHealthy,
    NetworkProviderFailureConfirmed,
    NetworkReachable,
    ProviderMissingConfirmed,
    ProviderPresent,
    CacheIdentityCollisionConfirmed,
    CacheIdentityClean,
    UnfinishedSelectorPipeOwnerConfirmed,
    ProcessTreeClean,
    ToolchainMismatchConfirmed,
    ToolchainMatch,
    WorkflowDependencyCancelledConfirmed,
    WorkflowDependencyHealthy,
    ArtifactCorruptionConfirmed,
    ArtifactIntegrityValid,
    ResourceExhaustionConfirmed,
    ResourcePressureNormal,
    CredentialFailureConfirmed,
    CredentialValid,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProbeObservation {
    probe: DiagnosticProbe,
    result: DiagnosticProbeResult,
}

impl ProbeObservation {
    #[must_use]
    pub const fn new(probe: DiagnosticProbe, result: DiagnosticProbeResult) -> Self {
        Self { probe, result }
    }

    #[must_use]
    pub const fn probe(&self) -> DiagnosticProbe {
        self.probe
    }

    #[must_use]
    pub const fn result(&self) -> DiagnosticProbeResult {
        self.result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemedyClass {
    RetrySame,
    RetryFreshRunner,
    ClearSpecificReconstructibleCache,
    RebuildProduct,
    ReResolveDependency,
    RerouteHosted,
    RebaseRefresh,
    QuarantineSuite,
    FixSourceRequired,
    OperatorAdminRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRepairDisposition {
    NoneIndicated,
    Required,
    Undetermined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportingObservation {
    CheckoutFailed,
    CompileStepFailed,
    TestStepFailed,
    PackageStepFailed,
    RunnerCancelled,
    PrerequisiteCancelled,
    DnsResolutionFailed,
    NetworkProviderUnavailable,
    NoRepositoryCommandExecuted,
    SwiftWarningBudgetExceeded,
    TestAssertionFailed,
    KnownMainlineFlake,
    IndividualTestTimedOut,
    AppHostCrashedOrRestarted,
    DetachedChildPipeHeldOpen,
    ArtifactReportedMissing,
    ArtifactReportedCorrupt,
    CacheCollisionReported,
    DependencyResolutionFailed,
    PackagedHelperMissing,
    ModuleImportMissing,
    ToolchainMismatch,
    ResourceExhaustion,
    WorkflowRoutingFailed,
    CredentialAuthFailed,
    BranchSpecificFailure,
    MainlineFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureDiagnosis {
    schema_version: u8,
    failure_class: FailureClass,
    confidence: DiagnosticConfidence,
    failed_step: String,
    elapsed_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_selector: Option<String>,
    observations: Vec<SupportingObservation>,
    alternatives: Vec<FailureClass>,
    next_probe: Option<DiagnosticProbe>,
    probe_performed: Option<DiagnosticProbe>,
    probe_result: Option<DiagnosticProbeResult>,
    recommended_remedy: Option<RemedyClass>,
    source_repair: SourceRepairDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical_evidence_ref: Option<String>,
    authorizes_mutation: bool,
}

impl FailureDiagnosis {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        self.failure_class
    }

    #[must_use]
    pub const fn confidence(&self) -> DiagnosticConfidence {
        self.confidence
    }

    #[must_use]
    pub fn failed_step(&self) -> &str {
        &self.failed_step
    }

    #[must_use]
    pub const fn elapsed_millis(&self) -> u64 {
        self.elapsed_millis
    }

    #[must_use]
    pub fn test_selector(&self) -> Option<&str> {
        self.test_selector.as_deref()
    }

    #[must_use]
    pub fn observations(&self) -> &[SupportingObservation] {
        &self.observations
    }

    #[must_use]
    pub fn alternatives(&self) -> &[FailureClass] {
        &self.alternatives
    }

    #[must_use]
    pub const fn next_probe(&self) -> Option<DiagnosticProbe> {
        self.next_probe
    }

    #[must_use]
    pub const fn probe_performed(&self) -> Option<DiagnosticProbe> {
        self.probe_performed
    }

    #[must_use]
    pub const fn probe_result(&self) -> Option<DiagnosticProbeResult> {
        self.probe_result
    }

    #[must_use]
    pub const fn recommended_remedy(&self) -> Option<RemedyClass> {
        self.recommended_remedy
    }

    #[must_use]
    pub const fn source_repair(&self) -> SourceRepairDisposition {
        self.source_repair
    }

    #[must_use]
    pub fn canonical_evidence_ref(&self) -> Option<&str> {
        self.canonical_evidence_ref.as_deref()
    }

    #[must_use]
    pub const fn authorizes_mutation(&self) -> bool {
        self.authorizes_mutation
    }

    #[must_use]
    pub fn render_human(&self) -> String {
        let mut output = String::new();
        use std::fmt::Write as _;
        let _ = writeln!(
            output,
            "failure class: {}",
            failure_class_name(self.failure_class)
        );
        let _ = writeln!(output, "confidence: {}", confidence_name(self.confidence));
        output.push_str("evidence:\n");
        let _ = writeln!(output, "  failed step: {}", self.failed_step);
        let _ = writeln!(output, "  elapsed millis: {}", self.elapsed_millis);
        if let Some(selector) = &self.test_selector {
            let _ = writeln!(output, "  test selector: {selector}");
        }
        for observation in &self.observations {
            let _ = writeln!(output, "  {}", observation_name(*observation));
        }
        if !self.alternatives.is_empty() {
            output.push_str("alternative plausible classes:\n");
            for alternative in &self.alternatives {
                let _ = writeln!(output, "  {}", failure_class_name(*alternative));
            }
        }
        match self.probe_performed {
            Some(probe) => {
                let _ = writeln!(output, "probe performed: {}", probe_name(probe));
            }
            None => output.push_str("probe performed: none\n"),
        }
        match self.probe_result {
            Some(result) => {
                let _ = writeln!(output, "probe result: {}", probe_result_name(result));
            }
            None if self.next_probe.is_some() => output.push_str("probe result: pending\n"),
            None => output.push_str("probe result: none\n"),
        }
        if let Some(probe) = self.next_probe {
            let _ = writeln!(output, "next probe: {}", probe_name(probe));
        }
        match self.recommended_remedy {
            Some(remedy) => {
                let _ = writeln!(output, "next action: {}", remedy_name(remedy));
            }
            None if self.next_probe.is_some() => {
                output.push_str("next action: perform the selected read-only probe\n");
            }
            None => {
                output.push_str(
                    "next action: inspect canonical evidence or gather one bounded observation\n",
                );
            }
        }
        let _ = writeln!(
            output,
            "source repair: {}",
            source_repair_name(self.source_repair)
        );
        if let Some(reference) = &self.canonical_evidence_ref {
            let _ = writeln!(output, "evidence ref: {reference}");
        }
        output
    }
}

pub const fn allowed_probes(class: FailureClass) -> &'static [DiagnosticProbe] {
    use DiagnosticProbe as Probe;
    match class {
        FailureClass::SourceCompileError => &[Probe::ExactToolchainIdentity],
        FailureClass::TestAssertion => &[],
        FailureClass::TestTimeout
        | FailureClass::TestHostCrash
        | FailureClass::ProcessSettlement => &[Probe::UnfinishedSelectorProcessTree],
        FailureClass::NetworkDns => &[Probe::ResolveAndHttpsHead],
        FailureClass::NetworkProvider => &[Probe::ResolveAndHttpsHead],
        FailureClass::DependencyResolution | FailureClass::CacheCorruption => {
            &[Probe::ResolverCacheIdentity]
        }
        FailureClass::ArtifactMissing => &[Probe::ProviderMetadataLookup],
        FailureClass::ArtifactCorruption => &[Probe::ArtifactChecksum],
        FailureClass::ToolchainMismatch => &[Probe::ExactToolchainIdentity],
        FailureClass::ResourceExhaustion => &[Probe::ResourcePressureSnapshot],
        FailureClass::RunnerCancellation | FailureClass::WorkflowRouting => {
            &[Probe::WorkflowDependencyStatus]
        }
        FailureClass::CredentialAuth => &[Probe::CredentialIdentityStatus],
        FailureClass::Unknown => &[],
    }
}

/// Classify one bounded failure packet and optionally consume one allowed read-only probe result.
///
/// The function performs no command execution and grants no repair authority.
///
/// # Errors
///
/// Returns an error when a supplied probe is outside the small allowed set for the provisional
/// failure class.
pub fn diagnose_failure(
    evidence: &FailureEvidence,
    probe: Option<ProbeObservation>,
) -> Result<FailureDiagnosis, FailureDiagnosticError> {
    let provisional = classify_raw(evidence);
    if let Some(probe_observation) = probe {
        let allowed = allowed_probes(provisional.0);
        let phase_probe = if provisional.0 == FailureClass::Unknown {
            cheapest_phase_probe(evidence.phase)
        } else {
            None
        };
        if !allowed.contains(&probe_observation.probe)
            && phase_probe != Some(probe_observation.probe)
        {
            return Err(error(
                "probe is outside the allowed set for this failure evidence",
            ));
        }
        if !probe_result_allowed(probe_observation.probe, probe_observation.result) {
            return Err(error("probe result is invalid for the selected probe"));
        }
    }

    let (failure_class, confidence) = classify_with_probe(evidence, probe, provisional);
    let observations = supporting_observations(evidence);
    let alternatives = if confidence == DiagnosticConfidence::Exact {
        Vec::new()
    } else {
        alternatives(failure_class, evidence)
            .into_iter()
            .filter(|candidate| {
                probe.is_none_or(|observation| !probe_contradicts(*candidate, observation.result))
            })
            .collect()
    };
    let next_probe = if probe.is_none() {
        cheapest_discriminating_probe(failure_class, evidence)
    } else if confidence == DiagnosticConfidence::Unknown
        || confidence == DiagnosticConfidence::ProbeRequired
    {
        cheapest_discriminating_probe(failure_class, evidence)
            .filter(|candidate| Some(*candidate) != probe.map(|value| value.probe))
    } else {
        None
    };
    let recommended_remedy = recommended_remedy(failure_class, evidence);
    let source_repair = source_repair_disposition(failure_class, evidence);

    Ok(FailureDiagnosis {
        schema_version: FAILURE_DIAGNOSTIC_SCHEMA_VERSION,
        failure_class,
        confidence,
        failed_step: evidence.failed_step.clone(),
        elapsed_millis: evidence.elapsed_millis,
        test_selector: evidence.test_selector.clone(),
        observations,
        alternatives,
        next_probe,
        probe_performed: probe.map(|value| value.probe),
        probe_result: probe.map(|value| value.result),
        recommended_remedy,
        source_repair,
        canonical_evidence_ref: evidence.canonical_evidence_ref.clone(),
        authorizes_mutation: false,
    })
}

fn classify_raw(evidence: &FailureEvidence) -> (FailureClass, DiagnosticConfidence) {
    use DiagnosticConfidence as Confidence;
    use FailureClass as Class;
    use KnownFailureSignature as Signature;

    if evidence.exit_class == FailureExitClass::Cancelled
        || evidence.prerequisite == PrerequisiteVerdict::Cancelled
    {
        return (Class::RunnerCancellation, Confidence::Exact);
    }
    if evidence.network == NetworkDiagnosticClass::DnsResolutionFailed {
        return (Class::NetworkDns, Confidence::Exact);
    }
    if evidence.has_signature(Signature::DnsResolutionFailed) {
        return (Class::NetworkDns, Confidence::Known);
    }
    if evidence.network == NetworkDiagnosticClass::ProviderUnavailable
        || evidence.has_signature(Signature::NetworkProviderUnavailable)
    {
        return (Class::NetworkProvider, Confidence::Known);
    }
    if evidence.has_signature(Signature::CredentialAuthFailure) {
        return (Class::CredentialAuth, Confidence::Known);
    }
    if evidence.artifact_cache == ArtifactCacheReceipt::ArtifactCorrupt
        || evidence.has_signature(Signature::ArtifactChecksumMismatch)
    {
        return (Class::ArtifactCorruption, Confidence::Known);
    }
    if evidence.artifact_cache == ArtifactCacheReceipt::ArtifactMissing
        || evidence.has_signature(Signature::ArtifactMissing)
        || evidence.has_signature(Signature::PackagedHelperMissing)
        || evidence.has_signature(Signature::ModuleImportMissing)
    {
        return (Class::ArtifactMissing, Confidence::Known);
    }
    if matches!(
        evidence.artifact_cache,
        ArtifactCacheReceipt::CacheCollision | ArtifactCacheReceipt::CacheCorrupt
    ) || evidence.has_signature(Signature::SwiftPmCacheCollision)
        || evidence.has_signature(Signature::SwiftPmCacheCorruption)
    {
        return (Class::CacheCorruption, Confidence::Known);
    }
    if evidence.has_signature(Signature::DependencyResolutionFailure) {
        return (Class::DependencyResolution, Confidence::Known);
    }
    if evidence.toolchain == ToolchainIdentityResult::Mismatch
        || evidence.has_signature(Signature::ToolchainMismatch)
    {
        return (Class::ToolchainMismatch, Confidence::Known);
    }
    if (evidence.resource_pressure != ResourcePressureSummary::Unknown
        && evidence.resource_pressure != ResourcePressureSummary::Normal)
        || evidence.has_signature(Signature::ResourceExhaustion)
    {
        return (Class::ResourceExhaustion, Confidence::Known);
    }
    if evidence.process_settlement == ProcessSettlementResult::HostCrashedOrRestarted
        || evidence.test_verdict == TestVerdict::HostCrashed
        || evidence.has_signature(Signature::AppHostCrashOrRestart)
    {
        return (Class::TestHostCrash, Confidence::Known);
    }
    if evidence.process_settlement == ProcessSettlementResult::DescendantPipeHeldOpen
        || evidence.has_signature(Signature::DetachedChildPipeHeldOpen)
    {
        return (Class::ProcessSettlement, Confidence::Known);
    }
    if evidence.test_verdict == TestVerdict::TimedOut
        || evidence.has_signature(Signature::IndividualTestTimeout)
        || (evidence.exit_class == FailureExitClass::TimedOut
            && evidence.phase == FailurePhase::Test)
    {
        return (Class::TestTimeout, Confidence::Known);
    }
    if matches!(
        evidence.test_verdict,
        TestVerdict::AssertionFailed | TestVerdict::KnownFlakyOnMain
    ) || evidence.has_signature(Signature::TestAssertionFailed)
    {
        return (Class::TestAssertion, Confidence::Known);
    }
    if evidence.has_signature(Signature::SwiftWarningBudgetExceeded) {
        return (Class::SourceCompileError, Confidence::Known);
    }
    if evidence.has_signature(Signature::WorkflowRoutingFailure) {
        return (Class::WorkflowRouting, Confidence::Known);
    }

    (Class::Unknown, Confidence::Unknown)
}

const fn probe_result_allowed(probe: DiagnosticProbe, result: DiagnosticProbeResult) -> bool {
    use DiagnosticProbe as Probe;
    use DiagnosticProbeResult as Result;
    match probe {
        Probe::ResolveAndHttpsHead => matches!(
            result,
            Result::DnsFailureConfirmed
                | Result::DnsHealthy
                | Result::NetworkProviderFailureConfirmed
                | Result::NetworkReachable
                | Result::Inconclusive
        ),
        Probe::ProviderMetadataLookup => matches!(
            result,
            Result::ProviderMissingConfirmed | Result::ProviderPresent | Result::Inconclusive
        ),
        Probe::ResolverCacheIdentity => matches!(
            result,
            Result::CacheIdentityCollisionConfirmed
                | Result::CacheIdentityClean
                | Result::Inconclusive
        ),
        Probe::UnfinishedSelectorProcessTree => matches!(
            result,
            Result::UnfinishedSelectorPipeOwnerConfirmed
                | Result::ProcessTreeClean
                | Result::Inconclusive
        ),
        Probe::ExactToolchainIdentity => matches!(
            result,
            Result::ToolchainMismatchConfirmed | Result::ToolchainMatch | Result::Inconclusive
        ),
        Probe::ResourcePressureSnapshot => matches!(
            result,
            Result::ResourceExhaustionConfirmed
                | Result::ResourcePressureNormal
                | Result::Inconclusive
        ),
        Probe::WorkflowDependencyStatus => matches!(
            result,
            Result::WorkflowDependencyCancelledConfirmed
                | Result::WorkflowDependencyHealthy
                | Result::Inconclusive
        ),
        Probe::ArtifactChecksum => matches!(
            result,
            Result::ArtifactCorruptionConfirmed
                | Result::ArtifactIntegrityValid
                | Result::Inconclusive
        ),
        Probe::CredentialIdentityStatus => matches!(
            result,
            Result::CredentialFailureConfirmed | Result::CredentialValid | Result::Inconclusive
        ),
    }
}

fn probe_contradicts(class: FailureClass, result: DiagnosticProbeResult) -> bool {
    use DiagnosticProbeResult as Result;
    use FailureClass as Class;
    matches!(
        (class, result),
        (
            Class::NetworkDns,
            Result::DnsHealthy | Result::NetworkReachable
        ) | (Class::NetworkProvider, Result::NetworkReachable)
            | (Class::ArtifactMissing, Result::ProviderPresent)
            | (Class::CacheCorruption, Result::CacheIdentityClean)
            | (Class::ProcessSettlement, Result::ProcessTreeClean)
            | (Class::ToolchainMismatch, Result::ToolchainMatch)
            | (Class::ResourceExhaustion, Result::ResourcePressureNormal)
            | (Class::RunnerCancellation, Result::WorkflowDependencyHealthy)
            | (Class::ArtifactCorruption, Result::ArtifactIntegrityValid)
            | (Class::CredentialAuth, Result::CredentialValid)
    )
}

fn classify_with_probe(
    evidence: &FailureEvidence,
    probe: Option<ProbeObservation>,
    provisional: (FailureClass, DiagnosticConfidence),
) -> (FailureClass, DiagnosticConfidence) {
    let Some(probe) = probe else {
        return provisional;
    };
    use DiagnosticConfidence as Confidence;
    use DiagnosticProbeResult as Result;
    use FailureClass as Class;

    if probe_contradicts(provisional.0, probe.result) {
        return (Class::Unknown, Confidence::ProbeRequired);
    }

    match probe.result {
        Result::DnsFailureConfirmed => (Class::NetworkDns, Confidence::Exact),
        Result::NetworkProviderFailureConfirmed => (Class::NetworkProvider, Confidence::Exact),
        Result::ProviderMissingConfirmed => (Class::ArtifactMissing, Confidence::Exact),
        Result::CacheIdentityCollisionConfirmed => (Class::CacheCorruption, Confidence::Exact),
        Result::UnfinishedSelectorPipeOwnerConfirmed => {
            if evidence.phase == FailurePhase::TestHost {
                (Class::TestHostCrash, Confidence::Exact)
            } else {
                (Class::ProcessSettlement, Confidence::Exact)
            }
        }
        Result::ToolchainMismatchConfirmed => (Class::ToolchainMismatch, Confidence::Exact),
        Result::WorkflowDependencyCancelledConfirmed => {
            (Class::RunnerCancellation, Confidence::Exact)
        }
        Result::ArtifactCorruptionConfirmed => (Class::ArtifactCorruption, Confidence::Exact),
        Result::ResourceExhaustionConfirmed => (Class::ResourceExhaustion, Confidence::Exact),
        Result::CredentialFailureConfirmed => (Class::CredentialAuth, Confidence::Exact),
        Result::DnsHealthy
        | Result::NetworkReachable
        | Result::ProviderPresent
        | Result::CacheIdentityClean
        | Result::ProcessTreeClean
        | Result::ToolchainMatch
        | Result::WorkflowDependencyHealthy
        | Result::ArtifactIntegrityValid
        | Result::ResourcePressureNormal
        | Result::CredentialValid
        | Result::Inconclusive => provisional,
    }
}

fn cheapest_discriminating_probe(
    class: FailureClass,
    evidence: &FailureEvidence,
) -> Option<DiagnosticProbe> {
    allowed_probes(class).first().copied().or_else(|| {
        if class == FailureClass::Unknown {
            cheapest_phase_probe(evidence.phase)
        } else {
            None
        }
    })
}

fn cheapest_phase_probe(phase: FailurePhase) -> Option<DiagnosticProbe> {
    match phase {
        FailurePhase::Checkout => Some(DiagnosticProbe::ResolveAndHttpsHead),
        FailurePhase::SourceCompile | FailurePhase::Toolchain => {
            Some(DiagnosticProbe::ExactToolchainIdentity)
        }
        FailurePhase::Test | FailurePhase::TestHost | FailurePhase::ProcessSettlement => {
            Some(DiagnosticProbe::UnfinishedSelectorProcessTree)
        }
        FailurePhase::DependencyResolution | FailurePhase::Cache => {
            Some(DiagnosticProbe::ResolverCacheIdentity)
        }
        FailurePhase::Artifact | FailurePhase::Package => {
            Some(DiagnosticProbe::ProviderMetadataLookup)
        }
        FailurePhase::Preflight | FailurePhase::WorkflowRouting => {
            Some(DiagnosticProbe::WorkflowDependencyStatus)
        }
        FailurePhase::Credential => Some(DiagnosticProbe::CredentialIdentityStatus),
        FailurePhase::Unknown => None,
    }
}

fn alternatives(class: FailureClass, evidence: &FailureEvidence) -> Vec<FailureClass> {
    use FailureClass as Class;
    let candidates: &[Class] = match class {
        Class::NetworkDns => &[Class::NetworkProvider, Class::CredentialAuth],
        Class::NetworkProvider => &[Class::NetworkDns, Class::CredentialAuth],
        Class::SourceCompileError => &[Class::ToolchainMismatch, Class::DependencyResolution],
        Class::TestAssertion => &[Class::TestTimeout, Class::TestHostCrash],
        Class::TestTimeout => &[Class::TestHostCrash, Class::ProcessSettlement],
        Class::TestHostCrash => &[Class::TestTimeout, Class::ProcessSettlement],
        Class::ProcessSettlement => &[Class::TestTimeout, Class::TestHostCrash],
        Class::DependencyResolution => &[Class::CacheCorruption, Class::NetworkProvider],
        Class::CacheCorruption => &[Class::DependencyResolution, Class::ToolchainMismatch],
        Class::ArtifactMissing => &[Class::NetworkProvider, Class::CredentialAuth],
        Class::ArtifactCorruption => &[Class::CacheCorruption, Class::NetworkProvider],
        Class::ToolchainMismatch => &[Class::SourceCompileError, Class::DependencyResolution],
        Class::ResourceExhaustion => &[Class::TestTimeout, Class::ProcessSettlement],
        Class::RunnerCancellation => &[Class::WorkflowRouting],
        Class::WorkflowRouting => &[Class::RunnerCancellation],
        Class::CredentialAuth => &[Class::NetworkProvider],
        Class::Unknown => match evidence.phase {
            FailurePhase::Checkout => &[
                Class::NetworkDns,
                Class::NetworkProvider,
                Class::CredentialAuth,
            ],
            FailurePhase::SourceCompile => &[
                Class::SourceCompileError,
                Class::ToolchainMismatch,
                Class::DependencyResolution,
            ],
            FailurePhase::Test => &[
                Class::TestAssertion,
                Class::TestTimeout,
                Class::TestHostCrash,
            ],
            FailurePhase::TestHost | FailurePhase::ProcessSettlement => &[
                Class::TestHostCrash,
                Class::TestTimeout,
                Class::ProcessSettlement,
            ],
            FailurePhase::DependencyResolution | FailurePhase::Cache => &[
                Class::DependencyResolution,
                Class::CacheCorruption,
                Class::NetworkProvider,
            ],
            FailurePhase::Artifact | FailurePhase::Package => &[
                Class::ArtifactMissing,
                Class::ArtifactCorruption,
                Class::NetworkProvider,
            ],
            FailurePhase::Toolchain => &[Class::ToolchainMismatch, Class::SourceCompileError],
            FailurePhase::Preflight | FailurePhase::WorkflowRouting => {
                &[Class::WorkflowRouting, Class::RunnerCancellation]
            }
            FailurePhase::Credential => &[Class::CredentialAuth, Class::NetworkProvider],
            FailurePhase::Unknown => &[],
        },
    };
    candidates.iter().copied().take(3).collect()
}

fn supporting_observations(evidence: &FailureEvidence) -> Vec<SupportingObservation> {
    use KnownFailureSignature as Signature;
    use SupportingObservation as Observation;
    let mut observations = BTreeSet::new();

    match evidence.phase {
        FailurePhase::Checkout => {
            observations.insert(Observation::CheckoutFailed);
        }
        FailurePhase::SourceCompile => {
            observations.insert(Observation::CompileStepFailed);
        }
        FailurePhase::Test | FailurePhase::TestHost => {
            observations.insert(Observation::TestStepFailed);
        }
        FailurePhase::Package => {
            observations.insert(Observation::PackageStepFailed);
        }
        _ => {}
    }
    match evidence.source_scope {
        SourceScope::BranchSpecific => {
            observations.insert(Observation::BranchSpecificFailure);
        }
        SourceScope::Mainline => {
            observations.insert(Observation::MainlineFailure);
        }
        SourceScope::Unknown => {}
    }
    if evidence.exit_class == FailureExitClass::Cancelled {
        observations.insert(Observation::RunnerCancelled);
    }
    if evidence.prerequisite == PrerequisiteVerdict::Cancelled {
        observations.insert(Observation::PrerequisiteCancelled);
    }
    if evidence.repository_command_status == RepositoryCommandStatus::NotExecuted {
        observations.insert(Observation::NoRepositoryCommandExecuted);
    }
    if evidence.network == NetworkDiagnosticClass::DnsResolutionFailed
        || evidence.has_signature(Signature::DnsResolutionFailed)
    {
        observations.insert(Observation::DnsResolutionFailed);
    }
    if evidence.network == NetworkDiagnosticClass::ProviderUnavailable
        || evidence.has_signature(Signature::NetworkProviderUnavailable)
    {
        observations.insert(Observation::NetworkProviderUnavailable);
    }
    if evidence.has_signature(Signature::SwiftWarningBudgetExceeded) {
        observations.insert(Observation::SwiftWarningBudgetExceeded);
    }
    if evidence.test_verdict == TestVerdict::AssertionFailed
        || evidence.has_signature(Signature::TestAssertionFailed)
    {
        observations.insert(Observation::TestAssertionFailed);
    }
    if evidence.test_verdict == TestVerdict::KnownFlakyOnMain {
        observations.insert(Observation::KnownMainlineFlake);
    }
    if evidence.test_verdict == TestVerdict::TimedOut
        || evidence.has_signature(Signature::IndividualTestTimeout)
    {
        observations.insert(Observation::IndividualTestTimedOut);
    }
    if evidence.process_settlement == ProcessSettlementResult::HostCrashedOrRestarted
        || evidence.test_verdict == TestVerdict::HostCrashed
        || evidence.has_signature(Signature::AppHostCrashOrRestart)
    {
        observations.insert(Observation::AppHostCrashedOrRestarted);
    }
    if evidence.process_settlement == ProcessSettlementResult::DescendantPipeHeldOpen
        || evidence.has_signature(Signature::DetachedChildPipeHeldOpen)
    {
        observations.insert(Observation::DetachedChildPipeHeldOpen);
    }
    if evidence.artifact_cache == ArtifactCacheReceipt::ArtifactMissing
        || evidence.has_signature(Signature::ArtifactMissing)
    {
        observations.insert(Observation::ArtifactReportedMissing);
    }
    if evidence.artifact_cache == ArtifactCacheReceipt::ArtifactCorrupt
        || evidence.has_signature(Signature::ArtifactChecksumMismatch)
    {
        observations.insert(Observation::ArtifactReportedCorrupt);
    }
    if evidence.artifact_cache == ArtifactCacheReceipt::CacheCollision
        || evidence.has_signature(Signature::SwiftPmCacheCollision)
    {
        observations.insert(Observation::CacheCollisionReported);
    }
    if evidence.has_signature(Signature::DependencyResolutionFailure) {
        observations.insert(Observation::DependencyResolutionFailed);
    }
    if evidence.has_signature(Signature::PackagedHelperMissing) {
        observations.insert(Observation::PackagedHelperMissing);
    }
    if evidence.has_signature(Signature::ModuleImportMissing) {
        observations.insert(Observation::ModuleImportMissing);
    }
    if evidence.toolchain == ToolchainIdentityResult::Mismatch
        || evidence.has_signature(Signature::ToolchainMismatch)
    {
        observations.insert(Observation::ToolchainMismatch);
    }
    if (evidence.resource_pressure != ResourcePressureSummary::Unknown
        && evidence.resource_pressure != ResourcePressureSummary::Normal)
        || evidence.has_signature(Signature::ResourceExhaustion)
    {
        observations.insert(Observation::ResourceExhaustion);
    }
    if evidence.has_signature(Signature::WorkflowRoutingFailure) {
        observations.insert(Observation::WorkflowRoutingFailed);
    }
    if evidence.has_signature(Signature::CredentialAuthFailure) {
        observations.insert(Observation::CredentialAuthFailed);
    }

    observations.into_iter().collect()
}

fn recommended_remedy(class: FailureClass, evidence: &FailureEvidence) -> Option<RemedyClass> {
    use FailureClass as Class;
    use RemedyClass as Remedy;
    match class {
        Class::SourceCompileError => Some(Remedy::FixSourceRequired),
        Class::TestAssertion => {
            if evidence.test_verdict == TestVerdict::KnownFlakyOnMain {
                Some(Remedy::QuarantineSuite)
            } else {
                Some(Remedy::FixSourceRequired)
            }
        }
        Class::TestTimeout => Some(Remedy::RetrySame),
        Class::TestHostCrash => Some(Remedy::RetryFreshRunner),
        Class::ProcessSettlement => Some(Remedy::FixSourceRequired),
        Class::NetworkDns => Some(Remedy::RetryFreshRunner),
        Class::NetworkProvider => Some(Remedy::RerouteHosted),
        Class::DependencyResolution => Some(Remedy::ReResolveDependency),
        Class::CacheCorruption => Some(Remedy::ClearSpecificReconstructibleCache),
        Class::ArtifactMissing | Class::ArtifactCorruption => Some(Remedy::RebuildProduct),
        Class::ToolchainMismatch => Some(Remedy::RerouteHosted),
        Class::ResourceExhaustion => Some(Remedy::RetryFreshRunner),
        Class::RunnerCancellation => Some(Remedy::RetrySame),
        Class::WorkflowRouting => Some(Remedy::RebaseRefresh),
        Class::CredentialAuth => Some(Remedy::OperatorAdminRequired),
        Class::Unknown => None,
    }
}

fn source_repair_disposition(
    class: FailureClass,
    evidence: &FailureEvidence,
) -> SourceRepairDisposition {
    use FailureClass as Class;
    use SourceRepairDisposition as Disposition;
    match class {
        Class::SourceCompileError | Class::ProcessSettlement => Disposition::Required,
        Class::TestAssertion if evidence.test_verdict != TestVerdict::KnownFlakyOnMain => {
            Disposition::Required
        }
        Class::ArtifactMissing
            if evidence.has_signature(KnownFailureSignature::PackagedHelperMissing)
                || evidence.has_signature(KnownFailureSignature::ModuleImportMissing) =>
        {
            Disposition::Required
        }
        Class::NetworkDns
        | Class::NetworkProvider
        | Class::ResourceExhaustion
        | Class::RunnerCancellation
        | Class::CredentialAuth => Disposition::NoneIndicated,
        _ => Disposition::Undetermined,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadClass {
    Checkout,
    Compile,
    Test,
    AppHost,
    Package,
    Dependency,
    Artifact,
    Workflow,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureIncident {
    failure_class: FailureClass,
    workload_class: WorkloadClass,
    observed_at_millis: u64,
    wasted_runner_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    remedy_attempted: Option<RemedyClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remedy_succeeded: Option<bool>,
}

impl FailureIncident {
    #[must_use]
    pub const fn new(
        failure_class: FailureClass,
        workload_class: WorkloadClass,
        observed_at_millis: u64,
        wasted_runner_millis: u64,
    ) -> Self {
        Self {
            failure_class,
            workload_class,
            observed_at_millis,
            wasted_runner_millis,
            remedy_attempted: None,
            remedy_succeeded: None,
        }
    }

    #[must_use]
    pub const fn with_remedy(mut self, remedy: RemedyClass, succeeded: bool) -> Self {
        self.remedy_attempted = Some(remedy);
        self.remedy_succeeded = Some(succeeded);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemedyStatistics {
    remedy: RemedyClass,
    attempts: u64,
    successes: u64,
}

impl RemedyStatistics {
    #[must_use]
    pub const fn remedy(&self) -> RemedyClass {
        self.remedy
    }

    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.attempts
    }

    #[must_use]
    pub const fn successes(&self) -> u64 {
        self.successes
    }

    #[must_use]
    pub fn success_rate_basis_points(&self) -> Option<u16> {
        self.successes
            .checked_mul(10_000)?
            .checked_div(self.attempts)
            .map(|value| value as u16)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureClassStatistics {
    failure_class: FailureClass,
    workload_class: WorkloadClass,
    count: u64,
    first_seen_millis: u64,
    last_seen_millis: u64,
    remedy_statistics: Vec<RemedyStatistics>,
    wasted_runner_millis: u64,
}

impl FailureClassStatistics {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        self.failure_class
    }

    #[must_use]
    pub const fn workload_class(&self) -> WorkloadClass {
        self.workload_class
    }

    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub const fn first_seen_millis(&self) -> u64 {
        self.first_seen_millis
    }

    #[must_use]
    pub const fn last_seen_millis(&self) -> u64 {
        self.last_seen_millis
    }

    #[must_use]
    pub fn remedy_statistics(&self) -> &[RemedyStatistics] {
        &self.remedy_statistics
    }

    #[must_use]
    pub const fn wasted_runner_millis(&self) -> u64 {
        self.wasted_runner_millis
    }
}

#[derive(Debug)]
struct StatisticsAccumulator {
    count: u64,
    first_seen_millis: u64,
    last_seen_millis: u64,
    wasted_runner_millis: u64,
    remedies: BTreeMap<RemedyClass, (u64, u64)>,
}

/// Aggregate bounded historical incidents by failure class and workload class.
///
/// # Errors
///
/// Returns an error for an oversized history or arithmetic overflow.
pub fn aggregate_failure_history(
    incidents: &[FailureIncident],
) -> Result<Vec<FailureClassStatistics>, FailureDiagnosticError> {
    if incidents.len() > MAX_FAILURE_HISTORY_INCIDENTS {
        return Err(error("failure history exceeds the accepted incident limit"));
    }
    let mut groups: BTreeMap<(FailureClass, WorkloadClass), StatisticsAccumulator> =
        BTreeMap::new();
    for incident in incidents {
        let entry = groups
            .entry((incident.failure_class, incident.workload_class))
            .or_insert_with(|| StatisticsAccumulator {
                count: 0,
                first_seen_millis: incident.observed_at_millis,
                last_seen_millis: incident.observed_at_millis,
                wasted_runner_millis: 0,
                remedies: BTreeMap::new(),
            });
        entry.count = entry
            .count
            .checked_add(1)
            .ok_or_else(|| error("failure history count overflow"))?;
        entry.first_seen_millis = entry.first_seen_millis.min(incident.observed_at_millis);
        entry.last_seen_millis = entry.last_seen_millis.max(incident.observed_at_millis);
        entry.wasted_runner_millis = entry
            .wasted_runner_millis
            .checked_add(incident.wasted_runner_millis)
            .ok_or_else(|| error("failure history runner-time overflow"))?;
        if let Some(remedy) = incident.remedy_attempted {
            let remedy_entry = entry.remedies.entry(remedy).or_insert((0, 0));
            remedy_entry.0 = remedy_entry
                .0
                .checked_add(1)
                .ok_or_else(|| error("remedy attempt count overflow"))?;
            if incident.remedy_succeeded == Some(true) {
                remedy_entry.1 = remedy_entry
                    .1
                    .checked_add(1)
                    .ok_or_else(|| error("remedy success count overflow"))?;
            }
        }
    }

    Ok(groups
        .into_iter()
        .map(
            |((failure_class, workload_class), value)| FailureClassStatistics {
                failure_class,
                workload_class,
                count: value.count,
                first_seen_millis: value.first_seen_millis,
                last_seen_millis: value.last_seen_millis,
                remedy_statistics: value
                    .remedies
                    .into_iter()
                    .map(|(remedy, (attempts, successes))| RemedyStatistics {
                        remedy,
                        attempts,
                        successes,
                    })
                    .collect(),
                wasted_runner_millis: value.wasted_runner_millis,
            },
        )
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PreflightDetectorContract {
    detector_millis: u64,
    late_failure_millis: u64,
    low_false_positive_risk: bool,
    duplicates_more_expensive_work: bool,
    same_semantic_meaning: bool,
}

impl PreflightDetectorContract {
    /// Define the measured contract for one proposed earlier detector.
    ///
    /// # Errors
    ///
    /// Returns an error when either measured duration is zero.
    pub fn new(
        detector_millis: u64,
        late_failure_millis: u64,
        low_false_positive_risk: bool,
        duplicates_more_expensive_work: bool,
        same_semantic_meaning: bool,
    ) -> Result<Self, FailureDiagnosticError> {
        if detector_millis == 0 || late_failure_millis == 0 {
            return Err(error(
                "preflight detector and late-failure durations must be positive",
            ));
        }
        Ok(Self {
            detector_millis,
            late_failure_millis,
            low_false_positive_risk,
            duplicates_more_expensive_work,
            same_semantic_meaning,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PreflightCandidate {
    failure_class: FailureClass,
    workload_class: WorkloadClass,
    historical_occurrences: u64,
    detector_millis: u64,
    historical_wasted_runner_millis: u64,
}

impl PreflightCandidate {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        self.failure_class
    }

    #[must_use]
    pub const fn workload_class(&self) -> WorkloadClass {
        self.workload_class
    }

    #[must_use]
    pub const fn historical_occurrences(&self) -> u64 {
        self.historical_occurrences
    }

    #[must_use]
    pub const fn detector_millis(&self) -> u64 {
        self.detector_millis
    }

    #[must_use]
    pub const fn historical_wasted_runner_millis(&self) -> u64 {
        self.historical_wasted_runner_millis
    }
}

#[must_use]
pub fn promote_preflight_candidate(
    statistics: &FailureClassStatistics,
    contract: PreflightDetectorContract,
) -> Option<PreflightCandidate> {
    let substantially_earlier = contract
        .detector_millis
        .checked_mul(PREFLIGHT_EARLIER_RATIO)
        .is_some_and(|threshold| threshold <= contract.late_failure_millis);
    if statistics.count < MIN_PREFLIGHT_OCCURRENCES
        || !substantially_earlier
        || !contract.low_false_positive_risk
        || contract.duplicates_more_expensive_work
        || !contract.same_semantic_meaning
    {
        return None;
    }
    Some(PreflightCandidate {
        failure_class: statistics.failure_class,
        workload_class: statistics.workload_class,
        historical_occurrences: statistics.count,
        detector_millis: contract.detector_millis,
        historical_wasted_runner_millis: statistics.wasted_runner_millis,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PreflightOutcome {
    detected_before_expensive_runner: bool,
    late_runner_millis: u64,
}

impl PreflightOutcome {
    #[must_use]
    pub const fn new(detected_before_expensive_runner: bool, late_runner_millis: u64) -> Self {
        Self {
            detected_before_expensive_runner,
            late_runner_millis,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PreflightSavings {
    detections: u64,
    avoided_runner_millis: u64,
}

impl PreflightSavings {
    #[must_use]
    pub const fn detections(&self) -> u64 {
        self.detections
    }

    #[must_use]
    pub const fn avoided_runner_millis(&self) -> u64 {
        self.avoided_runner_millis
    }
}

/// Measure expensive-runner time avoided after a preflight detector is promoted.
///
/// # Errors
///
/// Returns an error for an oversized outcome window or arithmetic overflow.
pub fn measure_preflight_savings(
    outcomes: &[PreflightOutcome],
) -> Result<PreflightSavings, FailureDiagnosticError> {
    if outcomes.len() > MAX_PREFLIGHT_OUTCOMES {
        return Err(error("preflight outcomes exceed the accepted item limit"));
    }
    let mut detections = 0_u64;
    let mut avoided_runner_millis = 0_u64;
    for outcome in outcomes {
        if outcome.detected_before_expensive_runner {
            detections = detections
                .checked_add(1)
                .ok_or_else(|| error("preflight detection count overflow"))?;
            avoided_runner_millis = avoided_runner_millis
                .checked_add(outcome.late_runner_millis)
                .ok_or_else(|| error("preflight avoided runner-time overflow"))?;
        }
    }
    Ok(PreflightSavings {
        detections,
        avoided_runner_millis,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureDiagnosticError {
    message: &'static str,
}

impl fmt::Display for FailureDiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for FailureDiagnosticError {}

const fn error(message: &'static str) -> FailureDiagnosticError {
    FailureDiagnosticError { message }
}

fn validate_text(
    value: &str,
    maximum_bytes: usize,
    field: &'static str,
) -> Result<(), FailureDiagnosticError> {
    if value.is_empty() || value.len() > maximum_bytes || value.chars().any(char::is_control) {
        return Err(match field {
            "failed step" => error("failed step must be bounded non-control text"),
            "canonical evidence reference" => {
                error("canonical evidence reference must be bounded non-control text")
            }
            "test selector" => error("test selector must be bounded non-control text"),
            _ => error("diagnostic text must be bounded non-control text"),
        });
    }
    Ok(())
}

const fn failure_class_name(value: FailureClass) -> &'static str {
    match value {
        FailureClass::SourceCompileError => "source_compile_error",
        FailureClass::TestAssertion => "test_assertion",
        FailureClass::TestTimeout => "test_timeout",
        FailureClass::TestHostCrash => "test_host_crash",
        FailureClass::ProcessSettlement => "process_settlement",
        FailureClass::NetworkDns => "network_dns",
        FailureClass::NetworkProvider => "network_provider",
        FailureClass::DependencyResolution => "dependency_resolution",
        FailureClass::CacheCorruption => "cache_corruption",
        FailureClass::ArtifactMissing => "artifact_missing",
        FailureClass::ArtifactCorruption => "artifact_corruption",
        FailureClass::ToolchainMismatch => "toolchain_mismatch",
        FailureClass::ResourceExhaustion => "resource_exhaustion",
        FailureClass::RunnerCancellation => "runner_cancellation",
        FailureClass::WorkflowRouting => "workflow_routing",
        FailureClass::CredentialAuth => "credential_auth",
        FailureClass::Unknown => "unknown",
    }
}

const fn confidence_name(value: DiagnosticConfidence) -> &'static str {
    match value {
        DiagnosticConfidence::Exact => "exact",
        DiagnosticConfidence::Known => "known",
        DiagnosticConfidence::Likely => "likely",
        DiagnosticConfidence::ProbeRequired => "probe_required",
        DiagnosticConfidence::Unknown => "unknown",
    }
}

const fn probe_name(value: DiagnosticProbe) -> &'static str {
    match value {
        DiagnosticProbe::ResolveAndHttpsHead => "resolve_and_https_head",
        DiagnosticProbe::ProviderMetadataLookup => "provider_metadata_lookup",
        DiagnosticProbe::ResolverCacheIdentity => "resolver_cache_identity",
        DiagnosticProbe::UnfinishedSelectorProcessTree => "unfinished_selector_process_tree",
        DiagnosticProbe::ExactToolchainIdentity => "exact_toolchain_identity",
        DiagnosticProbe::ResourcePressureSnapshot => "resource_pressure_snapshot",
        DiagnosticProbe::WorkflowDependencyStatus => "workflow_dependency_status",
        DiagnosticProbe::ArtifactChecksum => "artifact_checksum",
        DiagnosticProbe::CredentialIdentityStatus => "credential_identity_status",
    }
}

const fn probe_result_name(value: DiagnosticProbeResult) -> &'static str {
    match value {
        DiagnosticProbeResult::DnsFailureConfirmed => "dns_failure_confirmed",
        DiagnosticProbeResult::DnsHealthy => "dns_healthy",
        DiagnosticProbeResult::NetworkProviderFailureConfirmed => {
            "network_provider_failure_confirmed"
        }
        DiagnosticProbeResult::NetworkReachable => "network_reachable",
        DiagnosticProbeResult::ProviderMissingConfirmed => "provider_missing_confirmed",
        DiagnosticProbeResult::ProviderPresent => "provider_present",
        DiagnosticProbeResult::CacheIdentityCollisionConfirmed => {
            "cache_identity_collision_confirmed"
        }
        DiagnosticProbeResult::CacheIdentityClean => "cache_identity_clean",
        DiagnosticProbeResult::UnfinishedSelectorPipeOwnerConfirmed => {
            "unfinished_selector_pipe_owner_confirmed"
        }
        DiagnosticProbeResult::ProcessTreeClean => "process_tree_clean",
        DiagnosticProbeResult::ToolchainMismatchConfirmed => "toolchain_mismatch_confirmed",
        DiagnosticProbeResult::ToolchainMatch => "toolchain_match",
        DiagnosticProbeResult::WorkflowDependencyCancelledConfirmed => {
            "workflow_dependency_cancelled_confirmed"
        }
        DiagnosticProbeResult::WorkflowDependencyHealthy => "workflow_dependency_healthy",
        DiagnosticProbeResult::ArtifactCorruptionConfirmed => "artifact_corruption_confirmed",
        DiagnosticProbeResult::ArtifactIntegrityValid => "artifact_integrity_valid",
        DiagnosticProbeResult::ResourceExhaustionConfirmed => "resource_exhaustion_confirmed",
        DiagnosticProbeResult::ResourcePressureNormal => "resource_pressure_normal",
        DiagnosticProbeResult::CredentialFailureConfirmed => "credential_failure_confirmed",
        DiagnosticProbeResult::CredentialValid => "credential_valid",
        DiagnosticProbeResult::Inconclusive => "inconclusive",
    }
}

const fn remedy_name(value: RemedyClass) -> &'static str {
    match value {
        RemedyClass::RetrySame => "retry_same",
        RemedyClass::RetryFreshRunner => "retry_fresh_runner",
        RemedyClass::ClearSpecificReconstructibleCache => "clear_specific_reconstructible_cache",
        RemedyClass::RebuildProduct => "rebuild_product",
        RemedyClass::ReResolveDependency => "re_resolve_dependency",
        RemedyClass::RerouteHosted => "reroute_hosted",
        RemedyClass::RebaseRefresh => "rebase_refresh",
        RemedyClass::QuarantineSuite => "quarantine_suite",
        RemedyClass::FixSourceRequired => "fix_source_required",
        RemedyClass::OperatorAdminRequired => "operator_admin_required",
    }
}

const fn source_repair_name(value: SourceRepairDisposition) -> &'static str {
    match value {
        SourceRepairDisposition::NoneIndicated => "none_indicated",
        SourceRepairDisposition::Required => "required",
        SourceRepairDisposition::Undetermined => "undetermined",
    }
}

const fn observation_name(value: SupportingObservation) -> &'static str {
    match value {
        SupportingObservation::CheckoutFailed => "checkout failed",
        SupportingObservation::CompileStepFailed => "compile step failed",
        SupportingObservation::TestStepFailed => "test step failed",
        SupportingObservation::PackageStepFailed => "package step failed",
        SupportingObservation::RunnerCancelled => "runner reported cancellation",
        SupportingObservation::PrerequisiteCancelled => "prerequisite reported cancellation",
        SupportingObservation::DnsResolutionFailed => "DNS resolution failed",
        SupportingObservation::NetworkProviderUnavailable => "network provider unavailable",
        SupportingObservation::NoRepositoryCommandExecuted => "no repository command executed",
        SupportingObservation::SwiftWarningBudgetExceeded => "Swift warning budget exceeded",
        SupportingObservation::TestAssertionFailed => "test assertion failed",
        SupportingObservation::KnownMainlineFlake => "known mainline flake",
        SupportingObservation::IndividualTestTimedOut => "individual test timed out",
        SupportingObservation::AppHostCrashedOrRestarted => "app host crashed or restarted",
        SupportingObservation::DetachedChildPipeHeldOpen => "detached child retained a pipe",
        SupportingObservation::ArtifactReportedMissing => "artifact reported missing",
        SupportingObservation::ArtifactReportedCorrupt => "artifact reported corrupt",
        SupportingObservation::CacheCollisionReported => "cache identity collision reported",
        SupportingObservation::DependencyResolutionFailed => "dependency resolution failed",
        SupportingObservation::PackagedHelperMissing => "packaged helper missing",
        SupportingObservation::ModuleImportMissing => "module import missing",
        SupportingObservation::ToolchainMismatch => "toolchain identity mismatch",
        SupportingObservation::ResourceExhaustion => "resource exhaustion reported",
        SupportingObservation::WorkflowRoutingFailed => "workflow routing failed",
        SupportingObservation::CredentialAuthFailed => "credential authentication failed",
        SupportingObservation::BranchSpecificFailure => "failure is branch-specific",
        SupportingObservation::MainlineFailure => "failure is on mainline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(exit: FailureExitClass, phase: FailurePhase, step: &str) -> FailureEvidence {
        FailureEvidence::new(exit, phase, step, 60_000).expect("valid evidence fixture")
    }

    #[test]
    fn cmux_branch_swift_warning_classifies_as_source_compile_error() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::SourceCompile,
            "Build tagged macOS app",
        )
        .with_source_scope(SourceScope::BranchSpecific)
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_signature(KnownFailureSignature::SwiftWarningBudgetExceeded)
        .expect("signature");

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::SourceCompileError);
        assert_eq!(
            diagnosis.recommended_remedy(),
            Some(RemedyClass::FixSourceRequired)
        );
        assert_eq!(diagnosis.source_repair(), SourceRepairDisposition::Required);
    }

    #[test]
    fn cmux_runner_dns_outage_never_recommends_source_repair() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Checkout,
            "Checkout repository",
        )
        .with_network(NetworkDiagnosticClass::DnsResolutionFailed)
        .with_repository_command_status(RepositoryCommandStatus::NotExecuted)
        .with_signature(KnownFailureSignature::DnsResolutionFailed)
        .expect("signature");

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::NetworkDns);
        assert_eq!(diagnosis.confidence(), DiagnosticConfidence::Exact);
        assert_eq!(
            diagnosis.next_probe(),
            Some(DiagnosticProbe::ResolveAndHttpsHead)
        );
        assert_eq!(
            diagnosis.recommended_remedy(),
            Some(RemedyClass::RetryFreshRunner)
        );
        assert_eq!(
            diagnosis.source_repair(),
            SourceRepairDisposition::NoneIndicated
        );
        assert!(
            diagnosis
                .observations()
                .contains(&SupportingObservation::NoRepositoryCommandExecuted)
        );
    }

    #[test]
    fn cmux_cancelled_prerequisite_stays_out_of_source_classes() {
        let input = evidence(
            FailureExitClass::Cancelled,
            FailurePhase::WorkflowRouting,
            "app-host unit tests",
        )
        .with_prerequisite(PrerequisiteVerdict::Cancelled);

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::RunnerCancellation);
        assert_eq!(diagnosis.recommended_remedy(), Some(RemedyClass::RetrySame));
        assert_eq!(
            diagnosis.source_repair(),
            SourceRepairDisposition::NoneIndicated
        );
    }

    #[test]
    fn cmux_mainline_flake_classifies_as_assertion_and_quarantine_candidate() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Test,
            "Test Zig SDK",
        )
        .with_source_scope(SourceScope::Mainline)
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_test_verdict(TestVerdict::KnownFlakyOnMain)
        .with_test_selector(
            "resource.test.wait cancel false drains the raced response before reuse",
        )
        .expect("selector")
        .with_signature(KnownFailureSignature::TestAssertionFailed)
        .expect("signature");

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::TestAssertion);
        assert_eq!(
            diagnosis.recommended_remedy(),
            Some(RemedyClass::QuarantineSuite)
        );
    }

    #[test]
    fn known_test_assertion_does_not_inherit_timeout_probe() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Test,
            "Test Zig SDK",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_test_verdict(TestVerdict::AssertionFailed);

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::TestAssertion);
        assert_eq!(diagnosis.next_probe(), None);
    }

    #[test]
    fn cmux_missing_packaged_helper_classifies_separately_from_cache_failure() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Package,
            "Build tagged macOS app",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_signature(KnownFailureSignature::PackagedHelperMissing)
        .expect("signature");

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::ArtifactMissing);
        assert_eq!(
            diagnosis.recommended_remedy(),
            Some(RemedyClass::RebuildProduct)
        );
        assert_eq!(diagnosis.source_repair(), SourceRepairDisposition::Required);
    }

    #[test]
    fn cmux_app_host_timeout_with_restart_evidence_is_host_crash() {
        let input = evidence(
            FailureExitClass::TimedOut,
            FailurePhase::TestHost,
            "app-host unit test shard",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_process_settlement(ProcessSettlementResult::HostCrashedOrRestarted)
        .with_test_verdict(TestVerdict::HostCrashed);

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::TestHostCrash);
        assert_eq!(
            diagnosis.next_probe(),
            Some(DiagnosticProbe::UnfinishedSelectorProcessTree)
        );
        assert_eq!(
            diagnosis.recommended_remedy(),
            Some(RemedyClass::RetryFreshRunner)
        );
    }

    #[test]
    fn reusable_state_corruption_from_issue_21_maps_to_cache_corruption() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Cache,
            "Consume reusable state",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_reusable_state_integrity(ReusableStateIntegrityState::Corrupt);

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::CacheCorruption);
        assert_eq!(
            diagnosis.recommended_remedy(),
            Some(RemedyClass::ClearSpecificReconstructibleCache)
        );
    }

    #[test]
    fn swiftpm_cache_collision_uses_specific_reconstructible_cache_remedy() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::DependencyResolution,
            "Resolve package graph",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_artifact_cache(ArtifactCacheReceipt::CacheCollision)
        .with_signature(KnownFailureSignature::SwiftPmCacheCollision)
        .expect("signature");

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::CacheCorruption);
        assert_eq!(
            diagnosis.recommended_remedy(),
            Some(RemedyClass::ClearSpecificReconstructibleCache)
        );
    }

    #[test]
    fn detached_child_pipe_hang_selects_process_tree_probe() {
        let input = evidence(
            FailureExitClass::TimedOut,
            FailurePhase::ProcessSettlement,
            "Wait for command settlement",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed)
        .with_process_settlement(ProcessSettlementResult::DescendantPipeHeldOpen);

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::ProcessSettlement);
        assert_eq!(
            diagnosis.next_probe(),
            Some(DiagnosticProbe::UnfinishedSelectorProcessTree)
        );
    }

    #[test]
    fn plausible_xcode_failure_remains_unknown_without_discriminating_evidence() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::SourceCompile,
            "xcodebuild",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed);

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::Unknown);
        assert_eq!(diagnosis.confidence(), DiagnosticConfidence::Unknown);
        assert_eq!(
            diagnosis.alternatives(),
            &[
                FailureClass::SourceCompileError,
                FailureClass::ToolchainMismatch,
                FailureClass::DependencyResolution
            ]
        );
        assert_eq!(
            diagnosis.next_probe(),
            Some(DiagnosticProbe::ExactToolchainIdentity)
        );
        assert_eq!(diagnosis.recommended_remedy(), None);
    }

    #[test]
    fn probe_must_belong_to_allowed_set() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Checkout,
            "Checkout repository",
        )
        .with_network(NetworkDiagnosticClass::DnsResolutionFailed);
        let wrong_probe = ProbeObservation::new(
            DiagnosticProbe::ArtifactChecksum,
            DiagnosticProbeResult::ArtifactIntegrityValid,
        );

        let result = diagnose_failure(&input, Some(wrong_probe));
        assert!(result.is_err());
    }

    #[test]
    fn exact_diagnosis_has_no_plausible_alternatives() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Checkout,
            "Checkout repository",
        )
        .with_network(NetworkDiagnosticClass::DnsResolutionFailed)
        .with_repository_command_status(RepositoryCommandStatus::NotExecuted);

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert!(diagnosis.alternatives().is_empty());
    }

    #[test]
    fn contradictory_probe_downgrades_to_probe_required_unknown() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Checkout,
            "Checkout repository",
        )
        .with_network(NetworkDiagnosticClass::DnsResolutionFailed)
        .with_repository_command_status(RepositoryCommandStatus::NotExecuted);
        let probe = ProbeObservation::new(
            DiagnosticProbe::ResolveAndHttpsHead,
            DiagnosticProbeResult::NetworkReachable,
        );

        let diagnosis = diagnose_failure(&input, Some(probe)).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::Unknown);
        assert_eq!(diagnosis.confidence(), DiagnosticConfidence::ProbeRequired);
        assert_eq!(diagnosis.recommended_remedy(), None);
        assert_eq!(diagnosis.alternatives(), &[FailureClass::CredentialAuth]);
    }

    #[test]
    fn impossible_probe_result_pair_is_rejected() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Checkout,
            "Checkout repository",
        )
        .with_network(NetworkDiagnosticClass::DnsResolutionFailed);
        let probe = ProbeObservation::new(
            DiagnosticProbe::ResolveAndHttpsHead,
            DiagnosticProbeResult::ArtifactCorruptionConfirmed,
        );

        assert!(diagnose_failure(&input, Some(probe)).is_err());
    }

    #[test]
    fn probe_can_promote_bounded_evidence_to_exact_classification() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::DependencyResolution,
            "Resolve package graph",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed);
        let probe = ProbeObservation::new(
            DiagnosticProbe::ResolverCacheIdentity,
            DiagnosticProbeResult::CacheIdentityCollisionConfirmed,
        );

        let diagnosis = diagnose_failure(&input, Some(probe)).expect("diagnosis");
        assert_eq!(diagnosis.failure_class(), FailureClass::CacheCorruption);
        assert_eq!(diagnosis.confidence(), DiagnosticConfidence::Exact);
        assert_eq!(
            diagnosis.probe_result(),
            Some(DiagnosticProbeResult::CacheIdentityCollisionConfirmed)
        );
    }

    #[test]
    fn absent_repository_command_evidence_stays_unknown() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::SourceCompile,
            "Compile project",
        );

        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        assert!(
            !diagnosis
                .observations()
                .contains(&SupportingObservation::NoRepositoryCommandExecuted)
        );
    }

    #[test]
    fn diagnosis_is_deterministic_and_never_authorizes_mutation() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Checkout,
            "Checkout repository",
        )
        .with_network(NetworkDiagnosticClass::DnsResolutionFailed);

        let first = diagnose_failure(&input, None).expect("first");
        let second = diagnose_failure(&input, None).expect("second");
        assert_eq!(first, second);
        assert!(!first.authorizes_mutation());
    }

    #[test]
    fn history_measures_remedy_success_and_wasted_runner_time() {
        let incidents = vec![
            FailureIncident::new(
                FailureClass::ArtifactMissing,
                WorkloadClass::Package,
                100,
                240_000,
            )
            .with_remedy(RemedyClass::RebuildProduct, true),
            FailureIncident::new(
                FailureClass::ArtifactMissing,
                WorkloadClass::Package,
                200,
                600_000,
            )
            .with_remedy(RemedyClass::RebuildProduct, true),
            FailureIncident::new(
                FailureClass::ArtifactMissing,
                WorkloadClass::Package,
                300,
                1_200_000,
            )
            .with_remedy(RemedyClass::RebuildProduct, false),
        ];

        let summary = aggregate_failure_history(&incidents).expect("summary");
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].count(), 3);
        assert_eq!(summary[0].wasted_runner_millis(), 2_040_000);
        assert_eq!(summary[0].remedy_statistics()[0].attempts(), 3);
        assert_eq!(summary[0].remedy_statistics()[0].successes(), 2);
        assert_eq!(
            summary[0].remedy_statistics()[0].success_rate_basis_points(),
            Some(6_666)
        );
    }

    #[test]
    fn recurring_missing_helper_promotes_to_four_second_preflight() {
        let durations = [
            240_000_u64,
            300_000,
            360_000,
            420_000,
            480_000,
            540_000,
            600_000,
            720_000,
            840_000,
            960_000,
            1_080_000,
            1_200_000,
        ];
        let incidents: Vec<_> = durations
            .iter()
            .enumerate()
            .map(|(index, duration)| {
                FailureIncident::new(
                    FailureClass::ArtifactMissing,
                    WorkloadClass::Package,
                    index as u64,
                    *duration,
                )
                .with_remedy(RemedyClass::RebuildProduct, true)
            })
            .collect();
        let statistics = aggregate_failure_history(&incidents).expect("statistics");
        let contract =
            PreflightDetectorContract::new(4_000, 240_000, true, false, true).expect("contract");
        let candidate = promote_preflight_candidate(&statistics[0], contract).expect("candidate");

        assert_eq!(candidate.historical_occurrences(), 12);
        assert_eq!(candidate.detector_millis(), 4_000);
        assert_eq!(candidate.failure_class(), FailureClass::ArtifactMissing);

        let outcomes: Vec<_> = durations
            .iter()
            .map(|duration| PreflightOutcome::new(true, *duration))
            .collect();
        let savings = measure_preflight_savings(&outcomes).expect("savings");
        assert_eq!(savings.detections(), 12);
        assert_eq!(
            savings.avoided_runner_millis(),
            durations.iter().copied().sum::<u64>()
        );
    }

    #[test]
    fn preflight_promotion_rejects_semantic_drift_or_weak_timing() {
        let incidents = vec![
            FailureIncident::new(
                FailureClass::ArtifactMissing,
                WorkloadClass::Package,
                1,
                60_000,
            ),
            FailureIncident::new(
                FailureClass::ArtifactMissing,
                WorkloadClass::Package,
                2,
                60_000,
            ),
            FailureIncident::new(
                FailureClass::ArtifactMissing,
                WorkloadClass::Package,
                3,
                60_000,
            ),
        ];
        let statistics = aggregate_failure_history(&incidents).expect("statistics");
        let weak =
            PreflightDetectorContract::new(30_000, 60_000, true, false, true).expect("contract");
        let drift =
            PreflightDetectorContract::new(1_000, 60_000, true, false, false).expect("contract");

        assert_eq!(promote_preflight_candidate(&statistics[0], weak), None);
        assert_eq!(promote_preflight_candidate(&statistics[0], drift), None);
    }

    #[test]
    fn serialized_diagnosis_keeps_explicit_optional_action_fields() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::SourceCompile,
            "xcodebuild",
        )
        .with_repository_command_status(RepositoryCommandStatus::Executed);
        let diagnosis = diagnose_failure(&input, None).expect("diagnosis");
        let json = serde_json::to_value(&diagnosis).expect("serialize diagnosis");

        assert!(json.get("next_probe").is_some());
        assert!(json.get("probe_performed").is_some());
        assert!(json["probe_performed"].is_null());
        assert!(json.get("probe_result").is_some());
        assert!(json["probe_result"].is_null());
        assert!(json.get("recommended_remedy").is_some());
        assert!(json["recommended_remedy"].is_null());
    }

    #[test]
    fn human_dns_output_is_concise_and_actionable() {
        let input = evidence(
            FailureExitClass::NonZero,
            FailurePhase::Checkout,
            "Checkout repository",
        )
        .with_network(NetworkDiagnosticClass::DnsResolutionFailed);
        let output = diagnose_failure(&input, None)
            .expect("diagnosis")
            .render_human();

        assert!(output.contains("failure class: network_dns"));
        assert!(output.contains("DNS resolution failed"));
        assert!(output.contains("probe performed: none"));
        assert!(output.contains("probe result: pending"));
        assert!(output.contains("next action: retry_fresh_runner"));
        assert!(output.contains("source repair: none_indicated"));
    }
}
