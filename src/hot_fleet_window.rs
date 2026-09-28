//! Pure bounded contention-window observations for comparing hot execution policies.
//!
//! This module reduces already-owned per-task performance receipts and macOS resource reports into
//! observation-only cohort evidence. It performs no execution, admission, probing, persistence,
//! routing, optimization, lifecycle, or mutation work.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::artifact::Sha256Digest;
use crate::compute_workload::ComputeTrustClass;
use crate::execution_admission::{EpochMillis, ExecutionRequestId};
use crate::hot_execution_performance::{HotExecutionPerformanceReceipt, HotExecutionResultClass};
use crate::mac_availability::{MemoryPressure, ObservationFreshness};
use crate::macos_resource_observation::{
    MACOS_RESOURCE_OBSERVATION_SCHEMA_VERSION, MacOsResourceProblemKind, MacOsResourceReport,
};

pub const HOT_FLEET_WINDOW_SCHEMA_VERSION: u8 = 1;
pub const HOT_FLEET_COMPARISON_SCHEMA_VERSION: u8 = 1;
pub const MAX_HOT_FLEET_TASKS: usize = 16;
pub const MAX_HOT_FLEET_RESOURCE_REPORTS: usize = 64;
pub const MAX_HOT_FLEET_WINDOW_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
pub const MAX_HOT_FLEET_OBSERVED_BYTES: u64 = 1 << 50;

