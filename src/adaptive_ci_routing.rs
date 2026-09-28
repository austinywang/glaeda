//! Pure evidence-driven CI placement recommendations across generic execution pools.
//!
//! This module is deliberately recommendation-only. It consumes bounded observations and configured
//! pool/accounting facts, predicts completion ranges, applies an explicit operator policy, and emits
//! one inspectable report. It performs no dispatch, admission, host probing, billing API calls,
//! persistence, or lifecycle mutation.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::compute_workload::ComputeTrustClass;

pub const ADAPTIVE_CI_ROUTING_SCHEMA_VERSION: u8 = 1;
pub const MAX_ROUTING_ID_BYTES: usize = 96;
pub const MAX_ROUTING_CAPABILITIES: usize = 32;
pub const MAX_ROUTING_POOLS: usize = 32;
pub const MAX_ROUTING_OBSERVATIONS: usize = 256;
pub const MIN_ROUTING_VALIDATED_SAMPLES: u16 = 3;
pub const MAX_ROUTING_PHASE_MILLIS: u64 = 24 * 60 * 60 * 1_000;
pub const MAX_ROUTING_AGE_MILLIS: u64 = 180 * 24 * 60 * 60 * 1_000;
const PARTS_PER_MILLION: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RoutingId(String);

impl RoutingId {
    /// Parse one bounded opaque public routing identity.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, or non-canonical text.
    pub fn parse(value: &str) -> Result<Self, RoutingError> {
        let Some(first) = value.bytes().next() else {
            return Err(error(
                "identity",
                "routing_identity_empty",
                "routing identity must not be empty",
            ));
        };
        if value.len() > MAX_ROUTING_ID_BYTES
            || !(first.is_ascii_alphanumeric())
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
            })
            || value.contains("//")
            || value.contains("..")
        {
            return Err(error(
                "identity",
                "routing_identity_invalid",
                "routing identity must be bounded canonical text",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkloadClassV1 {
    project: RoutingId,
    profile: RoutingId,
    trust_class: ComputeTrustClass,
    source_class: RoutingId,
    input_class: RoutingId,
    resource_profile: RoutingId,
    requirements: Vec<RoutingId>,
}

impl WorkloadClassV1 {
    /// Construct a stable workload class without binding each source revision into a new class.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or excessive requirements.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project: RoutingId,
        profile: RoutingId,
        trust_class: ComputeTrustClass,
        source_class: RoutingId,
        input_class: RoutingId,
        resource_profile: RoutingId,
        mut requirements: Vec<RoutingId>,
    ) -> Result<Self, RoutingError> {
        if requirements.len() > MAX_ROUTING_CAPABILITIES {
            return Err(error(
                "workload.requirements",
                "routing_too_many_requirements",
                "workload requirements exceed the bounded maximum",
            ));
        }
        requirements.sort();
        if requirements.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(error(
                "workload.requirements",
                "routing_duplicate_requirement",
                "workload requirements must be unique",
            ));
        }
        Ok(Self {
            project,
            profile,
            trust_class,
            source_class,
            input_class,
            resource_profile,
            requirements,
        })
    }

    #[must_use]
    pub const fn project(&self) -> &RoutingId {
        &self.project
    }

    #[must_use]
    pub const fn profile(&self) -> &RoutingId {
        &self.profile
    }

    #[must_use]
    pub const fn trust_class(&self) -> ComputeTrustClass {
        self.trust_class
    }

    #[must_use]
    pub const fn source_class(&self) -> &RoutingId {
        &self.source_class
    }

    #[must_use]
    pub const fn input_class(&self) -> &RoutingId {
        &self.input_class
    }

    #[must_use]
    pub const fn resource_profile(&self) -> &RoutingId {
        &self.resource_profile
    }

    #[must_use]
    pub fn requirements(&self) -> &[RoutingId] {
        &self.requirements
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolAccountingClass {
    Owned,
    UnmeteredIncluded,
    MeteredIncluded,
    PaidBurst,
}

impl PoolAccountingClass {
    const fn economy_rank(self) -> u8 {
        match self {
            Self::Owned | Self::UnmeteredIncluded => 0,
            Self::MeteredIncluded => 1,
            Self::PaidBurst => 2,
        }
    }
}

/// Declares whether host-pressure evidence is meaningful for this execution pool.
///
/// This is deliberately independent of accounting: an owned/on-prem pool can have included-cost
/// accounting, while a provider-backed pool can use any commercial model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPressureApplicability {
    LocalObserved,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionPoolV1 {
    pool_id: RoutingId,
    execution_class: RoutingId,
    accounting_class: PoolAccountingClass,
    pressure_applicability: HostPressureApplicability,
    capabilities: Vec<RoutingId>,
}

impl ExecutionPoolV1 {
    /// Construct one generic execution pool. Provider names, if any, remain opaque configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or excessive capabilities.
    pub fn new(
        pool_id: RoutingId,
        execution_class: RoutingId,
        accounting_class: PoolAccountingClass,
        pressure_applicability: HostPressureApplicability,
        mut capabilities: Vec<RoutingId>,
    ) -> Result<Self, RoutingError> {
        if capabilities.len() > MAX_ROUTING_CAPABILITIES {
            return Err(error(
                "pool.capabilities",
                "routing_too_many_capabilities",
                "pool capabilities exceed the bounded maximum",
            ));
        }
        capabilities.sort();
        if capabilities.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(error(
                "pool.capabilities",
                "routing_duplicate_capability",
                "pool capabilities must be unique",
            ));
        }
        Ok(Self {
            pool_id,
            execution_class,
            accounting_class,
            pressure_applicability,
            capabilities,
        })
    }

    #[must_use]
    pub const fn pool_id(&self) -> &RoutingId {
        &self.pool_id
    }

    #[must_use]
    pub const fn execution_class(&self) -> &RoutingId {
        &self.execution_class
    }

    #[must_use]
    pub const fn accounting_class(&self) -> PoolAccountingClass {
        self.accounting_class
    }

    #[must_use]
    pub const fn pressure_applicability(&self) -> HostPressureApplicability {
        self.pressure_applicability
    }

    #[must_use]
    pub fn capabilities(&self) -> &[RoutingId] {
        &self.capabilities
    }

    fn satisfies(&self, workload: &WorkloadClassV1) -> bool {
        workload
            .requirements
            .iter()
            .all(|required| self.capabilities.binary_search(required).is_ok())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotStateClass {
    Unknown,
    Cold,
    Warm,
    HotExact,
}

impl HotStateClass {
    const fn preference_rank(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Cold => 1,
            Self::Warm => 2,
            Self::HotExact => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalityEvidenceSource {
    LocalAccepted,
    RemoteAdvisory,
    DeclaredUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotStateEvidenceV1 {
    class: HotStateClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_identity: Option<RoutingId>,
    source: LocalityEvidenceSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_until_millis: Option<u64>,
}

impl HotStateEvidenceV1 {
    /// Construct bounded locality evidence without granting pool eligibility.
    ///
    /// # Errors
    ///
    /// Returns an error when exact-hot locality lacks the exact reusable-state identity that made
    /// the observation comparable.
    pub fn new(
        class: HotStateClass,
        state_identity: Option<RoutingId>,
        source: LocalityEvidenceSource,
        valid_until_millis: Option<u64>,
    ) -> Result<Self, RoutingError> {
        if class == HotStateClass::HotExact && state_identity.is_none() {
            return Err(error(
                "hot_state.state_identity",
                "routing_exact_hot_identity_missing",
                "exact-hot locality requires an exact state identity",
            ));
        }
        if source == LocalityEvidenceSource::RemoteAdvisory && valid_until_millis.is_none() {
            return Err(error(
                "hot_state.valid_until_millis",
                "routing_remote_locality_expiry_missing",
                "remote locality evidence requires an explicit freshness expiry",
            ));
        }
        if source == LocalityEvidenceSource::DeclaredUnknown
            && (class != HotStateClass::Unknown || state_identity.is_some())
        {
            return Err(error(
                "hot_state.source",
                "routing_unknown_locality_claim",
                "unknown locality evidence cannot claim warm or exact reusable state",
            ));
        }
        if valid_until_millis == Some(0) {
            return Err(error(
                "hot_state.valid_until_millis",
                "routing_locality_expiry_invalid",
                "locality evidence expiry must be a positive epoch timestamp",
            ));
        }
        Ok(Self {
            class,
            state_identity,
            source,
            valid_until_millis,
        })
    }

    #[must_use]
    pub const fn class(&self) -> HotStateClass {
        self.class
    }

    #[must_use]
    pub const fn state_identity(&self) -> Option<&RoutingId> {
        self.state_identity.as_ref()
    }

    #[must_use]
    pub const fn source(&self) -> LocalityEvidenceSource {
        self.source
    }

    #[must_use]
    pub const fn valid_until_millis(&self) -> Option<u64> {
        self.valid_until_millis
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPressureClass {
    NotApplicable,
    Low,
    Moderate,
    High,
    Critical,
    Unknown,
}

impl HostPressureClass {
    const fn policy_rank(self) -> u8 {
        match self {
            Self::NotApplicable | Self::Low => 0,
            Self::Moderate => 1,
            Self::High => 2,
            Self::Critical => 3,
            Self::Unknown => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolEligibility {
    Eligible,
    Ineligible,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AllowanceBudgetV1 {
    pub period_id: RoutingId,
    pub included_budget_units: u64,
    pub remaining_estimate_units: u64,
    pub reset_at_millis: u64,
    pub observed_consumption_units: u64,
}

impl AllowanceBudgetV1 {
    /// Validate configured bounded accounting input.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero budget or a remaining estimate above the period budget.
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.included_budget_units == 0 {
            return Err(error(
                "allowance.included_budget_units",
                "routing_allowance_budget_zero",
                "allowance budget must be positive",
            ));
        }
        if self.remaining_estimate_units > self.included_budget_units {
            return Err(error(
                "allowance.remaining_estimate_units",
                "routing_allowance_remaining_exceeds_budget",
                "allowance remaining estimate cannot exceed the configured period budget",
            ));
        }
        Ok(())
    }

    fn projected_remaining_ppm(&self, consumption: u64) -> u32 {
        let remaining = self.remaining_estimate_units.saturating_sub(consumption);
        let ppm = u128::from(remaining) * u128::from(PARTS_PER_MILLION)
            / u128::from(self.included_budget_units);
        u32::try_from(ppm).unwrap_or(1_000_000)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentionEvidenceV1 {
    pub comparison_class: RoutingId,
    pub window_count: u16,
    pub offered_tasks: u32,
    pub maximum_simultaneous: u16,
    pub validated_completions: u32,
    pub elapsed_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_result_p50_millis: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_result_p90_millis: Option<u64>,
    pub semantic_mismatches: u32,
    pub unvalidated_completions: u32,
    pub failures: u32,
    pub resets: u32,
    pub unknown_results: u32,
    pub fallbacks: u32,
    pub unfinished: u32,
    pub peak_pressure: HostPressureClass,
}

impl ContentionEvidenceV1 {
    /// Validate one bounded #760-style aggregate.
    ///
    /// # Errors
    ///
    /// Returns an error for impossible counts or zero elapsed time.
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.window_count == 0 || self.offered_tasks == 0 || self.elapsed_millis == 0 {
            return Err(error(
                "contention",
                "routing_contention_empty",
                "contention evidence requires offered work, at least one window, and positive elapsed time",
            ));
        }
        let offered = u64::from(self.offered_tasks);
        if u32::from(self.maximum_simultaneous) > self.offered_tasks {
            return Err(error(
                "contention.maximum_simultaneous",
                "routing_contention_concurrency_out_of_range",
                "contention maximum simultaneous work cannot exceed offered work",
            ));
        }
        if u64::from(self.semantic_mismatches) > offered
            || u64::from(self.unvalidated_completions) > offered
            || u64::from(self.failures) > offered
            || u64::from(self.resets) > offered
            || u64::from(self.unknown_results) > offered
            || u64::from(self.fallbacks) > offered
            || u64::from(self.unfinished) > offered
        {
            return Err(error(
                "contention",
                "routing_contention_count_exceeds_offered",
                "contention outcome counts cannot exceed offered work",
            ));
        }
        if self.validated_completions > self.offered_tasks {
            return Err(error(
                "contention.validated_completions",
                "routing_contention_validated_exceeds_offered",
                "validated completions cannot exceed offered tasks",
            ));
        }
        if self.validated_completions > 0 && self.maximum_simultaneous == 0 {
            return Err(error(
                "contention.maximum_simultaneous",
                "routing_contention_zero_concurrency_with_completion",
                "validated contention completions require observed simultaneous work",
            ));
        }
        let terminal_partition = u64::from(self.validated_completions)
            + u64::from(self.semantic_mismatches)
            + u64::from(self.unvalidated_completions)
            + u64::from(self.failures)
            + u64::from(self.resets)
            + u64::from(self.unknown_results)
            + u64::from(self.unfinished);
        if terminal_partition != u64::from(self.offered_tasks) {
            return Err(error(
                "contention",
                "routing_contention_terminal_partition_invalid",
                "contention terminal counts must exactly cover offered work",
            ));
        }
        match (
            self.validated_completions,
            self.final_result_p50_millis,
            self.final_result_p90_millis,
        ) {
            (0, None, None) => {}
            (0, _, _) => {
                return Err(error(
                    "contention.final_result_p50_millis",
                    "routing_contention_latency_without_validated_completion",
                    "contention latency must be absent when there are no validated completions",
                ));
            }
            (_, Some(p50), Some(p90)) => {
                if p90 < p50 {
                    return Err(error(
                        "contention.final_result_p90_millis",
                        "routing_contention_percentile_order",
                        "contention p90 cannot be below p50",
                    ));
                }
                if p90 > self.elapsed_millis {
                    return Err(error(
                        "contention.final_result_p90_millis",
                        "routing_contention_latency_outside_window",
                        "contention p90 cannot exceed the observed window",
                    ));
                }
            }
            _ => {
                return Err(error(
                    "contention.final_result_p50_millis",
                    "routing_contention_validated_latency_missing",
                    "validated contention completions require p50 and p90 latency",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolCandidateV1 {
    pub pool: ExecutionPoolV1,
    pub eligibility: PoolEligibility,
    pub hot_state: HotStateEvidenceV1,
    pub pressure_after_admission: HostPressureClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowance: Option<AllowanceBudgetV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contention: Option<ContentionEvidenceV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOutcome {
    ValidatedSuccess,
    Failure,
    Fallback,
    SemanticMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ObservationTimingV1 {
    pub queue_millis: u64,
    pub start_millis: u64,
    pub preparation_millis: u64,
    pub execution_millis: u64,
    pub settlement_millis: u64,
}

impl ObservationTimingV1 {
    fn validate(self) -> Result<(), RoutingError> {
        for (field, value) in [
            ("queue_millis", self.queue_millis),
            ("start_millis", self.start_millis),
            ("preparation_millis", self.preparation_millis),
            ("execution_millis", self.execution_millis),
            ("settlement_millis", self.settlement_millis),
        ] {
            if value > MAX_ROUTING_PHASE_MILLIS {
                return Err(error(
                    field,
                    "routing_phase_duration_out_of_range",
                    "routing phase duration exceeds the bounded maximum",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourceEnvelopeV1 {
    pub cpu_millicores: u32,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutingObservationV1 {
    pub observation_id: RoutingId,
    pub workload: WorkloadClassV1,
    pub pool_id: RoutingId,
    pub execution_class: RoutingId,
    pub observed_at_millis: u64,
    pub hot_state: HotStateClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hot_state_identity: Option<RoutingId>,
    pub timing: ObservationTimingV1,
    pub outcome: ObservationOutcome,
    pub resource: ResourceEnvelopeV1,
    pub marginal_cost_microusd: u64,
    pub allowance_units: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QuantilesU64 {
    pub p50: u64,
    pub p90: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CompletionPredictionV1 {
    pub queue: QuantilesU64,
    pub start: QuantilesU64,
    pub preparation: QuantilesU64,
    pub execution: QuantilesU64,
    pub settlement: QuantilesU64,
    pub total: QuantilesU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourcePredictionV1 {
    pub cpu_millicores_p90: u64,
    pub memory_bytes_p90: u64,
    pub disk_bytes_p90: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceBasisV1 {
    pub fresh_observations: u16,
    pub validated_successes: u16,
    pub first_observed_at_millis: u64,
    pub last_observed_at_millis: u64,
    pub evidence_ids: Vec<RoutingId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolPredictionV1 {
    pub pool_id: RoutingId,
    pub execution_class: RoutingId,
    pub accounting_class: PoolAccountingClass,
    pub hot_state: HotStateEvidenceV1,
    pub basis: EvidenceBasisV1,
    pub completion: CompletionPredictionV1,
    pub marginal_cost_microusd: QuantilesU64,
    pub allowance_units: QuantilesU64,
    pub resource: ResourcePredictionV1,
    pub failure_permille: u16,
    pub fallback_permille: u16,
    pub semantic_mismatch_count: u16,
    pub pressure_after_admission: HostPressureClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowance: Option<AllowanceBudgetV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_allowance_remaining_ppm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contention: Option<ContentionEvidenceV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PredictionConfigV1 {
    pub max_age_millis: u64,
    pub max_samples: u16,
    pub min_validated_samples: u16,
}

impl Default for PredictionConfigV1 {
    fn default() -> Self {
        Self {
            max_age_millis: 30 * 24 * 60 * 60 * 1_000,
            max_samples: 64,
            min_validated_samples: 3,
        }
    }
}

impl PredictionConfigV1 {
    fn validate(self) -> Result<(), RoutingError> {
        if self.max_age_millis == 0 || self.max_age_millis > MAX_ROUTING_AGE_MILLIS {
            return Err(error(
                "prediction.max_age_millis",
                "routing_prediction_age_out_of_range",
                "prediction recency window must be positive and bounded",
            ));
        }
        if self.max_samples == 0
            || usize::from(self.max_samples) > MAX_ROUTING_OBSERVATIONS
            || self.min_validated_samples < MIN_ROUTING_VALIDATED_SAMPLES
            || self.min_validated_samples > self.max_samples
        {
            return Err(error(
                "prediction.samples",
                "routing_prediction_sample_bounds",
                "prediction sample bounds are invalid or below the comparable-sample floor",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingPolicyMode {
    Economy,
    Balanced,
    Latency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RoutingPolicyV1 {
    pub mode: RoutingPolicyMode,
    pub acceptable_completion_slack_millis: u64,
    pub min_latency_gain_to_spend_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_ceiling_microusd: Option<u64>,
    pub min_allowance_remaining_ppm: u32,
    pub max_failure_permille: u16,
    pub max_fallback_permille: u16,
    pub max_pressure: HostPressureClass,
}

impl RoutingPolicyV1 {
    #[must_use]
    pub const fn economy(acceptable_completion_slack_millis: u64) -> Self {
        Self {
            mode: RoutingPolicyMode::Economy,
            acceptable_completion_slack_millis,
            min_latency_gain_to_spend_millis: 0,
            spend_ceiling_microusd: None,
            min_allowance_remaining_ppm: 100_000,
            max_failure_permille: 100,
            max_fallback_permille: 100,
            max_pressure: HostPressureClass::High,
        }
    }

    #[must_use]
    pub const fn balanced(
        acceptable_completion_slack_millis: u64,
        min_latency_gain_to_spend_millis: u64,
    ) -> Self {
        Self {
            mode: RoutingPolicyMode::Balanced,
            acceptable_completion_slack_millis,
            min_latency_gain_to_spend_millis,
            spend_ceiling_microusd: None,
            min_allowance_remaining_ppm: 50_000,
            max_failure_permille: 100,
            max_fallback_permille: 100,
            max_pressure: HostPressureClass::High,
        }
    }

    #[must_use]
    pub const fn latency(spend_ceiling_microusd: u64) -> Self {
        Self {
            mode: RoutingPolicyMode::Latency,
            acceptable_completion_slack_millis: 0,
            min_latency_gain_to_spend_millis: 0,
            spend_ceiling_microusd: Some(spend_ceiling_microusd),
            min_allowance_remaining_ppm: 0,
            max_failure_permille: 100,
            max_fallback_permille: 100,
            max_pressure: HostPressureClass::High,
        }
    }

    fn validate(self) -> Result<(), RoutingError> {
        if self.min_allowance_remaining_ppm > 1_000_000
            || self.max_failure_permille > 1_000
            || self.max_fallback_permille > 1_000
        {
            return Err(error(
                "policy",
                "routing_policy_ratio_out_of_range",
                "policy ratios exceed their bounded range",
            ));
        }
        if self.mode == RoutingPolicyMode::Latency && self.spend_ceiling_microusd.is_none() {
            return Err(error(
                "policy.spend_ceiling_microusd",
                "routing_latency_spend_ceiling_required",
                "latency policy requires an explicit spend ceiling",
            ));
        }
        if matches!(
            self.max_pressure,
            HostPressureClass::Unknown | HostPressureClass::NotApplicable
        ) {
            return Err(error(
                "policy.max_pressure",
                "routing_policy_unknown_pressure_ceiling",
                "policy pressure ceiling must be a local observed pressure class",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationAuthority {
    RecommendationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationStatus {
    Recommended,
    Abstained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolExclusionReason {
    Ineligible,
    EligibilityUnknown,
    MissingCapability,
    MissingEvidence,
    StaleEvidence,
    InsufficientEvidence,
    SemanticMismatch,
    ReliabilityAbovePolicy,
    FallbackAbovePolicy,
    PressureAbovePolicy,
    SpendCeiling,
    AllowanceReserve,
    AllowanceExpired,
    AllowanceRequired,
    UnexpectedAllowance,
    InvalidAllowance,
    InvalidPressureSemantics,
    InvalidContentionEvidence,
    StaleLocalityEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolExclusionV1 {
    pub pool_id: RoutingId,
    pub reason: PoolExclusionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutingRecommendationV1 {
    pub schema_version: u8,
    pub authority: RecommendationAuthority,
    pub automatic_routing_eligible: bool,
    pub workload: WorkloadClassV1,
    pub policy: RoutingPolicyV1,
    pub status: RecommendationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<RoutingId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<RoutingId>,
    pub predictions: Vec<PoolPredictionV1>,
    pub exclusions: Vec<PoolExclusionV1>,
}

impl RoutingRecommendationV1 {
    /// Render an inspectable human explanation from the same typed report as JSON output.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut out = format!(
            "workload: {} / {}\npolicy: {:?}\nauthority: recommendation_only\nautomatic routing: disabled\n",
            self.workload.project.as_str(),
            self.workload.profile.as_str(),
            self.policy.mode
        );
        match &self.choice {
            Some(choice) => {
                out.push_str(&format!("choice: {}\n", choice.as_str()));
                if let Some(fallback) = &self.fallback {
                    out.push_str(&format!("fallback: {}\n", fallback.as_str()));
                }
                if let Some(prediction) = self
                    .predictions
                    .iter()
                    .find(|prediction| &prediction.pool_id == choice)
                {
                    out.push_str("basis:\n");
                    out.push_str(&format!(
                        "  compatible hot state: {:?}\n  predicted completion: {}-{} ms\n  queue: {}-{} ms\n  preparation: {}-{} ms\n  execution: {}-{} ms\n  settlement: {}-{} ms\n  comparable validated runs: {}\n  marginal money cost: {}-{} microUSD\n  allowance consumption: {}-{} units\n  pressure after admission: {:?}\n  failure rate: {}/1000\n  fallback rate: {}/1000\n",
                        prediction.hot_state.class,
                        prediction.completion.total.p50,
                        prediction.completion.total.p90,
                        prediction.completion.queue.p50,
                        prediction.completion.queue.p90,
                        prediction.completion.preparation.p50,
                        prediction.completion.preparation.p90,
                        prediction.completion.execution.p50,
                        prediction.completion.execution.p90,
                        prediction.completion.settlement.p50,
                        prediction.completion.settlement.p90,
                        prediction.basis.validated_successes,
                        prediction.marginal_cost_microusd.p50,
                        prediction.marginal_cost_microusd.p90,
                        prediction.allowance_units.p50,
                        prediction.allowance_units.p90,
                        prediction.pressure_after_admission,
                        prediction.failure_permille,
                        prediction.fallback_permille,
                    ));
                    if prediction.hot_state.source == LocalityEvidenceSource::RemoteAdvisory {
                        out.push_str(
                            "  locality source: remote advisory; pool eligibility remains independently declared\n",
                        );
                    }
                    if let Some(allowance) = &prediction.allowance {
                        out.push_str(&format!(
                            "  allowance period: {}\n  included/remaining/observed: {}/{}/{} units\n  allowance reset: {}\n",
                            allowance.period_id.as_str(),
                            allowance.included_budget_units,
                            allowance.remaining_estimate_units,
                            allowance.observed_consumption_units,
                            allowance.reset_at_millis,
                        ));
                    }
                    if let Some(remaining) = prediction.projected_allowance_remaining_ppm {
                        out.push_str(&format!(
                            "  projected allowance remaining: {remaining}/1000000\n"
                        ));
                    }
                    if let Some(contention) = &prediction.contention {
                        out.push_str(&format!(
                            "  contention: {}/{} validated in {} ms; max simultaneous {}; p90 {} ms; unvalidated {}; failures {}; resets {}; unknown {}; unfinished {}; fallbacks {}; pressure {:?}\n",
                            contention.validated_completions,
                            contention.offered_tasks,
                            contention.elapsed_millis,
                            contention.maximum_simultaneous,
                            render_optional_millis(contention.final_result_p90_millis),
                            contention.unvalidated_completions,
                            contention.failures,
                            contention.resets,
                            contention.unknown_results,
                            contention.unfinished,
                            contention.fallbacks,
                            contention.peak_pressure
                        ));
                    }
                }
            }
            None => out.push_str("choice: abstained\n"),
        }
        let eligible_alternatives = self
            .predictions
            .iter()
            .filter(|prediction| self.choice.as_ref() != Some(&prediction.pool_id))
            .filter(|prediction| {
                !self
                    .exclusions
                    .iter()
                    .any(|exclusion| exclusion.pool_id == prediction.pool_id)
            })
            .collect::<Vec<_>>();
        if !eligible_alternatives.is_empty() {
            out.push_str("alternatives:\n");
            for prediction in eligible_alternatives {
                out.push_str(&format!(
                    "  {}: {}-{} ms, {}-{} microUSD, {:?}\n",
                    prediction.pool_id.as_str(),
                    prediction.completion.total.p50,
                    prediction.completion.total.p90,
                    prediction.marginal_cost_microusd.p50,
                    prediction.marginal_cost_microusd.p90,
                    prediction.accounting_class
                ));
            }
        }
        if !self.exclusions.is_empty() {
            out.push_str("excluded:\n");
            for exclusion in &self.exclusions {
                out.push_str(&format!(
                    "  {}: {:?}\n",
                    exclusion.pool_id.as_str(),
                    exclusion.reason
                ));
            }
        }
        out
    }

    /// Render deterministic pretty JSON from the typed report.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed report fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Produce a pure recommendation from bounded observations and explicit policy.
///
/// Remote heat evidence may affect the predicted locality class for an already eligible pool, but
/// it never changes pool eligibility. The returned report also carries zero dispatch authority.
///
/// # Errors
///
/// Returns an error for invalid global configuration or malformed observation timing.
pub fn recommend_ci_pool(
    workload: &WorkloadClassV1,
    candidates: &[PoolCandidateV1],
    observations: &[RoutingObservationV1],
    now_millis: u64,
    prediction_config: PredictionConfigV1,
    policy: RoutingPolicyV1,
) -> Result<RoutingRecommendationV1, RoutingError> {
    prediction_config.validate()?;
    policy.validate()?;
    if candidates.len() > MAX_ROUTING_POOLS {
        return Err(error(
            "candidates",
            "routing_too_many_pools",
            "routing candidate input exceeds the bounded maximum",
        ));
    }
    let mut candidate_ids = BTreeSet::new();
    for candidate in candidates {
        if !candidate_ids.insert(candidate.pool.pool_id.as_str()) {
            return Err(error(
                "candidates.pool_id",
                "routing_duplicate_pool_id",
                "routing candidates require unique pool identities",
            ));
        }
    }
    if observations.len() > MAX_ROUTING_OBSERVATIONS {
        return Err(error(
            "observations",
            "routing_too_many_observations",
            "routing observation input exceeds the bounded maximum",
        ));
    }
    let mut observation_ids = BTreeSet::new();
    for observation in observations {
        if !observation_ids.insert(observation.observation_id.as_str()) {
            return Err(error(
                "observations.observation_id",
                "routing_duplicate_observation_id",
                "routing observation identifiers must be unique",
            ));
        }
        observation.timing.validate()?;
        if observation.hot_state == HotStateClass::HotExact
            && observation.hot_state_identity.is_none()
        {
            return Err(error(
                "observations.hot_state_identity",
                "routing_exact_hot_observation_identity_missing",
                "exact-hot observations require an exact state identity",
            ));
        }
    }

    let mut predictions = Vec::new();
    let mut exclusions = Vec::new();
    let mut evaluated = Vec::new();

    for candidate in candidates {
        let pool_id = candidate.pool.pool_id.clone();
        let hard_exclusion = match candidate.eligibility {
            PoolEligibility::Ineligible => Some(PoolExclusionReason::Ineligible),
            PoolEligibility::Unknown => Some(PoolExclusionReason::EligibilityUnknown),
            PoolEligibility::Eligible if !candidate.pool.satisfies(workload) => {
                Some(PoolExclusionReason::MissingCapability)
            }
            PoolEligibility::Eligible => None,
        };
        if let Some(reason) = hard_exclusion {
            exclusions.push(PoolExclusionV1 { pool_id, reason });
            continue;
        }
        if candidate
            .hot_state
            .valid_until_millis
            .is_some_and(|valid_until| valid_until <= now_millis)
        {
            exclusions.push(PoolExclusionV1 {
                pool_id,
                reason: PoolExclusionReason::StaleLocalityEvidence,
            });
            continue;
        }

        match candidate.pool.accounting_class {
            PoolAccountingClass::MeteredIncluded if candidate.allowance.is_none() => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::AllowanceRequired,
                });
                continue;
            }
            PoolAccountingClass::MeteredIncluded => {}
            _ if candidate.allowance.is_some() => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::UnexpectedAllowance,
                });
                continue;
            }
            _ => {}
        }
        let pressure_semantics_valid = match candidate.pool.pressure_applicability {
            HostPressureApplicability::LocalObserved => {
                candidate.pressure_after_admission != HostPressureClass::NotApplicable
            }
            HostPressureApplicability::NotApplicable => {
                candidate.pressure_after_admission == HostPressureClass::NotApplicable
            }
        };
        if !pressure_semantics_valid
            || candidate.contention.as_ref().is_some_and(|contention| {
                match candidate.pool.pressure_applicability {
                    HostPressureApplicability::LocalObserved => {
                        contention.peak_pressure == HostPressureClass::NotApplicable
                    }
                    HostPressureApplicability::NotApplicable => {
                        contention.peak_pressure != HostPressureClass::NotApplicable
                    }
                }
            })
        {
            exclusions.push(PoolExclusionV1 {
                pool_id,
                reason: PoolExclusionReason::InvalidPressureSemantics,
            });
            continue;
        }
        if let Some(allowance) = &candidate.allowance {
            if allowance.validate().is_err() {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::InvalidAllowance,
                });
                continue;
            }
            if allowance.reset_at_millis <= now_millis {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::AllowanceExpired,
                });
                continue;
            }
        }
        if let Some(contention) = &candidate.contention
            && contention.validate().is_err()
        {
            exclusions.push(PoolExclusionV1 {
                pool_id,
                reason: PoolExclusionReason::InvalidContentionEvidence,
            });
            continue;
        }

        let prediction = match predict_pool(
            workload,
            candidate,
            observations,
            now_millis,
            prediction_config,
        ) {
            Ok(prediction) => prediction,
            Err(EvidenceRefusal::Missing) => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::MissingEvidence,
                });
                continue;
            }
            Err(EvidenceRefusal::Stale) => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::StaleEvidence,
                });
                continue;
            }
            Err(EvidenceRefusal::Insufficient) => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::InsufficientEvidence,
                });
                continue;
            }
        };

        let policy_exclusion = policy_exclusion(&prediction, candidate, policy);
        predictions.push(prediction.clone());
        if let Some(reason) = policy_exclusion {
            exclusions.push(PoolExclusionV1 { pool_id, reason });
        } else {
            evaluated.push(EvaluatedCandidate { prediction });
        }
    }

    predictions.sort_by(|left, right| left.pool_id.cmp(&right.pool_id));
    exclusions.sort_by(|left, right| left.pool_id.cmp(&right.pool_id));

    let choice = select_candidate(&evaluated, policy).map(|entry| entry.prediction.pool_id.clone());
    let fallback_candidates = evaluated
        .iter()
        .filter(|entry| choice.as_ref() != Some(&entry.prediction.pool_id))
        .cloned()
        .collect::<Vec<_>>();
    let fallback = select_candidate(&fallback_candidates, policy)
        .map(|entry| entry.prediction.pool_id.clone());
    let status = if choice.is_some() {
        RecommendationStatus::Recommended
    } else {
        RecommendationStatus::Abstained
    };

    Ok(RoutingRecommendationV1 {
        schema_version: ADAPTIVE_CI_ROUTING_SCHEMA_VERSION,
        authority: RecommendationAuthority::RecommendationOnly,
        automatic_routing_eligible: false,
        workload: workload.clone(),
        policy,
        status,
        choice,
        fallback,
        predictions,
        exclusions,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceRefusal {
    Missing,
    Stale,
    Insufficient,
}

#[derive(Clone)]
struct EvaluatedCandidate {
    prediction: PoolPredictionV1,
}

fn predict_pool(
    workload: &WorkloadClassV1,
    candidate: &PoolCandidateV1,
    observations: &[RoutingObservationV1],
    now_millis: u64,
    config: PredictionConfigV1,
) -> Result<PoolPredictionV1, EvidenceRefusal> {
    let matching: Vec<&RoutingObservationV1> = observations
        .iter()
        .filter(|observation| {
            observation.workload == *workload
                && observation.pool_id == candidate.pool.pool_id
                && observation.execution_class == candidate.pool.execution_class
                && observation.hot_state == candidate.hot_state.class
                && candidate
                    .hot_state
                    .state_identity
                    .as_ref()
                    .is_none_or(|identity| {
                        observation.hot_state_identity.as_ref() == Some(identity)
                    })
        })
        .collect();
    if matching.is_empty() {
        return Err(EvidenceRefusal::Missing);
    }

    let mut fresh: Vec<&RoutingObservationV1> = matching
        .into_iter()
        .filter(|observation| {
            observation.observed_at_millis <= now_millis
                && now_millis - observation.observed_at_millis <= config.max_age_millis
        })
        .collect();
    if fresh.is_empty() {
        return Err(EvidenceRefusal::Stale);
    }
    fresh.sort_by(|left, right| {
        right
            .observed_at_millis
            .cmp(&left.observed_at_millis)
            .then_with(|| left.observation_id.cmp(&right.observation_id))
    });
    fresh.truncate(usize::from(config.max_samples));

    let successes: Vec<&RoutingObservationV1> = fresh
        .iter()
        .copied()
        .filter(|observation| observation.outcome == ObservationOutcome::ValidatedSuccess)
        .collect();
    if successes.len() < usize::from(config.min_validated_samples) {
        return Err(EvidenceRefusal::Insufficient);
    }

    let queue = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.queue_millis),
    );
    let start = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.start_millis),
    );
    let preparation = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.preparation_millis),
    );
    let execution = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.execution_millis),
    );
    let settlement = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.settlement_millis),
    );
    let total = quantiles(successes.iter().map(|observation| {
        sum_components([
            observation.timing.queue_millis,
            observation.timing.start_millis,
            observation.timing.preparation_millis,
            observation.timing.execution_millis,
            observation.timing.settlement_millis,
        ])
    }));

    // Cost, allowance, and resource consumption occur on failed/fallback attempts too.
    let marginal_cost_microusd = quantiles(
        fresh
            .iter()
            .map(|observation| observation.marginal_cost_microusd),
    );
    let allowance_units = quantiles(fresh.iter().map(|observation| observation.allowance_units));
    let resource = ResourcePredictionV1 {
        cpu_millicores_p90: quantiles(
            fresh
                .iter()
                .map(|observation| u64::from(observation.resource.cpu_millicores)),
        )
        .p90,
        memory_bytes_p90: quantiles(
            fresh
                .iter()
                .map(|observation| observation.resource.memory_bytes),
        )
        .p90,
        disk_bytes_p90: quantiles(
            fresh
                .iter()
                .map(|observation| observation.resource.disk_bytes),
        )
        .p90,
    };

    let failures = fresh
        .iter()
        .filter(|observation| observation.outcome == ObservationOutcome::Failure)
        .count();
    let fallbacks = fresh
        .iter()
        .filter(|observation| observation.outcome == ObservationOutcome::Fallback)
        .count();
    let semantic_mismatches = fresh
        .iter()
        .filter(|observation| observation.outcome == ObservationOutcome::SemanticMismatch)
        .count();
    let denominator = fresh.len();
    let failure_permille = rate_permille(failures + semantic_mismatches, denominator);
    let fallback_permille = rate_permille(fallbacks, denominator);
    let first_observed_at_millis = fresh
        .iter()
        .map(|observation| observation.observed_at_millis)
        .min()
        .unwrap_or(now_millis);
    let last_observed_at_millis = fresh
        .iter()
        .map(|observation| observation.observed_at_millis)
        .max()
        .unwrap_or(now_millis);
    let projected_allowance_remaining_ppm = candidate
        .allowance
        .as_ref()
        .map(|allowance| allowance.projected_remaining_ppm(allowance_units.p90));
    let mut evidence_ids = fresh
        .iter()
        .map(|observation| {
            (
                observation.observed_at_millis,
                observation.observation_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    evidence_ids.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let evidence_ids = evidence_ids
        .into_iter()
        .map(|(_, identity)| identity)
        .collect();

    Ok(PoolPredictionV1 {
        pool_id: candidate.pool.pool_id.clone(),
        execution_class: candidate.pool.execution_class.clone(),
        accounting_class: candidate.pool.accounting_class,
        hot_state: candidate.hot_state.clone(),
        basis: EvidenceBasisV1 {
            fresh_observations: u16::try_from(fresh.len()).unwrap_or(u16::MAX),
            validated_successes: u16::try_from(successes.len()).unwrap_or(u16::MAX),
            first_observed_at_millis,
            last_observed_at_millis,
            evidence_ids,
        },
        completion: CompletionPredictionV1 {
            queue,
            start,
            preparation,
            execution,
            settlement,
            total,
        },
        marginal_cost_microusd,
        allowance_units,
        resource,
        failure_permille,
        fallback_permille,
        semantic_mismatch_count: u16::try_from(semantic_mismatches).unwrap_or(u16::MAX),
        pressure_after_admission: candidate.pressure_after_admission,
        allowance: candidate.allowance.clone(),
        projected_allowance_remaining_ppm,
        contention: candidate.contention.clone(),
    })
}

fn policy_exclusion(
    prediction: &PoolPredictionV1,
    candidate: &PoolCandidateV1,
    policy: RoutingPolicyV1,
) -> Option<PoolExclusionReason> {
    if prediction.semantic_mismatch_count > 0
        || candidate
            .contention
            .as_ref()
            .is_some_and(|contention| contention.semantic_mismatches > 0)
    {
        return Some(PoolExclusionReason::SemanticMismatch);
    }
    if prediction.failure_permille > policy.max_failure_permille {
        return Some(PoolExclusionReason::ReliabilityAbovePolicy);
    }
    if prediction.fallback_permille > policy.max_fallback_permille {
        return Some(PoolExclusionReason::FallbackAbovePolicy);
    }
    if let Some(contention) = &candidate.contention {
        let contention_failure_permille = rate_permille_u64(
            u64::from(contention.unvalidated_completions)
                + u64::from(contention.failures)
                + u64::from(contention.resets)
                + u64::from(contention.unknown_results)
                + u64::from(contention.unfinished),
            u64::from(contention.offered_tasks),
        );
        if contention_failure_permille > policy.max_failure_permille {
            return Some(PoolExclusionReason::ReliabilityAbovePolicy);
        }
        let contention_fallback_permille = rate_permille_u64(
            u64::from(contention.fallbacks),
            u64::from(contention.offered_tasks),
        );
        if contention_fallback_permille > policy.max_fallback_permille {
            return Some(PoolExclusionReason::FallbackAbovePolicy);
        }
    }
    if prediction.pressure_after_admission.policy_rank() > policy.max_pressure.policy_rank()
        || candidate.contention.as_ref().is_some_and(|contention| {
            contention.peak_pressure.policy_rank() > policy.max_pressure.policy_rank()
        })
    {
        return Some(PoolExclusionReason::PressureAbovePolicy);
    }
    if policy
        .spend_ceiling_microusd
        .is_some_and(|ceiling| prediction.marginal_cost_microusd.p90 > ceiling)
    {
        return Some(PoolExclusionReason::SpendCeiling);
    }
    if candidate.allowance.is_some()
        && prediction
            .projected_allowance_remaining_ppm
            .is_some_and(|remaining| remaining < policy.min_allowance_remaining_ppm)
    {
        return Some(PoolExclusionReason::AllowanceReserve);
    }
    None
}

fn select_candidate(
    candidates: &[EvaluatedCandidate],
    policy: RoutingPolicyV1,
) -> Option<&EvaluatedCandidate> {
    match policy.mode {
        RoutingPolicyMode::Economy => select_economy(candidates, policy),
        RoutingPolicyMode::Balanced => {
            let economy = select_economy(candidates, policy)?;
            let fastest_p90 = candidates
                .iter()
                .map(|entry| entry.prediction.completion.total.p90)
                .min()?;
            let tail_deadline =
                fastest_p90.saturating_add(policy.acceptable_completion_slack_millis);
            let fastest = candidates
                .iter()
                .filter(|entry| entry.prediction.completion.total.p90 <= tail_deadline)
                .min_by(|left, right| latency_cmp(left, right))?;
            if fastest
                .prediction
                .completion
                .total
                .p50
                .saturating_add(policy.min_latency_gain_to_spend_millis)
                <= economy.prediction.completion.total.p50
            {
                Some(fastest)
            } else {
                Some(economy)
            }
        }
        RoutingPolicyMode::Latency => candidates
            .iter()
            .min_by(|left, right| latency_cmp(left, right)),
    }
}

fn select_economy(
    candidates: &[EvaluatedCandidate],
    policy: RoutingPolicyV1,
) -> Option<&EvaluatedCandidate> {
    let fastest_p90 = candidates
        .iter()
        .map(|entry| entry.prediction.completion.total.p90)
        .min()?;
    let deadline = fastest_p90.saturating_add(policy.acceptable_completion_slack_millis);
    candidates
        .iter()
        .filter(|entry| entry.prediction.completion.total.p90 <= deadline)
        .min_by(|left, right| economy_cmp(left, right))
}

fn economy_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    left.prediction
        .accounting_class
        .economy_rank()
        .cmp(&right.prediction.accounting_class.economy_rank())
        .then_with(|| {
            right
                .prediction
                .projected_allowance_remaining_ppm
                .unwrap_or(1_000_000)
                .cmp(
                    &left
                        .prediction
                        .projected_allowance_remaining_ppm
                        .unwrap_or(1_000_000),
                )
        })
        .then_with(|| {
            left.prediction
                .marginal_cost_microusd
                .p50
                .cmp(&right.prediction.marginal_cost_microusd.p50)
        })
        .then_with(|| pressure_cmp(left, right))
        .then_with(|| contention_cmp(left, right))
        .then_with(|| {
            left.prediction
                .failure_permille
                .cmp(&right.prediction.failure_permille)
        })
        .then_with(|| {
            left.prediction
                .fallback_permille
                .cmp(&right.prediction.fallback_permille)
        })
        .then_with(|| {
            left.prediction
                .completion
                .total
                .p50
                .cmp(&right.prediction.completion.total.p50)
        })
        .then_with(|| {
            right
                .prediction
                .hot_state
                .class
                .preference_rank()
                .cmp(&left.prediction.hot_state.class.preference_rank())
        })
        .then_with(|| left.prediction.pool_id.cmp(&right.prediction.pool_id))
}

fn latency_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    left.prediction
        .completion
        .total
        .p50
        .cmp(&right.prediction.completion.total.p50)
        .then_with(|| {
            left.prediction
                .completion
                .total
                .p90
                .cmp(&right.prediction.completion.total.p90)
        })
        .then_with(|| {
            left.prediction
                .failure_permille
                .cmp(&right.prediction.failure_permille)
        })
        .then_with(|| {
            left.prediction
                .fallback_permille
                .cmp(&right.prediction.fallback_permille)
        })
        .then_with(|| contention_cmp(left, right))
        .then_with(|| {
            left.prediction
                .marginal_cost_microusd
                .p50
                .cmp(&right.prediction.marginal_cost_microusd.p50)
        })
        .then_with(|| left.prediction.pool_id.cmp(&right.prediction.pool_id))
}

fn pressure_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    left.prediction
        .pressure_after_admission
        .policy_rank()
        .cmp(&right.prediction.pressure_after_admission.policy_rank())
}

fn contention_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    match (&left.prediction.contention, &right.prediction.contention) {
        (Some(left), Some(right))
            if left.comparison_class == right.comparison_class
                && left.offered_tasks == right.offered_tasks =>
        {
            let left_rate =
                u128::from(left.validated_completions) * u128::from(right.elapsed_millis);
            let right_rate =
                u128::from(right.validated_completions) * u128::from(left.elapsed_millis);
            right_rate
                .cmp(&left_rate)
                .then_with(|| {
                    left.final_result_p90_millis
                        .unwrap_or(u64::MAX)
                        .cmp(&right.final_result_p90_millis.unwrap_or(u64::MAX))
                })
                .then_with(|| {
                    left.unvalidated_completions
                        .cmp(&right.unvalidated_completions)
                })
                .then_with(|| left.failures.cmp(&right.failures))
                .then_with(|| left.resets.cmp(&right.resets))
                .then_with(|| left.unknown_results.cmp(&right.unknown_results))
                .then_with(|| left.fallbacks.cmp(&right.fallbacks))
                .then_with(|| left.unfinished.cmp(&right.unfinished))
        }
        _ => Ordering::Equal,
    }
}

fn render_optional_millis(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |millis| millis.to_string())
}

fn quantiles(values: impl Iterator<Item = u64>) -> QuantilesU64 {
    let mut values: Vec<u64> = values.collect();
    values.sort_unstable();
    let p50_index = values.len().div_ceil(2).saturating_sub(1);
    let p90_index = (values.len() * 9).div_ceil(10).saturating_sub(1);
    QuantilesU64 {
        p50: values[p50_index],
        p90: values[p90_index],
    }
}

fn sum_components(values: [u64; 5]) -> u64 {
    values.into_iter().fold(0_u64, u64::saturating_add)
}

fn rate_permille(numerator: usize, denominator: usize) -> u16 {
    let value = numerator.saturating_mul(1_000) / denominator.max(1);
    u16::try_from(value).unwrap_or(1_000)
}

fn rate_permille_u64(numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 {
        return 1_000;
    }
    let value = (u128::from(numerator) * 1_000) / u128::from(denominator);
    u16::try_from(value).unwrap_or(1_000)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutingError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Display for RoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for RoutingError {}

const fn error(field: &'static str, code: &'static str, message: &'static str) -> RoutingError {
    RoutingError {
        field,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn id(value: &str) -> RoutingId {
        RoutingId::parse(value).unwrap()
    }

    fn workload() -> WorkloadClassV1 {
        WorkloadClassV1::new(
            id("project:cmux"),
            id("macos-compile-admission"),
            ComputeTrustClass::Trusted,
            id("ordinary-pr"),
            id("swift-app-edit"),
            id("compile-medium"),
            vec![id("os:macos"), id("arch:arm64")],
        )
        .unwrap()
    }

    fn pool(
        pool_id: &str,
        accounting_class: PoolAccountingClass,
        heat: HotStateClass,
    ) -> PoolCandidateV1 {
        PoolCandidateV1 {
            pool: ExecutionPoolV1::new(
                id(pool_id),
                id("macos-arm64"),
                accounting_class,
                if accounting_class == PoolAccountingClass::Owned {
                    HostPressureApplicability::LocalObserved
                } else {
                    HostPressureApplicability::NotApplicable
                },
                vec![id("arch:arm64"), id("os:macos")],
            )
            .unwrap(),
            eligibility: PoolEligibility::Eligible,
            hot_state: HotStateEvidenceV1::new(
                heat,
                (heat == HotStateClass::HotExact).then(|| id("state:main-xcode27")),
                LocalityEvidenceSource::LocalAccepted,
                None,
            )
            .unwrap(),
            pressure_after_admission: if accounting_class == PoolAccountingClass::Owned {
                HostPressureClass::Low
            } else {
                HostPressureClass::NotApplicable
            },
            allowance: None,
            contention: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn observation(
        workload: &WorkloadClassV1,
        pool_id: &str,
        heat: HotStateClass,
        age_millis: u64,
        total_execution_millis: u64,
        cost: u64,
        allowance_units: u64,
        outcome: ObservationOutcome,
    ) -> RoutingObservationV1 {
        RoutingObservationV1 {
            observation_id: id(&format!("obs:{pool_id}:{age_millis}")),
            workload: workload.clone(),
            pool_id: id(pool_id),
            execution_class: id("macos-arm64"),
            observed_at_millis: NOW - age_millis,
            hot_state: heat,
            hot_state_identity: (heat == HotStateClass::HotExact).then(|| id("state:main-xcode27")),
            timing: ObservationTimingV1 {
                queue_millis: 5_000,
                start_millis: 5_000,
                preparation_millis: 10_000,
                execution_millis: total_execution_millis,
                settlement_millis: 5_000,
            },
            outcome,
            resource: ResourceEnvelopeV1 {
                cpu_millicores: 4_000,
                memory_bytes: 8_u64 << 30,
                disk_bytes: 2_u64 << 30,
            },
            marginal_cost_microusd: cost,
            allowance_units,
        }
    }

    fn three_successes(
        workload: &WorkloadClassV1,
        pool_id: &str,
        heat: HotStateClass,
        execution_millis: u64,
        cost: u64,
        allowance_units: u64,
    ) -> Vec<RoutingObservationV1> {
        [1_000, 2_000, 3_000]
            .into_iter()
            .map(|age| {
                observation(
                    workload,
                    pool_id,
                    heat,
                    age,
                    execution_millis,
                    cost,
                    allowance_units,
                    ObservationOutcome::ValidatedSuccess,
                )
            })
            .collect()
    }

    #[test]
    fn prediction_config_cannot_enable_single_run_recommendations() {
        let workload = workload();
        let candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold);
        let observations = vec![observation(
            &workload,
            "owned",
            HotStateClass::Cold,
            1_000,
            40_000,
            0,
            0,
            ObservationOutcome::ValidatedSuccess,
        )];
        let config = PredictionConfigV1 {
            max_age_millis: 60_000,
            max_samples: 1,
            min_validated_samples: 1,
        };

        let error = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            config,
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap_err();

        assert_eq!(error.code, "routing_prediction_sample_bounds");
    }

    #[test]
    fn missing_stale_and_insufficient_evidence_are_explicit() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact),
            pool("stale", PoolAccountingClass::Owned, HotStateClass::HotExact),
            pool("thin", PoolAccountingClass::Owned, HotStateClass::HotExact),
        ];
        let mut observations =
            three_successes(&workload, "stale", HotStateClass::HotExact, 60_000, 0, 0);
        for observation in &mut observations {
            observation.observed_at_millis = NOW - 40 * 24 * 60 * 60 * 1_000;
        }
        observations.push(observation(
            &workload,
            "thin",
            HotStateClass::HotExact,
            1_000,
            60_000,
            0,
            0,
            ObservationOutcome::ValidatedSuccess,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned") && entry.reason == PoolExclusionReason::MissingEvidence
        }));
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("stale") && entry.reason == PoolExclusionReason::StaleEvidence
        }));
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("thin") && entry.reason == PoolExclusionReason::InsufficientEvidence
        }));
    }

    #[test]
    fn economy_prefers_owned_when_delay_is_inside_explicit_slack() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact),
            pool(
                "hosted",
                PoolAccountingClass::PaidBurst,
                HotStateClass::Cold,
            ),
        ];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::HotExact, 75_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "hosted",
            HotStateClass::Cold,
            45_000,
            45_000,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("owned")));
        assert!(!report.automatic_routing_eligible);
    }

    #[test]
    fn balanced_spends_when_finish_time_gain_is_material() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold),
            pool("burst", PoolAccountingClass::PaidBurst, HotStateClass::Cold),
        ];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Cold, 180_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "burst",
            HotStateClass::Cold,
            60_000,
            90_000,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::balanced(180_000, 60_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("burst")));
    }

    #[test]
    fn balanced_rejects_median_win_outside_tail_slack() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold),
            pool("burst", PoolAccountingClass::PaidBurst, HotStateClass::Cold),
        ];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Cold, 75_000, 0, 0);
        let mut burst = three_successes(&workload, "burst", HotStateClass::Cold, 20_000, 90_000, 0);
        burst[0].timing.execution_millis = 20_000;
        burst[1].timing.execution_millis = 20_000;
        burst[2].timing.execution_millis = 400_000;
        observations.extend(burst);

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::balanced(60_000, 30_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("owned")));
    }

    #[test]
    fn latency_policy_can_prove_hosted_is_the_best_choice() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::Warm),
            pool(
                "hosted",
                PoolAccountingClass::PaidBurst,
                HotStateClass::Cold,
            ),
        ];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Warm, 110_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "hosted",
            HotStateClass::Cold,
            35_000,
            70_000,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(100_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("hosted")));
    }

    #[test]
    fn exact_hot_locality_can_beat_faster_nominal_cold_pool() {
        let workload = workload();
        let candidates = vec![
            pool("hot", PoolAccountingClass::Owned, HotStateClass::HotExact),
            pool("cold", PoolAccountingClass::Owned, HotStateClass::Cold),
        ];
        let mut observations =
            three_successes(&workload, "hot", HotStateClass::HotExact, 40_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "cold",
            HotStateClass::Cold,
            85_000,
            0,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(0),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("hot")));
    }

    #[test]
    fn allowance_reserve_keeps_scarce_included_capacity_for_later() {
        let workload = workload();
        let owned = pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold);
        let mut included = pool(
            "included",
            PoolAccountingClass::MeteredIncluded,
            HotStateClass::Cold,
        );
        included.allowance = Some(AllowanceBudgetV1 {
            period_id: id("2026-09"),
            included_budget_units: 1_000,
            remaining_estimate_units: 90,
            reset_at_millis: NOW + 10_000,
            observed_consumption_units: 910,
        });
        let candidates = vec![owned, included];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Cold, 90_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "included",
            HotStateClass::Cold,
            45_000,
            0,
            50,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(120_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("owned")));
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("included") && entry.reason == PoolExclusionReason::AllowanceReserve
        }));
    }

    #[test]
    fn contention_evidence_rejects_semantic_mismatch_and_critical_pressure() {
        let workload = workload();
        let mut bad = pool("bad", PoolAccountingClass::Owned, HotStateClass::HotExact);
        bad.contention = Some(ContentionEvidenceV1 {
            comparison_class: id("cmux-macos-contention"),
            window_count: 4,
            offered_tasks: 16,
            maximum_simultaneous: 4,
            validated_completions: 14,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(20_000),
            final_result_p90_millis: Some(50_000),
            semantic_mismatches: 1,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 1,
            peak_pressure: HostPressureClass::Critical,
        });
        let observations = three_successes(&workload, "bad", HotStateClass::HotExact, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[bad],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(
            report
                .exclusions
                .iter()
                .any(|entry| { entry.reason == PoolExclusionReason::SemanticMismatch })
        );
    }

    #[test]
    fn remote_heat_is_advisory_and_never_changes_eligibility() {
        let workload = workload();
        let mut remote = pool(
            "remote",
            PoolAccountingClass::Owned,
            HotStateClass::HotExact,
        );
        remote.eligibility = PoolEligibility::Unknown;
        remote.hot_state = HotStateEvidenceV1::new(
            HotStateClass::HotExact,
            Some(id("state:main-xcode27")),
            LocalityEvidenceSource::RemoteAdvisory,
            Some(NOW + 60_000),
        )
        .unwrap();
        let observations =
            three_successes(&workload, "remote", HotStateClass::HotExact, 30_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[remote],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(0),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(
            report
                .exclusions
                .iter()
                .any(|entry| { entry.reason == PoolExclusionReason::EligibilityUnknown })
        );
    }

    #[test]
    fn unknown_locality_source_cannot_claim_exact_hot_state() {
        let error = HotStateEvidenceV1::new(
            HotStateClass::HotExact,
            Some(id("state:claimed")),
            LocalityEvidenceSource::DeclaredUnknown,
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "routing_unknown_locality_claim");
    }

    #[test]
    fn manually_built_latency_policy_requires_spend_ceiling() {
        let workload = workload();
        let candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold);
        let observations = three_successes(&workload, "owned", HotStateClass::Cold, 40_000, 0, 0);
        let mut policy = RoutingPolicyV1::latency(0);
        policy.spend_ceiling_microusd = None;

        let error = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            policy,
        )
        .unwrap_err();

        assert_eq!(error.code, "routing_latency_spend_ceiling_required");
    }

    #[test]
    fn expired_remote_heat_is_advisory_only_and_refused_as_stale() {
        let workload = workload();
        let mut remote = pool(
            "remote",
            PoolAccountingClass::PaidBurst,
            HotStateClass::HotExact,
        );
        remote.hot_state = HotStateEvidenceV1::new(
            HotStateClass::HotExact,
            Some(id("state:main-xcode27")),
            LocalityEvidenceSource::RemoteAdvisory,
            Some(NOW),
        )
        .unwrap();
        let observations = three_successes(
            &workload,
            "remote",
            HotStateClass::HotExact,
            30_000,
            10_000,
            0,
        );

        let report = recommend_ci_pool(
            &workload,
            &[remote],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(20_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("remote")
                && entry.reason == PoolExclusionReason::StaleLocalityEvidence
        }));
    }

    #[test]
    fn owned_native_linux_pilot_is_generic_and_recommendable() {
        let workload = WorkloadClassV1::new(
            id("project:glaeda"),
            id("required-verify"),
            ComputeTrustClass::Trusted,
            id("ordinary-change"),
            id("rust-workspace"),
            id("verify-medium"),
            vec![id("arch:x86_64"), id("os:linux")],
        )
        .unwrap();
        let candidate = PoolCandidateV1 {
            pool: ExecutionPoolV1::new(
                id("owned-native-linux"),
                id("native-linux-x86_64"),
                PoolAccountingClass::Owned,
                HostPressureApplicability::LocalObserved,
                vec![id("arch:x86_64"), id("os:linux")],
            )
            .unwrap(),
            eligibility: PoolEligibility::Eligible,
            hot_state: HotStateEvidenceV1::new(
                HotStateClass::Warm,
                Some(id("state:rust-main")),
                LocalityEvidenceSource::LocalAccepted,
                None,
            )
            .unwrap(),
            pressure_after_admission: HostPressureClass::Moderate,
            allowance: None,
            contention: Some(ContentionEvidenceV1 {
                comparison_class: id("glaeda-linux-contention"),
                window_count: 4,
                offered_tasks: 16,
                maximum_simultaneous: 4,
                validated_completions: 16,
                elapsed_millis: 120_000,
                final_result_p50_millis: Some(28_000),
                final_result_p90_millis: Some(41_000),
                semantic_mismatches: 0,
                unvalidated_completions: 0,
                failures: 0,
                resets: 0,
                unknown_results: 0,
                fallbacks: 0,
                unfinished: 0,
                peak_pressure: HostPressureClass::Moderate,
            }),
        };
        let mut observations = three_successes(
            &workload,
            "owned-native-linux",
            HotStateClass::Warm,
            55_000,
            0,
            0,
        );
        for observation in &mut observations {
            observation.execution_class = id("native-linux-x86_64");
            observation.hot_state_identity = Some(id("state:rust-main"));
        }

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("owned-native-linux")));
        assert_eq!(
            report.predictions[0].execution_class,
            id("native-linux-x86_64")
        );
    }

    #[test]
    fn duplicate_candidate_pool_ids_are_rejected() {
        let workload = workload();
        let candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact);
        let observations =
            three_successes(&workload, "owned", HotStateClass::HotExact, 60_000, 0, 0);

        let error = recommend_ci_pool(
            &workload,
            &[candidate.clone(), candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap_err();

        assert_eq!(error.code, "routing_duplicate_pool_id");
    }

    #[test]
    fn duplicate_observation_receipt_cannot_inflate_sample_count() {
        let workload = workload();
        let candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact);
        let one = observation(
            &workload,
            "owned",
            HotStateClass::HotExact,
            1_000,
            60_000,
            0,
            0,
            ObservationOutcome::ValidatedSuccess,
        );
        let observations = vec![one.clone(), one];

        let error = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap_err();

        assert_eq!(error.code, "routing_duplicate_observation_id");
    }

    #[test]
    fn changed_execution_class_does_not_reuse_old_pool_history() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact);
        candidate.pool.execution_class = id("macos-arm64-next");
        let observations =
            three_successes(&workload, "owned", HotStateClass::HotExact, 60_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned") && entry.reason == PoolExclusionReason::MissingEvidence
        }));
    }

    #[test]
    fn total_percentiles_preserve_observed_phase_correlation() {
        let workload = workload();
        let candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact);
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::HotExact, 0, 0, 0);
        observations[0].timing = ObservationTimingV1 {
            queue_millis: 100,
            start_millis: 0,
            preparation_millis: 0,
            execution_millis: 0,
            settlement_millis: 0,
        };
        observations[1].timing = ObservationTimingV1 {
            queue_millis: 0,
            start_millis: 0,
            preparation_millis: 0,
            execution_millis: 100,
            settlement_millis: 0,
        };
        observations[2].timing = ObservationTimingV1 {
            queue_millis: 0,
            start_millis: 0,
            preparation_millis: 0,
            execution_millis: 0,
            settlement_millis: 0,
        };

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        let prediction = &report.predictions[0];
        assert_eq!(prediction.completion.queue.p90, 100);
        assert_eq!(prediction.completion.execution.p90, 100);
        assert_eq!(prediction.completion.total.p90, 100);
    }

    #[test]
    fn failed_attempt_cost_counts_toward_spend_ceiling() {
        let workload = workload();
        let candidate = pool("burst", PoolAccountingClass::PaidBurst, HotStateClass::Cold);
        let mut observations =
            three_successes(&workload, "burst", HotStateClass::Cold, 40_000, 0, 0);
        observations.push(observation(
            &workload,
            "burst",
            HotStateClass::Cold,
            4_000,
            20_000,
            1_000,
            0,
            ObservationOutcome::Failure,
        ));
        let mut policy = RoutingPolicyV1::latency(500);
        policy.max_failure_permille = 1_000;

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            policy,
        )
        .unwrap();

        assert_eq!(report.predictions[0].marginal_cost_microusd.p90, 1_000);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("burst") && entry.reason == PoolExclusionReason::SpendCeiling
        }));
    }

    #[test]
    fn expired_allowance_period_is_refused() {
        let workload = workload();
        let mut candidate = pool(
            "included",
            PoolAccountingClass::MeteredIncluded,
            HotStateClass::Cold,
        );
        candidate.allowance = Some(AllowanceBudgetV1 {
            period_id: id("expired-period"),
            included_budget_units: 1_000,
            remaining_estimate_units: 900,
            reset_at_millis: NOW,
            observed_consumption_units: 100,
        });
        let observations =
            three_successes(&workload, "included", HotStateClass::Cold, 40_000, 0, 20);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("included") && entry.reason == PoolExclusionReason::AllowanceExpired
        }));
    }

    #[test]
    fn contention_pressure_respects_policy_ceiling() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Warm);
        candidate.contention = Some(ContentionEvidenceV1 {
            comparison_class: id("cmux-macos-contention"),
            window_count: 4,
            offered_tasks: 16,
            maximum_simultaneous: 4,
            validated_completions: 16,
            elapsed_millis: 120_000,
            final_result_p50_millis: Some(30_000),
            final_result_p90_millis: Some(45_000),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::High,
        });
        let observations = three_successes(&workload, "owned", HotStateClass::Warm, 40_000, 0, 0);
        let mut policy = RoutingPolicyV1::economy(60_000);
        policy.max_pressure = HostPressureClass::Moderate;

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            policy,
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned") && entry.reason == PoolExclusionReason::PressureAbovePolicy
        }));
    }

    #[test]
    fn fallback_rate_breaks_otherwise_equal_choices() {
        let workload = workload();
        let stable = pool("stable", PoolAccountingClass::Owned, HotStateClass::Warm);
        let flaky = pool("flaky", PoolAccountingClass::Owned, HotStateClass::Warm);
        let mut observations =
            three_successes(&workload, "stable", HotStateClass::Warm, 40_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "flaky",
            HotStateClass::Warm,
            40_000,
            0,
            0,
        ));
        observations.push(observation(
            &workload,
            "flaky",
            HotStateClass::Warm,
            4_000,
            40_000,
            0,
            0,
            ObservationOutcome::Fallback,
        ));

        let mut policy = RoutingPolicyV1::economy(60_000);
        policy.max_fallback_permille = 1_000;
        let report = recommend_ci_pool(
            &workload,
            &[stable, flaky],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            policy,
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("stable")));
    }

    #[test]
    fn fallback_rate_above_policy_abstains() {
        let workload = workload();
        let candidate = pool("flaky", PoolAccountingClass::Owned, HotStateClass::Warm);
        let mut observations =
            three_successes(&workload, "flaky", HotStateClass::Warm, 40_000, 0, 0);
        observations.push(observation(
            &workload,
            "flaky",
            HotStateClass::Warm,
            4_000,
            40_000,
            0,
            0,
            ObservationOutcome::Fallback,
        ));

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(0),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("flaky") && entry.reason == PoolExclusionReason::FallbackAbovePolicy
        }));
    }

    #[test]
    fn exact_hot_state_generation_fences_old_history() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact);
        candidate.hot_state = HotStateEvidenceV1::new(
            HotStateClass::HotExact,
            Some(id("state:new-xcode-generation")),
            LocalityEvidenceSource::LocalAccepted,
            None,
        )
        .unwrap();
        let observations =
            three_successes(&workload, "owned", HotStateClass::HotExact, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(0),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned") && entry.reason == PoolExclusionReason::MissingEvidence
        }));
    }

    #[test]
    fn exact_hot_claim_requires_state_identity() {
        let error = HotStateEvidenceV1::new(
            HotStateClass::HotExact,
            None,
            LocalityEvidenceSource::LocalAccepted,
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "routing_exact_hot_identity_missing");
    }

    #[test]
    fn recommendation_names_policy_eligible_fallback() {
        let workload = workload();
        let candidates = vec![
            pool("hot", PoolAccountingClass::Owned, HotStateClass::HotExact),
            pool("cold", PoolAccountingClass::Owned, HotStateClass::Cold),
        ];
        let mut observations =
            three_successes(&workload, "hot", HotStateClass::HotExact, 30_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "cold",
            HotStateClass::Cold,
            70_000,
            0,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(0),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("hot")));
        assert_eq!(report.fallback, Some(id("cold")));
        assert!(report.render_human().contains("fallback: cold"));
    }

    #[test]
    fn metered_included_pool_requires_allowance_accounting() {
        let workload = workload();
        let candidate = pool(
            "included",
            PoolAccountingClass::MeteredIncluded,
            HotStateClass::Cold,
        );
        let observations =
            three_successes(&workload, "included", HotStateClass::Cold, 40_000, 0, 10);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("included")
                && entry.reason == PoolExclusionReason::AllowanceRequired
        }));
    }

    #[test]
    fn non_metered_pool_rejects_allowance_accounting() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold);
        candidate.allowance = Some(AllowanceBudgetV1 {
            period_id: id("unexpected"),
            included_budget_units: 100,
            remaining_estimate_units: 90,
            reset_at_millis: NOW + 60_000,
            observed_consumption_units: 10,
        });
        let observations = three_successes(&workload, "owned", HotStateClass::Cold, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned") && entry.reason == PoolExclusionReason::UnexpectedAllowance
        }));
    }

    #[test]
    fn included_accounting_can_still_use_local_pressure_evidence() {
        let workload = workload();
        let candidate = PoolCandidateV1 {
            pool: ExecutionPoolV1::new(
                id("internal-included"),
                id("macos-arm64"),
                PoolAccountingClass::UnmeteredIncluded,
                HostPressureApplicability::LocalObserved,
                vec![id("arch:arm64"), id("os:macos")],
            )
            .unwrap(),
            eligibility: PoolEligibility::Eligible,
            hot_state: HotStateEvidenceV1::new(
                HotStateClass::Cold,
                None,
                LocalityEvidenceSource::LocalAccepted,
                None,
            )
            .unwrap(),
            pressure_after_admission: HostPressureClass::Low,
            allowance: None,
            contention: None,
        };
        let observations = three_successes(
            &workload,
            "internal-included",
            HotStateClass::Cold,
            40_000,
            0,
            0,
        );

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("internal-included")));
        assert!(!report.exclusions.iter().any(|entry| {
            entry.pool_id == id("internal-included")
                && entry.reason == PoolExclusionReason::InvalidPressureSemantics
        }));
    }

    #[test]
    fn owned_pool_cannot_bypass_pressure_with_not_applicable() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold);
        candidate.pressure_after_admission = HostPressureClass::NotApplicable;
        let observations = three_successes(&workload, "owned", HotStateClass::Cold, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned")
                && entry.reason == PoolExclusionReason::InvalidPressureSemantics
        }));
    }

    #[test]
    fn hosted_pool_cannot_invent_local_pressure() {
        let workload = workload();
        let mut candidate = pool(
            "hosted",
            PoolAccountingClass::PaidBurst,
            HotStateClass::Cold,
        );
        candidate.pressure_after_admission = HostPressureClass::Low;
        let observations =
            three_successes(&workload, "hosted", HotStateClass::Cold, 40_000, 10_000, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(20_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("hosted")
                && entry.reason == PoolExclusionReason::InvalidPressureSemantics
        }));
    }

    #[test]
    fn local_contention_cannot_hide_pressure_as_not_applicable() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Warm);
        candidate.contention = Some(ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 4,
            maximum_simultaneous: 4,
            validated_completions: 4,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(20_000),
            final_result_p90_millis: Some(30_000),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::NotApplicable,
        });
        let observations = three_successes(&workload, "owned", HotStateClass::Warm, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned")
                && entry.reason == PoolExclusionReason::InvalidPressureSemantics
        }));
    }

    #[test]
    fn nonlocal_contention_cannot_claim_local_pressure() {
        let workload = workload();
        let mut candidate = pool(
            "hosted",
            PoolAccountingClass::PaidBurst,
            HotStateClass::Cold,
        );
        candidate.contention = Some(ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 4,
            maximum_simultaneous: 4,
            validated_completions: 4,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(20_000),
            final_result_p90_millis: Some(30_000),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Low,
        });
        let observations =
            three_successes(&workload, "hosted", HotStateClass::Cold, 40_000, 10_000, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(20_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("hosted")
                && entry.reason == PoolExclusionReason::InvalidPressureSemantics
        }));
    }

    #[test]
    fn validated_contention_requires_observed_concurrency() {
        let evidence = ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 1,
            maximum_simultaneous: 0,
            validated_completions: 1,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(20_000),
            final_result_p90_millis: Some(20_000),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Low,
        };

        assert_eq!(
            evidence.validate().unwrap_err().code,
            "routing_contention_zero_concurrency_with_completion"
        );
    }

    #[test]
    fn impossible_contention_terminal_partition_is_rejected() {
        let evidence = ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 4,
            maximum_simultaneous: 2,
            validated_completions: 3,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(20_000),
            final_result_p90_millis: Some(30_000),
            semantic_mismatches: 1,
            unvalidated_completions: 0,
            failures: 1,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Low,
        };

        assert_eq!(
            evidence.validate().unwrap_err().code,
            "routing_contention_terminal_partition_invalid"
        );
    }

    #[test]
    fn missing_contention_outcomes_are_rejected() {
        let evidence = ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 4,
            maximum_simultaneous: 2,
            validated_completions: 2,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(20_000),
            final_result_p90_millis: Some(30_000),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 1,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Low,
        };

        assert_eq!(
            evidence.validate().unwrap_err().code,
            "routing_contention_terminal_partition_invalid"
        );
    }

    #[test]
    fn zero_completion_contention_window_is_valid_negative_evidence() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Warm);
        candidate.contention = Some(ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 4,
            maximum_simultaneous: 0,
            validated_completions: 0,
            elapsed_millis: 120_000,
            final_result_p50_millis: None,
            final_result_p90_millis: None,
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 4,
            peak_pressure: HostPressureClass::Moderate,
        });
        let observations = three_successes(&workload, "owned", HotStateClass::Warm, 40_000, 0, 0);

        candidate.contention.as_ref().unwrap().validate().unwrap();
        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned")
                && entry.reason == PoolExclusionReason::ReliabilityAbovePolicy
        }));
    }

    #[test]
    fn contention_latency_presence_tracks_validated_completions() {
        let no_completion = ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 1,
            maximum_simultaneous: 0,
            validated_completions: 0,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(1),
            final_result_p90_millis: Some(1),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 1,
            peak_pressure: HostPressureClass::Low,
        };
        assert_eq!(
            no_completion.validate().unwrap_err().code,
            "routing_contention_latency_without_validated_completion"
        );

        let missing_latency = ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 1,
            maximum_simultaneous: 1,
            validated_completions: 1,
            elapsed_millis: 60_000,
            final_result_p50_millis: None,
            final_result_p90_millis: None,
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Low,
        };
        assert_eq!(
            missing_latency.validate().unwrap_err().code,
            "routing_contention_validated_latency_missing"
        );
    }

    #[test]
    fn contention_failure_rate_obeys_policy_ceiling() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Warm);
        candidate.contention = Some(ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 4,
            offered_tasks: 16,
            maximum_simultaneous: 4,
            validated_completions: 14,
            elapsed_millis: 120_000,
            final_result_p50_millis: Some(30_000),
            final_result_p90_millis: Some(45_000),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 2,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Moderate,
        });
        let observations = three_successes(&workload, "owned", HotStateClass::Warm, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned")
                && entry.reason == PoolExclusionReason::ReliabilityAbovePolicy
        }));
    }

    #[test]
    fn contention_unvalidated_and_reset_outcomes_count_as_unreliable() {
        let workload = workload();
        let mut candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Warm);
        candidate.contention = Some(ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 4,
            offered_tasks: 16,
            maximum_simultaneous: 4,
            validated_completions: 14,
            elapsed_millis: 120_000,
            final_result_p50_millis: Some(30_000),
            final_result_p90_millis: Some(45_000),
            semantic_mismatches: 0,
            unvalidated_completions: 1,
            failures: 0,
            resets: 1,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Moderate,
        });
        let observations = three_successes(&workload, "owned", HotStateClass::Warm, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned")
                && entry.reason == PoolExclusionReason::ReliabilityAbovePolicy
        }));
    }

    #[test]
    fn contention_concurrency_must_fit_offered_work() {
        let evidence = ContentionEvidenceV1 {
            comparison_class: id("contention"),
            window_count: 1,
            offered_tasks: 4,
            maximum_simultaneous: 5,
            validated_completions: 4,
            elapsed_millis: 60_000,
            final_result_p50_millis: Some(20_000),
            final_result_p90_millis: Some(30_000),
            semantic_mismatches: 0,
            unvalidated_completions: 0,
            failures: 0,
            resets: 0,
            unknown_results: 0,
            fallbacks: 0,
            unfinished: 0,
            peak_pressure: HostPressureClass::Low,
        };

        assert_eq!(
            evidence.validate().unwrap_err().code,
            "routing_contention_concurrency_out_of_range"
        );
    }

    #[test]
    fn equal_timestamp_sample_truncation_is_deterministic() {
        let workload = workload();
        let candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold);
        let mut observations = Vec::new();
        for (identity, execution) in [
            ("obs:a", 10_000),
            ("obs:b", 20_000),
            ("obs:c", 30_000),
            ("obs:d", 400_000),
        ] {
            let mut value = observation(
                &workload,
                "owned",
                HotStateClass::Cold,
                1_000,
                execution,
                0,
                0,
                ObservationOutcome::ValidatedSuccess,
            );
            value.observation_id = id(identity);
            value.observed_at_millis = NOW - 1_000;
            observations.push(value);
        }
        let config = PredictionConfigV1 {
            max_age_millis: PredictionConfigV1::default().max_age_millis,
            max_samples: 3,
            min_validated_samples: 3,
        };
        let first = recommend_ci_pool(
            &workload,
            std::slice::from_ref(&candidate),
            &observations,
            NOW,
            config,
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();
        observations.reverse();
        let second = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            config,
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(
            first.predictions[0].basis.evidence_ids,
            second.predictions[0].basis.evidence_ids
        );
        assert_eq!(
            first.predictions[0].completion.total,
            second.predictions[0].completion.total
        );
        assert_eq!(
            first.predictions[0].basis.evidence_ids,
            vec![id("obs:a"), id("obs:b"), id("obs:c")]
        );
    }

    #[test]
    fn human_alternatives_omit_policy_excluded_pools() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold),
            pool("burst", PoolAccountingClass::PaidBurst, HotStateClass::Cold),
        ];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Cold, 60_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "burst",
            HotStateClass::Cold,
            30_000,
            100_000,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(50_000),
        )
        .unwrap();
        let human = report.render_human();

        assert_eq!(report.choice, Some(id("owned")));
        assert!(human.contains("excluded:\n  burst: SpendCeiling"));
        assert!(!human.contains("alternatives:\n  burst:"));
    }

    #[test]
    fn human_and_json_explain_the_same_recommendation() {
        let workload = workload();
        let candidate = pool("owned", PoolAccountingClass::Owned, HotStateClass::HotExact);
        let observations =
            three_successes(&workload, "owned", HotStateClass::HotExact, 60_000, 0, 0);
        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        let human = report.render_human();
        let json = report.render_json().unwrap();
        assert!(human.contains("choice: owned"));
        assert!(human.contains("automatic routing: disabled"));
        assert!(human.lines().count() > 8);
        assert!(!human.contains("\\n"));
        assert!(json.contains("\"choice\": \"owned\""));
        assert!(json.contains("\"automatic_routing_eligible\": false"));
        assert!(json.contains("\"authority\": \"recommendation_only\""));
        assert!(json.contains("\"evidence_ids\""));
    }
}