const MAX_TOKEN_BYTES: usize = 96;
const WORKLOAD_SET_DOMAIN: &[u8] = b"glaeda.hot_fleet_workload_set.v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WindowDocumentType {
    HotFleetWindowReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ComparisonDocumentType {
    HotFleetComparisonReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetAuthority {
    ObservationOnly,
}

impl HotFleetAuthority {
    const fn as_str(self) -> &'static str {
        "observation_only"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetArm {
    Baseline,
    Candidate,
}

impl HotFleetArm {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetDecisionKind {
    HotStatePolicy,
    ResidencyPolicy,
    ResourceProfile,
    HeavySlotPolicy,
}

impl HotFleetDecisionKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::HotStatePolicy => "hot_state_policy",
            Self::ResidencyPolicy => "residency_policy",
            Self::ResourceProfile => "resource_profile",
            Self::HeavySlotPolicy => "heavy_slot_policy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetConcurrencyBasis {
    ActiveTasks,
    HeavyExecution,
}

impl HotFleetConcurrencyBasis {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ActiveTasks => "active_tasks",
            Self::HeavyExecution => "heavy_execution",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetConcurrencyObservationSource {
    HarnessOwned,
    ReviewedObserver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetWindowCloseReason {
    AllSettled,
    Deadline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetSemanticValidation {
    Accepted,
    Rejected,
    Unobserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetResourceObservationStatus {
    Complete,
    Partial,
}

impl HotFleetResourceObservationStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetRateDirection {
    BaselineHigher,
    Equal,
    CandidateHigher,
}

impl HotFleetRateDirection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BaselineHigher => "baseline_higher",
            Self::Equal => "equal",
            Self::CandidateHigher => "candidate_higher",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetRateRangeRelationship {
    BaselineAboveCandidate,
    Overlap,
    CandidateAboveBaseline,
}

impl HotFleetRateRangeRelationship {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BaselineAboveCandidate => "baseline_above_candidate",
            Self::Overlap => "overlap",
            Self::CandidateAboveBaseline => "candidate_above_baseline",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
struct Token(String);

impl Token {
    fn parse(value: &str) -> Result<Self, HotFleetWindowError> {
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
pub struct HotFleetWindowIdentity {
    experiment_id: Token,
    block_id: Token,
    arm: HotFleetArm,
    decision_kind: HotFleetDecisionKind,
    decision_id: Token,
    validator_id: Token,
    source_class: Token,
    backend_id: Token,
    host_class: Token,
    host_instance_digest: Sha256Digest,
    arrival_pattern_id: Token,
    trust_class: ComputeTrustClass,
    non_treatment_policy_id: Token,
}

impl HotFleetWindowIdentity {
    /// Construct the declared comparison identity for one contention window.
    ///
    /// # Errors
    ///
    /// Returns an error unless every opaque text identity is a bounded canonical token.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        experiment_id: &str,
        block_id: &str,
        arm: HotFleetArm,
        decision_kind: HotFleetDecisionKind,
        decision_id: &str,
        validator_id: &str,
        source_class: &str,
        backend_id: &str,
        host_class: &str,
        host_instance_digest: Sha256Digest,
        arrival_pattern_id: &str,
        trust_class: ComputeTrustClass,
        non_treatment_policy_id: &str,
    ) -> Result<Self, HotFleetWindowError> {
        Ok(Self {
            experiment_id: Token::parse(experiment_id)?,
            block_id: Token::parse(block_id)?,
            arm,
            decision_kind,
            decision_id: Token::parse(decision_id)?,
            validator_id: Token::parse(validator_id)?,
            source_class: Token::parse(source_class)?,
            backend_id: Token::parse(backend_id)?,
            host_class: Token::parse(host_class)?,
            host_instance_digest,
            arrival_pattern_id: Token::parse(arrival_pattern_id)?,
            trust_class,
            non_treatment_policy_id: Token::parse(non_treatment_policy_id)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetWindowTiming {
    start_at: EpochMillis,
    end_at: EpochMillis,
    elapsed_millis: u64,
    budget_millis: u64,
    close_reason: HotFleetWindowCloseReason,
}

impl HotFleetWindowTiming {
    /// Construct resource-correlation bounds plus a monotonic elapsed denominator.
    ///
    /// # Errors
    ///
    /// Returns an error for time reversal, zero or oversized durations, elapsed time beyond the
    /// fixed budget, or a deadline close before the fixed budget is consumed.
    pub fn new(
        start_at: EpochMillis,
        end_at: EpochMillis,
        elapsed_millis: u64,
        budget_millis: u64,
        close_reason: HotFleetWindowCloseReason,
    ) -> Result<Self, HotFleetWindowError> {
        if end_at.get() < start_at.get() {
            return Err(error(
                "timing.end_at",
                "hot_fleet_time_reversal",
                "hot-fleet window end time cannot precede start time",
            ));
        }
        if elapsed_millis == 0
            || budget_millis == 0
            || elapsed_millis > budget_millis
            || budget_millis > MAX_HOT_FLEET_WINDOW_MILLIS
        {
            return Err(error(
                "timing",
                "hot_fleet_duration_out_of_range",
                "hot-fleet elapsed time and budget must be positive and bounded",
            ));
        }
        if end_at.get() - start_at.get() > MAX_HOT_FLEET_WINDOW_MILLIS {
            return Err(error(
                "timing",
                "hot_fleet_correlation_span_out_of_range",
                "hot-fleet resource-correlation span exceeds the bounded window",
            ));
        }
        if close_reason == HotFleetWindowCloseReason::Deadline && elapsed_millis != budget_millis {
            return Err(error(
                "timing.elapsed_millis",
                "hot_fleet_deadline_before_budget",
                "deadline-closed hot-fleet windows must reach the fixed budget",
            ));
        }
        Ok(Self {
            start_at,
            end_at,
            elapsed_millis,
            budget_millis,
            close_reason,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetConcurrencyObservation {
    basis: HotFleetConcurrencyBasis,
    maximum_simultaneous: u8,
    source: HotFleetConcurrencyObservationSource,
}

impl HotFleetConcurrencyObservation {
    /// Construct one already-observed concurrency maximum.
    ///
    /// `HarnessOwned` is suitable for a synthetic or frozen benchmark. Natural heavy-execution
    /// claims require a separately reviewed observer and should use `ReviewedObserver`.
    ///
    /// # Errors
    ///
    /// Returns an error when the maximum exceeds the bounded cohort size.
    pub fn new(
        basis: HotFleetConcurrencyBasis,
        maximum_simultaneous: u8,
        source: HotFleetConcurrencyObservationSource,
    ) -> Result<Self, HotFleetWindowError> {
        if usize::from(maximum_simultaneous) > MAX_HOT_FLEET_TASKS {
            return Err(error(
                "concurrency.maximum_simultaneous",
                "hot_fleet_concurrency_out_of_range",
                "hot-fleet maximum simultaneous work exceeds the bounded cohort size",
            ));
        }
        Ok(Self {
            basis,
            maximum_simultaneous,
            source,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotFleetAttemptEvidence {
    request_id: ExecutionRequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    performance_receipt: Option<HotExecutionPerformanceReceipt>,
    semantic_validation: HotFleetSemanticValidation,
    fallback_observed: bool,
}

impl HotFleetAttemptEvidence {
    /// Bind one actual attempt to performance and semantic-validation evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when semantic acceptance or rejection is claimed without a successful
    /// performance receipt.
    pub fn new(
        request_id: ExecutionRequestId,
        performance_receipt: Option<HotExecutionPerformanceReceipt>,
        semantic_validation: HotFleetSemanticValidation,
        fallback_observed: bool,
    ) -> Result<Self, HotFleetWindowError> {
        if semantic_validation != HotFleetSemanticValidation::Unobserved
            && !matches!(
                performance_receipt
                    .as_ref()
                    .map(HotExecutionPerformanceReceipt::result),
                Some(HotExecutionResultClass::Succeeded)
            )
        {
            return Err(error(
                "members.attempt.semantic_validation",
                "hot_fleet_validation_without_success",
                "semantic validation may classify only a successful performance receipt",
            ));
        }
        Ok(Self {
            request_id,
            performance_receipt,
            semantic_validation,
            fallback_observed,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotFleetWorkItem {
    work_id: Token,
    workload_identity: Sha256Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt: Option<HotFleetAttemptEvidence>,
}

impl HotFleetWorkItem {
    /// Construct one offered item from an experiment-local ID and exact semantic digest.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid experiment-local work ID.
    pub fn new(
        work_id: &str,
        workload_identity: Sha256Digest,
        attempt: Option<HotFleetAttemptEvidence>,
    ) -> Result<Self, HotFleetWindowError> {
        Ok(Self {
            work_id: Token::parse(work_id)?,
            workload_identity,
            attempt,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetWindowCounts {
    pub offered: u8,
    pub maximum_simultaneous: u8,
    pub started: u8,
    pub settled: u8,
    pub validated_completions: u8,
    pub semantic_mismatches: u8,
    pub unvalidated_completions: u8,
    pub failures: u8,
    pub resets: u8,
    pub unknown_results: u8,
    pub fallbacks: u8,
    pub unfinished: u8,
    pub member_receipt_count: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotFleetResourceSummary {
    pub status: HotFleetResourceObservationStatus,
    pub report_count: u8,
    pub max_observed_memory_pressure: MemoryPressure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_observed_swap_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_observed_swap_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_swap_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_observed_aggregate_lima_rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetValidatedRate {
    pub validated_completions: u8,
    pub elapsed_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotFleetWindowReceiptV1 {
    document_type: WindowDocumentType,
    schema_version: u8,
    authority: HotFleetAuthority,
    identity: HotFleetWindowIdentity,
    workload_set_digest: Sha256Digest,
    timing: HotFleetWindowTiming,
    concurrency: HotFleetConcurrencyObservation,
    counts: HotFleetWindowCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_result_p50_millis: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_result_p90_millis: Option<u64>,
    resources: HotFleetResourceSummary,
    members: Vec<HotFleetWorkItem>,
}

impl HotFleetWindowReceiptV1 {
    /// Reduce complete offered/member evidence into one bounded observation-only window.
    ///
    /// Every headline task count is derived here from member evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate IDs, invalid count relationships, missing validated latency,
    /// validated latency beyond the contention window, incomplete all-settled closure, or invalid
    /// resource samples.
    pub fn new(
        identity: HotFleetWindowIdentity,
        timing: HotFleetWindowTiming,
        concurrency: HotFleetConcurrencyObservation,
        members: Vec<HotFleetWorkItem>,
        resource_reports: &[MacOsResourceReport],
    ) -> Result<Self, HotFleetWindowError> {
        if members.is_empty() || members.len() > MAX_HOT_FLEET_TASKS {
            return Err(error(
                "members",
                "hot_fleet_member_count_out_of_range",
                "hot-fleet windows require between one and sixteen offered work items",
            ));
        }

        let mut work_ids = BTreeSet::new();
        let mut request_ids = BTreeSet::new();
        let mut started = 0_u8;
        let mut validated = 0_u8;
        let mut mismatches = 0_u8;
        let mut unvalidated = 0_u8;
        let mut failures = 0_u8;
        let mut resets = 0_u8;
        let mut unknown_results = 0_u8;
        let mut fallbacks = 0_u8;
        let mut receipt_count = 0_u8;
        let mut latencies = Vec::new();

        for member in &members {
            if !work_ids.insert(member.work_id.as_str()) {
                return Err(error(
                    "members.work_id",
                    "duplicate_hot_fleet_work_id",
                    "offered work IDs must be unique within one hot-fleet window",
                ));
            }
            let Some(attempt) = member.attempt.as_ref() else {
                continue;
            };
            started += 1;
            if !request_ids.insert(attempt.request_id.as_str()) {
                return Err(error(
                    "members.attempt.request_id",
                    "duplicate_hot_fleet_request_id",
                    "execution request IDs must be unique within one hot-fleet window",
                ));
            }
            if attempt.fallback_observed {
                fallbacks += 1;
            }
            let Some(receipt) = attempt.performance_receipt.as_ref() else {
                continue;
            };
            receipt_count += 1;
            match receipt.result() {
                HotExecutionResultClass::Succeeded => match attempt.semantic_validation {
                    HotFleetSemanticValidation::Accepted => {
                        let latency = receipt
                            .milestones()
                            .final_relevant_result_millis()
                            .ok_or_else(|| {
                                error(
                                    "members.attempt.performance_receipt.milestones",
                                    "validated_hot_fleet_latency_missing",
                                    "validated completion requires final-result latency",
                                )
                            })?;
                        if latency > timing.elapsed_millis {
                            return Err(error(
                                "members.attempt.performance_receipt.milestones",
                                "validated_hot_fleet_latency_outside_window",
                                "validated final-result latency exceeds the contention window",
                            ));
                        }
                        validated += 1;
                        latencies.push(latency);
                    }
                    HotFleetSemanticValidation::Rejected => mismatches += 1,
                    HotFleetSemanticValidation::Unobserved => unvalidated += 1,
                },
                HotExecutionResultClass::Failed | HotExecutionResultClass::Canceled => {
                    failures += 1;
                }
                HotExecutionResultClass::ResetRequired => resets += 1,
                HotExecutionResultClass::Unknown => unknown_results += 1,
            }
        }

        if concurrency.maximum_simultaneous > started {
            return Err(error(
                "concurrency.maximum_simultaneous",
                "hot_fleet_concurrency_exceeds_started",
                "maximum simultaneous work cannot exceed started work",
            ));
        }

        let offered = members.len() as u8;
        let settled = validated + mismatches + unvalidated + failures + resets + unknown_results;
        let unfinished = offered.checked_sub(settled).ok_or_else(|| {
            error(
                "counts",
                "hot_fleet_terminal_partition_invalid",
                "terminal outcomes must form a disjoint subset of offered work",
            )
        })?;
        if timing.close_reason == HotFleetWindowCloseReason::AllSettled && unfinished != 0 {
            return Err(error(
                "timing.close_reason",
                "hot_fleet_all_settled_has_unfinished_work",
                "all-settled windows cannot contain unfinished work",
            ));
        }

        latencies.sort_unstable();
        let workload_set_digest = workload_set_digest(&members)?;
        let resources = reduce_resources(timing, resource_reports)?;
        Ok(Self {
            document_type: WindowDocumentType::HotFleetWindowReceipt,
            schema_version: HOT_FLEET_WINDOW_SCHEMA_VERSION,
            authority: HotFleetAuthority::ObservationOnly,
            identity,
            workload_set_digest,
            timing,
            concurrency,
            counts: HotFleetWindowCounts {
                offered,
                maximum_simultaneous: concurrency.maximum_simultaneous,
                started,
                settled,
                validated_completions: validated,
                semantic_mismatches: mismatches,
                unvalidated_completions: unvalidated,
                failures,
                resets,
                unknown_results,
                fallbacks,
                unfinished,
                member_receipt_count: receipt_count,
            },
            final_result_p50_millis: nearest_rank(&latencies, 50),
            final_result_p90_millis: nearest_rank(&latencies, 90),
            resources,
            members,
        })
    }

    #[must_use]
    pub const fn counts(&self) -> HotFleetWindowCounts {
        self.counts
    }

    #[must_use]
    pub const fn resources(&self) -> &HotFleetResourceSummary {
        &self.resources
    }

    #[must_use]
    pub const fn workload_set_digest(&self) -> &Sha256Digest {
        &self.workload_set_digest
    }

    #[must_use]
    pub const fn final_result_p50_millis(&self) -> Option<u64> {
        self.final_result_p50_millis
    }

    #[must_use]
    pub const fn final_result_p90_millis(&self) -> Option<u64> {
        self.final_result_p90_millis
    }

    #[must_use]
    pub const fn validated_rate(&self) -> HotFleetValidatedRate {
        HotFleetValidatedRate {
            validated_completions: self.counts.validated_completions,
            elapsed_millis: self.timing.elapsed_millis,
        }
    }

    /// Render a stable human summary from this typed receipt.
    #[must_use]
    pub fn render_human(&self) -> String {
        format!(
            "hot fleet window\nauthority: {}\nexperiment: {}\nblock: {}\narm: {}\ndecision: {}/{}\nworkload set: {}\noffered: {}\nstarted: {}\nsettled: {}\nvalidated: {}\nsemantic mismatches: {}\nunvalidated completions: {}\nfailures: {}\nresets: {}\nunknown results: {}\nfallbacks: {}\nunfinished: {}\nmaximum simultaneous: {} ({})\nelapsed: {} ms\nfinal-result p50: {}\nfinal-result p90: {}\nresource status: {}\nmax-observed memory pressure: {}\nfirst/max/last observed swap: {}/{}/{}\nmax-observed aggregate Lima RSS: {}\n",
            self.authority.as_str(),
            self.identity.experiment_id.as_str(),
            self.identity.block_id.as_str(),
            self.identity.arm.as_str(),
            self.identity.decision_kind.as_str(),
            self.identity.decision_id.as_str(),
            self.workload_set_digest.as_str(),
            self.counts.offered,
            self.counts.started,
            self.counts.settled,
            self.counts.validated_completions,
            self.counts.semantic_mismatches,
            self.counts.unvalidated_completions,
            self.counts.failures,
            self.counts.resets,
            self.counts.unknown_results,
            self.counts.fallbacks,
            self.counts.unfinished,
            self.counts.maximum_simultaneous,
            self.concurrency.basis.as_str(),
            self.timing.elapsed_millis,
            render_optional_millis(self.final_result_p50_millis),
            render_optional_millis(self.final_result_p90_millis),
            self.resources.status.as_str(),
            pressure_str(self.resources.max_observed_memory_pressure),
            render_optional_bytes(self.resources.first_observed_swap_bytes),
            render_optional_bytes(self.resources.max_observed_swap_bytes),
            render_optional_bytes(self.resources.last_observed_swap_bytes),
            render_optional_bytes(self.resources.max_observed_aggregate_lima_rss_bytes),
        )
    }

    /// Render deterministic pretty JSON from this typed receipt.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed model fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetRateRange {
    pub lower: HotFleetValidatedRate,
    pub upper: HotFleetValidatedRate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetArmResourceSummary {
    pub max_observed_memory_pressure: MemoryPressure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_observed_swap_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_observed_aggregate_lima_rss_bytes: Option<u64>,
    pub all_resource_observations_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotFleetComparisonReportV1 {
    document_type: ComparisonDocumentType,
    pub schema_version: u8,
    pub authority: HotFleetAuthority,
    experiment_id: Token,
    pub decision_kind: HotFleetDecisionKind,
    baseline_decision_id: Token,
    candidate_decision_id: Token,
    workload_set_digest: Sha256Digest,
    pub first_pair_direction: HotFleetRateDirection,
    pub second_pair_direction: HotFleetRateDirection,
    pub directions_consistent: bool,
    pub baseline_rate_range: HotFleetRateRange,
    pub candidate_rate_range: HotFleetRateRange,
    pub rate_range_relationship: HotFleetRateRangeRelationship,
    pub baseline_resources: HotFleetArmResourceSummary,
    pub candidate_resources: HotFleetArmResourceSummary,
}

impl HotFleetComparisonReportV1 {
    /// Render the same descriptive A-B-B-A report carried by the JSON model.
    #[must_use]
    pub fn render_human(&self) -> String {
        format!(
            "hot fleet comparison\nauthority: {}\nexperiment: {}\ndecision kind: {}\nbaseline decision: {}\ncandidate decision: {}\nworkload set: {}\nfirst pair: {}\nsecond pair: {}\ndirections consistent: {}\nbaseline rate range: {}\ncandidate rate range: {}\nrate ranges: {}\nbaseline max pressure: {}\ncandidate max pressure: {}\nbaseline max swap: {}\ncandidate max swap: {}\n",
            self.authority.as_str(),
            self.experiment_id.as_str(),
            self.decision_kind.as_str(),
            self.baseline_decision_id.as_str(),
            self.candidate_decision_id.as_str(),
            self.workload_set_digest.as_str(),
            self.first_pair_direction.as_str(),
            self.second_pair_direction.as_str(),
            self.directions_consistent,
            render_rate_range(self.baseline_rate_range),
            render_rate_range(self.candidate_rate_range),
            self.rate_range_relationship.as_str(),
            pressure_str(self.baseline_resources.max_observed_memory_pressure),
            pressure_str(self.candidate_resources.max_observed_memory_pressure),
            render_optional_bytes(self.baseline_resources.max_observed_swap_bytes),
            render_optional_bytes(self.candidate_resources.max_observed_swap_bytes),
        )
    }

    /// Render deterministic pretty JSON from this typed comparison report.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed model fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotFleetComparisonRefusalReason {
    ArmOrder,
    ExperimentIdentity,
    DuplicateBlockIdentity,
    WorkloadSet,
    ValidatorIdentity,
    SourceClass,
    BackendIdentity,
    HostClass,
    HostInstance,
    ArrivalPattern,
    TrustClass,
    NonTreatmentPolicy,
    WindowBudget,
    ConcurrencyBasis,
    DecisionKind,
    BaselineDecisionIdentity,
    CandidateDecisionIdentity,
    TreatmentUnchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotFleetComparisonRefusal {
    pub reason: HotFleetComparisonRefusalReason,
    message: &'static str,
}

impl fmt::Display for HotFleetComparisonRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for HotFleetComparisonRefusal {}

/// Compare four exact contention windows in baseline-candidate-candidate-baseline order.
///
/// This reducer reports exact pair directions, rate ranges, range overlap or separation, and
/// resource observations. It grants no execution, routing, admission, residency, or policy-change
/// authority.
///
/// # Errors
///
/// Returns an explicit refusal reason when the four windows are not exact comparable work.
pub fn compare_hot_fleet_abba(
    windows: [&HotFleetWindowReceiptV1; 4],
) -> Result<HotFleetComparisonReportV1, HotFleetComparisonRefusal> {
    let [a1, b1, b2, a2] = windows;
    if [
        a1.identity.arm,
        b1.identity.arm,
        b2.identity.arm,
        a2.identity.arm,
    ] != [
        HotFleetArm::Baseline,
        HotFleetArm::Candidate,
        HotFleetArm::Candidate,
        HotFleetArm::Baseline,
    ] {
        return Err(refusal(
            HotFleetComparisonRefusalReason::ArmOrder,
            "hot-fleet comparison requires baseline-candidate-candidate-baseline order",
        ));
    }

    require_equal(
        [
            a1.identity.experiment_id.as_str(),
            b1.identity.experiment_id.as_str(),
            b2.identity.experiment_id.as_str(),
            a2.identity.experiment_id.as_str(),
        ],
        HotFleetComparisonRefusalReason::ExperimentIdentity,
        "experiment identity differs across crossover windows",
    )?;

    let mut blocks = BTreeSet::new();
    for block in [
        a1.identity.block_id.as_str(),
        b1.identity.block_id.as_str(),
        b2.identity.block_id.as_str(),
        a2.identity.block_id.as_str(),
    ] {
        if !blocks.insert(block) {
            return Err(refusal(
                HotFleetComparisonRefusalReason::DuplicateBlockIdentity,
                "crossover windows require four distinct block identities",
            ));
        }
    }

    require_equal(
        [
            a1.workload_set_digest.as_str(),
            b1.workload_set_digest.as_str(),
            b2.workload_set_digest.as_str(),
            a2.workload_set_digest.as_str(),
        ],
        HotFleetComparisonRefusalReason::WorkloadSet,
        "workload set differs across crossover windows",
    )?;
    require_equal(
        [
            a1.identity.validator_id.as_str(),
            b1.identity.validator_id.as_str(),
            b2.identity.validator_id.as_str(),
            a2.identity.validator_id.as_str(),
        ],
        HotFleetComparisonRefusalReason::ValidatorIdentity,
        "semantic validator identity differs across crossover windows",
    )?;
    require_equal(
        [
            a1.identity.source_class.as_str(),
            b1.identity.source_class.as_str(),
            b2.identity.source_class.as_str(),
            a2.identity.source_class.as_str(),
        ],
        HotFleetComparisonRefusalReason::SourceClass,
        "source class differs across crossover windows",
    )?;
    require_equal(
        [
            a1.identity.backend_id.as_str(),
            b1.identity.backend_id.as_str(),
            b2.identity.backend_id.as_str(),
            a2.identity.backend_id.as_str(),
        ],
        HotFleetComparisonRefusalReason::BackendIdentity,
        "backend identity differs across crossover windows",
    )?;
    require_equal(
        [
            a1.identity.host_class.as_str(),
            b1.identity.host_class.as_str(),
            b2.identity.host_class.as_str(),
            a2.identity.host_class.as_str(),
        ],
        HotFleetComparisonRefusalReason::HostClass,
        "host class differs across crossover windows",
    )?;
    require_equal(
        [
            a1.identity.host_instance_digest.as_str(),
            b1.identity.host_instance_digest.as_str(),
            b2.identity.host_instance_digest.as_str(),
            a2.identity.host_instance_digest.as_str(),
        ],
        HotFleetComparisonRefusalReason::HostInstance,
        "host instance differs across crossover windows",
    )?;
    require_equal(
        [
            a1.identity.arrival_pattern_id.as_str(),
            b1.identity.arrival_pattern_id.as_str(),
            b2.identity.arrival_pattern_id.as_str(),
            a2.identity.arrival_pattern_id.as_str(),
        ],
        HotFleetComparisonRefusalReason::ArrivalPattern,
        "arrival pattern differs across crossover windows",
    )?;
    require_equal(
        [
            a1.identity.non_treatment_policy_id.as_str(),
            b1.identity.non_treatment_policy_id.as_str(),
            b2.identity.non_treatment_policy_id.as_str(),
            a2.identity.non_treatment_policy_id.as_str(),
        ],
        HotFleetComparisonRefusalReason::NonTreatmentPolicy,
        "non-treatment policy differs across crossover windows",
    )?;

    if ![b1, b2, a2]
        .iter()
        .all(|window| window.identity.trust_class == a1.identity.trust_class)
    {
        return Err(refusal(
            HotFleetComparisonRefusalReason::TrustClass,
            "trust class differs across crossover windows",
        ));
    }
    if ![b1, b2, a2]
        .iter()
        .all(|window| window.timing.budget_millis == a1.timing.budget_millis)
    {
        return Err(refusal(
            HotFleetComparisonRefusalReason::WindowBudget,
            "fixed window budget differs across crossover windows",
        ));
    }
    if ![b1, b2, a2]
        .iter()
        .all(|window| window.concurrency.basis == a1.concurrency.basis)
    {
        return Err(refusal(
            HotFleetComparisonRefusalReason::ConcurrencyBasis,
            "concurrency basis differs across crossover windows",
        ));
    }
    if ![b1, b2, a2]
        .iter()
        .all(|window| window.identity.decision_kind == a1.identity.decision_kind)
    {
        return Err(refusal(
            HotFleetComparisonRefusalReason::DecisionKind,
            "treatment decision kind differs across crossover windows",
        ));
    }
    if a1.identity.decision_id != a2.identity.decision_id {
        return Err(refusal(
            HotFleetComparisonRefusalReason::BaselineDecisionIdentity,
            "baseline decision identity differs across baseline windows",
        ));
    }
    if b1.identity.decision_id != b2.identity.decision_id {
        return Err(refusal(
            HotFleetComparisonRefusalReason::CandidateDecisionIdentity,
            "candidate decision identity differs across candidate windows",
        ));
    }
    if a1.identity.decision_id == b1.identity.decision_id {
        return Err(refusal(
            HotFleetComparisonRefusalReason::TreatmentUnchanged,
            "baseline and candidate must declare different treatment identities",
        ));
    }

    let a1_rate = a1.validated_rate();
    let b1_rate = b1.validated_rate();
    let b2_rate = b2.validated_rate();
    let a2_rate = a2.validated_rate();
    let first_pair_direction = pair_direction(a1_rate, b1_rate);
    let second_pair_direction = pair_direction(a2_rate, b2_rate);
    let baseline_rate_range = rate_range(a1_rate, a2_rate);
    let candidate_rate_range = rate_range(b1_rate, b2_rate);

    Ok(HotFleetComparisonReportV1 {
        document_type: ComparisonDocumentType::HotFleetComparisonReport,
        schema_version: HOT_FLEET_COMPARISON_SCHEMA_VERSION,
        authority: HotFleetAuthority::ObservationOnly,
        experiment_id: a1.identity.experiment_id.clone(),
        decision_kind: a1.identity.decision_kind,
        baseline_decision_id: a1.identity.decision_id.clone(),
        candidate_decision_id: b1.identity.decision_id.clone(),
        workload_set_digest: a1.workload_set_digest.clone(),
        first_pair_direction,
        second_pair_direction,
        directions_consistent: first_pair_direction == second_pair_direction,
        baseline_rate_range,
        candidate_rate_range,
        rate_range_relationship: range_relationship(baseline_rate_range, candidate_rate_range),
        baseline_resources: arm_resources([a1, a2]),
        candidate_resources: arm_resources([b1, b2]),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotFleetWindowError {
    pub field: &'static str,
    pub code: &'static str,
    message: &'static str,
}

impl fmt::Display for HotFleetWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for HotFleetWindowError {}

fn workload_set_digest(members: &[HotFleetWorkItem]) -> Result<Sha256Digest, HotFleetWindowError> {
    let mut canonical = members.iter().collect::<Vec<_>>();
    canonical.sort_by(|left, right| left.work_id.cmp(&right.work_id));
    let mut hasher = Sha256::new();
    hasher.update(WORKLOAD_SET_DOMAIN);
    for member in canonical {
        let work_id = member.work_id.as_str().as_bytes();
        hasher.update((work_id.len() as u64).to_be_bytes());
        hasher.update(work_id);
        let identity = member.workload_identity.as_str().as_bytes();
        hasher.update((identity.len() as u64).to_be_bytes());
        hasher.update(identity);
    }
    let digest = hasher.finalize();
    let mut text = String::with_capacity(71);
    text.push_str("sha256:");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&text).map_err(|_| {
        error(
            "workload_set_digest",
            "hot_fleet_workload_digest_failed",
            "canonical hot-fleet workload-set digest construction failed",
        )
    })
}

fn reduce_resources(
    timing: HotFleetWindowTiming,
    reports: &[MacOsResourceReport],
) -> Result<HotFleetResourceSummary, HotFleetWindowError> {
    if reports.len() > MAX_HOT_FLEET_RESOURCE_REPORTS {
        return Err(error(
            "resource_reports",
            "hot_fleet_resource_report_count_out_of_range",
            "hot-fleet resource report count exceeds the bounded maximum",
        ));
    }
    if reports.is_empty() {
        return Ok(HotFleetResourceSummary {
            status: HotFleetResourceObservationStatus::Partial,
            report_count: 0,
            max_observed_memory_pressure: MemoryPressure::Unknown,
            first_observed_swap_bytes: None,
            max_observed_swap_bytes: None,
            last_observed_swap_bytes: None,
            max_observed_aggregate_lima_rss_bytes: None,
        });
    }

    let mut previous_at = None;
    let mut complete = true;
    let mut max_pressure = MemoryPressure::Unknown;
    let mut max_swap = None;
    let mut max_lima_rss = None;

    for report in reports {
        if report.schema_version != MACOS_RESOURCE_OBSERVATION_SCHEMA_VERSION {
            return Err(error(
                "resource_reports.schema_version",
                "hot_fleet_resource_schema_mismatch",
                "hot-fleet resource report uses an unsupported schema version",
            ));
        }
        if report.observed_at_millis < timing.start_at.get()
            || report.observed_at_millis > timing.end_at.get()
        {
            return Err(error(
                "resource_reports.observed_at_millis",
                "hot_fleet_resource_outside_window",
                "resource report timestamp falls outside the declared window",
            ));
        }
        if previous_at.is_some_and(|at| report.observed_at_millis <= at) {
            return Err(error(
                "resource_reports.observed_at_millis",
                "hot_fleet_resource_time_order_invalid",
                "resource report timestamps must be strictly increasing",
            ));
        }
        previous_at = Some(report.observed_at_millis);

        if report.freshness != ObservationFreshness::Fresh {
            complete = false;
        }
        if report.memory_pressure == MemoryPressure::Unknown {
            complete = false;
        } else if pressure_rank(report.memory_pressure) > pressure_rank(max_pressure) {
            max_pressure = report.memory_pressure;
        }

        if let Some(swap) = report.swap.as_ref() {
            if [swap.total_bytes, swap.used_bytes, swap.free_bytes]
                .into_iter()
                .any(|value| value > MAX_HOT_FLEET_OBSERVED_BYTES)
                || swap.used_bytes > swap.total_bytes
                || swap.free_bytes > swap.total_bytes
            {
                return Err(resource_bytes_out_of_range());
            }
            max_swap =
                Some(max_swap.map_or(swap.used_bytes, |value: u64| value.max(swap.used_bytes)));
        } else {
            complete = false;
        }

        let lima_complete = !report.problems.iter().any(|problem| {
            matches!(
                problem,
                MacOsResourceProblemKind::LimaProcessObservationUnavailable
                    | MacOsResourceProblemKind::LimaProcessListTruncated
            )
        });
        if lima_complete {
            let aggregate = report
                .lima_processes
                .iter()
                .try_fold(0_u64, |total, process| {
                    total
                        .checked_add(process.rss_bytes)
                        .ok_or_else(resource_bytes_out_of_range)
                })?;
            if aggregate > MAX_HOT_FLEET_OBSERVED_BYTES {
                return Err(resource_bytes_out_of_range());
            }
            max_lima_rss = Some(max_lima_rss.map_or(aggregate, |value: u64| value.max(aggregate)));
        } else {
            complete = false;
        }
    }

    Ok(HotFleetResourceSummary {
        status: if complete {
            HotFleetResourceObservationStatus::Complete
        } else {
            HotFleetResourceObservationStatus::Partial
        },
        report_count: reports.len() as u8,
        max_observed_memory_pressure: max_pressure,
        first_observed_swap_bytes: reports
            .first()
            .and_then(|report| report.swap.as_ref().map(|swap| swap.used_bytes)),
        max_observed_swap_bytes: max_swap,
        last_observed_swap_bytes: reports
            .last()
            .and_then(|report| report.swap.as_ref().map(|swap| swap.used_bytes)),
        max_observed_aggregate_lima_rss_bytes: max_lima_rss,
    })
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

fn rate_cmp(left: HotFleetValidatedRate, right: HotFleetValidatedRate) -> Ordering {
    let left_cross = u128::from(left.validated_completions) * u128::from(right.elapsed_millis);
    let right_cross = u128::from(right.validated_completions) * u128::from(left.elapsed_millis);
    left_cross.cmp(&right_cross)
}

fn pair_direction(
    baseline: HotFleetValidatedRate,
    candidate: HotFleetValidatedRate,
) -> HotFleetRateDirection {
    match rate_cmp(candidate, baseline) {
        Ordering::Less => HotFleetRateDirection::BaselineHigher,
        Ordering::Equal => HotFleetRateDirection::Equal,
        Ordering::Greater => HotFleetRateDirection::CandidateHigher,
    }
}

fn rate_range(first: HotFleetValidatedRate, second: HotFleetValidatedRate) -> HotFleetRateRange {
    if rate_cmp(first, second).is_le() {
        HotFleetRateRange {
            lower: first,
            upper: second,
        }
    } else {
        HotFleetRateRange {
            lower: second,
            upper: first,
        }
    }
}

fn range_relationship(
    baseline: HotFleetRateRange,
    candidate: HotFleetRateRange,
) -> HotFleetRateRangeRelationship {
    if rate_cmp(baseline.upper, candidate.lower).is_lt() {
        HotFleetRateRangeRelationship::CandidateAboveBaseline
    } else if rate_cmp(candidate.upper, baseline.lower).is_lt() {
        HotFleetRateRangeRelationship::BaselineAboveCandidate
    } else {
        HotFleetRateRangeRelationship::Overlap
    }
}

fn arm_resources(windows: [&HotFleetWindowReceiptV1; 2]) -> HotFleetArmResourceSummary {
    let [first, second] = windows;
    HotFleetArmResourceSummary {
        max_observed_memory_pressure: max_pressure(
            first.resources.max_observed_memory_pressure,
            second.resources.max_observed_memory_pressure,
        ),
        max_observed_swap_bytes: max_optional(
            first.resources.max_observed_swap_bytes,
            second.resources.max_observed_swap_bytes,
        ),
        max_observed_aggregate_lima_rss_bytes: max_optional(
            first.resources.max_observed_aggregate_lima_rss_bytes,
            second.resources.max_observed_aggregate_lima_rss_bytes,
        ),
        all_resource_observations_complete: first.resources.status
            == HotFleetResourceObservationStatus::Complete
            && second.resources.status == HotFleetResourceObservationStatus::Complete,
    }
}

fn max_pressure(first: MemoryPressure, second: MemoryPressure) -> MemoryPressure {
    if pressure_rank(first) >= pressure_rank(second) {
        first
    } else {
        second
    }
}

fn pressure_rank(pressure: MemoryPressure) -> u8 {
    match pressure {
        MemoryPressure::Unknown => 0,
        MemoryPressure::Normal => 1,
        MemoryPressure::Elevated => 2,
        MemoryPressure::Critical => 3,
    }
}

const fn pressure_str(pressure: MemoryPressure) -> &'static str {
    match pressure {
        MemoryPressure::Normal => "normal",
        MemoryPressure::Elevated => "elevated",
        MemoryPressure::Critical => "critical",
        MemoryPressure::Unknown => "unknown",
    }
}

fn max_optional(first: Option<u64>, second: Option<u64>) -> Option<u64> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.max(second)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn require_equal<T: Copy + PartialEq>(
    values: [T; 4],
    reason: HotFleetComparisonRefusalReason,
    message: &'static str,
) -> Result<(), HotFleetComparisonRefusal> {
    if values[1..].iter().any(|value| *value != values[0]) {
        return Err(refusal(reason, message));
    }
    Ok(())
}

const fn refusal(
    reason: HotFleetComparisonRefusalReason,
    message: &'static str,
) -> HotFleetComparisonRefusal {
    HotFleetComparisonRefusal { reason, message }
}

const fn error(
    field: &'static str,
    code: &'static str,
    message: &'static str,
) -> HotFleetWindowError {
    HotFleetWindowError {
        field,
        code,
        message,
    }
}

const fn invalid_token() -> HotFleetWindowError {
    error(
        "identity",
        "invalid_hot_fleet_token",
        "hot-fleet identity must be a bounded lowercase ASCII token",
    )
}

const fn resource_bytes_out_of_range() -> HotFleetWindowError {
    error(
        "resource_reports",
        "hot_fleet_resource_bytes_out_of_range",
        "hot-fleet resource byte observation exceeds the bounded maximum",
    )
}

fn render_optional_millis(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| format!("{value} ms"))
}

fn render_optional_bytes(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| format!("{value} bytes"))
}

fn render_rate_range(range: HotFleetRateRange) -> String {
    format!(
        "{}/{} ms .. {}/{} ms",
        range.lower.validated_completions,
        range.lower.elapsed_millis,
        range.upper.validated_completions,
        range.upper.elapsed_millis,
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
    use crate::mac_availability::HostPowerSource;
    use crate::macos_resource_observation::{
        BatteryChargeState, LimaProcessObservation, LimaProcessRole, MacPowerObservation,
        ObservationCompleteness, SwapObservation,
    };

    fn digest(seed: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
    }

    fn performance_receipt(
        work_id: &str,
        final_millis: u64,
        result: HotExecutionResultClass,
    ) -> HotExecutionPerformanceReceipt {
        HotExecutionPerformanceReceipt::new(
            HotExecutionPerformanceIdentity::new(
                work_id,
                "project",
                "source",
                "candidate",
                "lima-vz",
                "apple-silicon",
                "profile",
            )
            .unwrap(),
            HotExecutionMode::ResidentTaskLoop,
            final_millis,
            HotExecutionMilestones::new(
                Some(10),
                Some(10),
                Some(10),
                Some(20),
                Some(final_millis - 10),
                Some(final_millis),
                None,
            )
            .unwrap(),
            HotExecutionHeat::new(
                HotSandboxState::ResidentHit,
                HotRepositoryState::ResidentHit,
                HotDependencyState::ResidentHit,
                HotBuildState::IncrementalHit,
                HotIndexServiceState::ResidentHit,
            ),
            None,
            None,
            result,
        )
        .unwrap()
    }

    fn attempt(
        request_id: &str,
        work_id: &str,
        final_millis: u64,
        result: HotExecutionResultClass,
        validation: HotFleetSemanticValidation,
        fallback_observed: bool,
    ) -> HotFleetAttemptEvidence {
        HotFleetAttemptEvidence::new(
            ExecutionRequestId::parse(request_id).unwrap(),
            Some(performance_receipt(work_id, final_millis, result)),
            validation,
            fallback_observed,
        )
        .unwrap()
    }

    fn members(validated: usize, latency_base: u64) -> Vec<HotFleetWorkItem> {
        (0..4)
            .map(|index| {
                let work_id = format!("work-{index}");
                let evidence = if index < validated {
                    attempt(
                        &format!("req-{index}"),
                        &work_id,
                        latency_base + index as u64 * 10,
                        HotExecutionResultClass::Succeeded,
                        HotFleetSemanticValidation::Accepted,
                        false,
                    )
                } else {
                    attempt(
                        &format!("req-{index}"),
                        &work_id,
                        latency_base,
                        HotExecutionResultClass::Failed,
                        HotFleetSemanticValidation::Unobserved,
                        false,
                    )
                };
                HotFleetWorkItem::new(
                    &work_id,
                    digest(char::from(b'a' + index as u8)),
                    Some(evidence),
                )
                .unwrap()
            })
            .collect()
    }

    fn identity(arm: HotFleetArm, block: &str, decision: &str) -> HotFleetWindowIdentity {
        HotFleetWindowIdentity::new(
            "exp-1",
            block,
            arm,
            HotFleetDecisionKind::HeavySlotPolicy,
            decision,
            "validator-v1",
            "same-source-class",
            "lima-vz",
            "apple-silicon",
            digest('f'),
            "blocked-arrivals",
            ComputeTrustClass::UltraTrusted,
            "non-treatment-v1",
        )
        .unwrap()
    }

    fn resource_report(
        at: u64,
        pressure: MemoryPressure,
        swap_used: u64,
        lima_rss: u64,
    ) -> MacOsResourceReport {
        MacOsResourceReport {
            schema_version: MACOS_RESOURCE_OBSERVATION_SCHEMA_VERSION,
            observed_at_millis: at,
            freshness: ObservationFreshness::Fresh,
            completeness: ObservationCompleteness::Complete,
            memory_pressure: pressure,
            swap: Some(SwapObservation {
                total_bytes: 16_u64 << 30,
                used_bytes: swap_used,
                free_bytes: (16_u64 << 30) - swap_used,
                encrypted: Some(true),
            }),
            power: MacPowerObservation {
                source: HostPowerSource::Ac,
                battery_percent: None,
                charge_state: BatteryChargeState::Unknown,
            },
            lima_processes: vec![LimaProcessObservation {
                pid: 100,
                parent_pid: 1,
                role: LimaProcessRole::VirtualMachine,
                cpu_basis_points: 100,
                rss_bytes: lima_rss,
                elapsed_seconds: 10,
            }],
            problems: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn window(
        arm: HotFleetArm,
        block: &str,
        decision: &str,
        validated: usize,
        latency_base: u64,
        pressure: MemoryPressure,
        swap_used: u64,
        lima_rss: u64,
    ) -> HotFleetWindowReceiptV1 {
        HotFleetWindowReceiptV1::new(
            identity(arm, block, decision),
            HotFleetWindowTiming::new(
                EpochMillis::new(1_000).unwrap(),
                EpochMillis::new(5_000).unwrap(),
                4_000,
                4_000,
                HotFleetWindowCloseReason::Deadline,
            )
            .unwrap(),
            HotFleetConcurrencyObservation::new(
                HotFleetConcurrencyBasis::HeavyExecution,
                4,
                HotFleetConcurrencyObservationSource::HarnessOwned,
            )
            .unwrap(),
            members(validated, latency_base),
            &[
                resource_report(1_000, pressure, 0, lima_rss),
                resource_report(5_000, pressure, swap_used, lima_rss),
            ],
        )
        .unwrap()
    }

    #[test]
    fn lower_latency_can_have_lower_validated_throughput_and_critical_pressure() {
        let a1 = window(
            HotFleetArm::Baseline,
            "a1",
            "slots-2",
            4,
            1_000,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );
        let b1 = window(
            HotFleetArm::Candidate,
            "b1",
            "slots-4",
            3,
            700,
            MemoryPressure::Critical,
            2_u64 << 30,
            8_u64 << 30,
        );
        let b2 = window(
            HotFleetArm::Candidate,
            "b2",
            "slots-4",
            3,
            720,
            MemoryPressure::Critical,
            3_u64 << 30,
            8_u64 << 30,
        );
        let a2 = window(
            HotFleetArm::Baseline,
            "a2",
            "slots-2",
            4,
            1_020,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );

        assert_eq!(a1.final_result_p50_millis(), Some(1_010));
        assert_eq!(a1.final_result_p90_millis(), Some(1_030));
        assert!(b1.final_result_p50_millis() < a1.final_result_p50_millis());

        let report = compare_hot_fleet_abba([&a1, &b1, &b2, &a2]).unwrap();
        assert_eq!(
            report.first_pair_direction,
            HotFleetRateDirection::BaselineHigher
        );
        assert_eq!(
            report.second_pair_direction,
            HotFleetRateDirection::BaselineHigher
        );
        assert!(report.directions_consistent);
        assert_eq!(
            report.rate_range_relationship,
            HotFleetRateRangeRelationship::BaselineAboveCandidate
        );
        assert_eq!(
            report.candidate_resources.max_observed_memory_pressure,
            MemoryPressure::Critical
        );
        assert_eq!(
            report.candidate_resources.max_observed_swap_bytes,
            Some(3_u64 << 30)
        );
    }

    #[test]
    fn modest_resident_rss_can_raise_validated_throughput_with_stable_pressure() {
        let a1 = window(
            HotFleetArm::Baseline,
            "a1",
            "coldish",
            3,
            1_000,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );
        let b1 = window(
            HotFleetArm::Candidate,
            "b1",
            "resident",
            4,
            950,
            MemoryPressure::Normal,
            0,
            6_u64 << 30,
        );
        let b2 = window(
            HotFleetArm::Candidate,
            "b2",
            "resident",
            4,
            960,
            MemoryPressure::Normal,
            0,
            6_u64 << 30,
        );
        let a2 = window(
            HotFleetArm::Baseline,
            "a2",
            "coldish",
            3,
            1_010,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );

        let report = compare_hot_fleet_abba([&a1, &b1, &b2, &a2]).unwrap();
        assert_eq!(
            report.first_pair_direction,
            HotFleetRateDirection::CandidateHigher
        );
        assert_eq!(
            report.rate_range_relationship,
            HotFleetRateRangeRelationship::CandidateAboveBaseline
        );
        assert_eq!(
            report
                .candidate_resources
                .max_observed_aggregate_lima_rss_bytes,
            Some(6_u64 << 30)
        );
        assert_eq!(
            report.candidate_resources.max_observed_memory_pressure,
            MemoryPressure::Normal
        );
    }

    #[test]
    fn successful_unvalidated_work_is_visible_and_has_zero_validated_rate() {
        let item = HotFleetWorkItem::new(
            "work-0",
            digest('a'),
            Some(attempt(
                "req-unvalidated",
                "work-0",
                500,
                HotExecutionResultClass::Succeeded,
                HotFleetSemanticValidation::Unobserved,
                true,
            )),
        )
        .unwrap();
        let receipt = HotFleetWindowReceiptV1::new(
            identity(HotFleetArm::Baseline, "a1", "a"),
            HotFleetWindowTiming::new(
                EpochMillis::new(1_000).unwrap(),
                EpochMillis::new(2_000).unwrap(),
                1_000,
                1_000,
                HotFleetWindowCloseReason::Deadline,
            )
            .unwrap(),
            HotFleetConcurrencyObservation::new(
                HotFleetConcurrencyBasis::ActiveTasks,
                1,
                HotFleetConcurrencyObservationSource::HarnessOwned,
            )
            .unwrap(),
            vec![item],
            &[],
        )
        .unwrap();

        assert_eq!(receipt.counts().validated_completions, 0);
        assert_eq!(receipt.counts().unvalidated_completions, 1);
        assert_eq!(receipt.counts().fallbacks, 1);
        assert_eq!(receipt.counts().unfinished, 0);
        assert_eq!(receipt.validated_rate().validated_completions, 0);
        assert_eq!(
            receipt.resources().status,
            HotFleetResourceObservationStatus::Partial
        );
    }

    #[test]
    fn duplicate_work_and_request_ids_fail_closed() {
        let timing = HotFleetWindowTiming::new(
            EpochMillis::new(1_000).unwrap(),
            EpochMillis::new(2_000).unwrap(),
            1_000,
            1_000,
            HotFleetWindowCloseReason::Deadline,
        )
        .unwrap();
        let concurrency = HotFleetConcurrencyObservation::new(
            HotFleetConcurrencyBasis::ActiveTasks,
            2,
            HotFleetConcurrencyObservationSource::HarnessOwned,
        )
        .unwrap();

        let duplicate_work = HotFleetWindowReceiptV1::new(
            identity(HotFleetArm::Baseline, "a1", "a"),
            timing,
            concurrency,
            vec![
                HotFleetWorkItem::new(
                    "same-work",
                    digest('a'),
                    Some(attempt(
                        "req-1",
                        "same-work",
                        500,
                        HotExecutionResultClass::Succeeded,
                        HotFleetSemanticValidation::Accepted,
                        false,
                    )),
                )
                .unwrap(),
                HotFleetWorkItem::new(
                    "same-work",
                    digest('b'),
                    Some(attempt(
                        "req-2",
                        "same-work",
                        500,
                        HotExecutionResultClass::Succeeded,
                        HotFleetSemanticValidation::Accepted,
                        false,
                    )),
                )
                .unwrap(),
            ],
            &[],
        )
        .unwrap_err();
        assert_eq!(duplicate_work.code, "duplicate_hot_fleet_work_id");

        let duplicate_request = HotFleetWindowReceiptV1::new(
            identity(HotFleetArm::Baseline, "a2", "a"),
            timing,
            concurrency,
            vec![
                HotFleetWorkItem::new(
                    "work-a",
                    digest('a'),
                    Some(attempt(
                        "shared-request",
                        "work-a",
                        500,
                        HotExecutionResultClass::Succeeded,
                        HotFleetSemanticValidation::Accepted,
                        false,
                    )),
                )
                .unwrap(),
                HotFleetWorkItem::new(
                    "work-b",
                    digest('b'),
                    Some(attempt(
                        "shared-request",
                        "work-b",
                        500,
                        HotExecutionResultClass::Succeeded,
                        HotFleetSemanticValidation::Accepted,
                        false,
                    )),
                )
                .unwrap(),
            ],
            &[],
        )
        .unwrap_err();
        assert_eq!(duplicate_request.code, "duplicate_hot_fleet_request_id");
    }

    #[test]
    fn workload_digest_is_order_independent_and_semantic_identity_sensitive() {
        let timing = HotFleetWindowTiming::new(
            EpochMillis::new(1_000).unwrap(),
            EpochMillis::new(2_000).unwrap(),
            1_000,
            1_000,
            HotFleetWindowCloseReason::Deadline,
        )
        .unwrap();
        let concurrency = HotFleetConcurrencyObservation::new(
            HotFleetConcurrencyBasis::ActiveTasks,
            0,
            HotFleetConcurrencyObservationSource::HarnessOwned,
        )
        .unwrap();
        let make = |block: &str, members: Vec<HotFleetWorkItem>| {
            HotFleetWindowReceiptV1::new(
                identity(HotFleetArm::Baseline, block, "a"),
                timing,
                concurrency,
                members,
                &[],
            )
            .unwrap()
        };
        let left = make(
            "a1",
            vec![
                HotFleetWorkItem::new("work-a", digest('a'), None).unwrap(),
                HotFleetWorkItem::new("work-b", digest('b'), None).unwrap(),
            ],
        );
        let reordered = make(
            "a2",
            vec![
                HotFleetWorkItem::new("work-b", digest('b'), None).unwrap(),
                HotFleetWorkItem::new("work-a", digest('a'), None).unwrap(),
            ],
        );
        let changed = make(
            "a3",
            vec![
                HotFleetWorkItem::new("work-a", digest('c'), None).unwrap(),
                HotFleetWorkItem::new("work-b", digest('b'), None).unwrap(),
            ],
        );

        assert_eq!(left.workload_set_digest(), reordered.workload_set_digest());
        assert_ne!(left.workload_set_digest(), changed.workload_set_digest());
    }

    #[test]
    fn unrelated_power_gap_does_not_poison_memory_resource_completeness() {
        let mut first = resource_report(1_000, MemoryPressure::Normal, 0, 4_u64 << 30);
        first.completeness = ObservationCompleteness::Partial;
        first.problems = vec![MacOsResourceProblemKind::PowerUnavailable];
        let receipt = HotFleetWindowReceiptV1::new(
            identity(HotFleetArm::Baseline, "a1", "a"),
            HotFleetWindowTiming::new(
                EpochMillis::new(1_000).unwrap(),
                EpochMillis::new(2_000).unwrap(),
                1_000,
                1_000,
                HotFleetWindowCloseReason::Deadline,
            )
            .unwrap(),
            HotFleetConcurrencyObservation::new(
                HotFleetConcurrencyBasis::ActiveTasks,
                0,
                HotFleetConcurrencyObservationSource::HarnessOwned,
            )
            .unwrap(),
            vec![HotFleetWorkItem::new("work-a", digest('a'), None).unwrap()],
            &[
                first,
                resource_report(2_000, MemoryPressure::Normal, 0, 4_u64 << 30),
            ],
        )
        .unwrap();

        assert_eq!(
            receipt.resources().status,
            HotFleetResourceObservationStatus::Complete
        );
    }

    #[test]
    fn abba_rejects_order_and_non_treatment_mismatch() {
        let a1 = window(
            HotFleetArm::Baseline,
            "a1",
            "a",
            4,
            1_000,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );
        let b1 = window(
            HotFleetArm::Candidate,
            "b1",
            "b",
            4,
            1_000,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );
        let b2 = window(
            HotFleetArm::Candidate,
            "b2",
            "b",
            4,
            1_000,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );
        let mut a2 = window(
            HotFleetArm::Baseline,
            "a2",
            "a",
            4,
            1_000,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );

        let reversed = compare_hot_fleet_abba([&b1, &a1, &a2, &b2]).unwrap_err();
        assert_eq!(reversed.reason, HotFleetComparisonRefusalReason::ArmOrder);

        a2.identity.non_treatment_policy_id = Token::parse("other-policy").unwrap();
        let mismatch = compare_hot_fleet_abba([&a1, &b1, &b2, &a2]).unwrap_err();
        assert_eq!(
            mismatch.reason,
            HotFleetComparisonRefusalReason::NonTreatmentPolicy
        );
    }

    #[test]
    fn human_and_json_render_from_the_same_typed_model() {
        let receipt = window(
            HotFleetArm::Baseline,
            "a1",
            "baseline",
            4,
            1_000,
            MemoryPressure::Normal,
            0,
            4_u64 << 30,
        );
        let human = receipt.render_human();
        let json = receipt.render_json().unwrap();
        assert!(human.contains("validated: 4"));
        assert!(human.contains("resource status: complete"));
        assert!(json.contains("\"validated_completions\": 4"));
        assert!(json.contains("\"authority\": \"observation_only\""));
    }
}
