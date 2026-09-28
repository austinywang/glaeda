//! Pure provider-neutral accelerator burst eligibility and ranking.
//!
//! This module is deliberately observation-only. It accepts already-sanitized requirement and
//! candidate facts, then returns an inspectable plan. It grants zero provisioning, billing,
//! credential, network, storage, remote-desktop, execution, lease, teardown, or purchase authority.

use std::fmt;

use serde::Serialize;

const GIB: u64 = 1024 * 1024 * 1024;
const MAX_ACCELERATOR_MEMORY_BYTES: u64 = 1024 * GIB;
const MAX_EXPECTED_MILLISECONDS: u64 = 7 * 24 * 60 * 60 * 1000;
const MAX_RTT_MILLISECONDS: u32 = 60_000;
const MAX_HOURLY_PRICE_MICRO_USD: u64 = 1_000_000_000;
const MAX_TARGETS: usize = 128;
const MAX_PUBLIC_ID_BYTES: usize = 96;

pub const ACCELERATOR_BURST_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorVendor {
    Nvidia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorApi {
    Cuda,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorIntent {
    Batch,
    Interactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorLeaseState {
    Cold,
    Warm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AcceleratorTargetId(String);

impl AcceleratorTargetId {
    pub fn parse(value: &str) -> Result<Self, AcceleratorBurstError> {
        validate_identifier("target_id", value)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AcceleratorRegionId(String);

impl AcceleratorRegionId {
    pub fn parse(value: &str) -> Result<Self, AcceleratorBurstError> {
        validate_identifier("region_id", value)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceleratorRequirement {
    schema_version: u8,
    vendor: AcceleratorVendor,
    api: AcceleratorApi,
    minimum_memory_bytes: u64,
    intent: AcceleratorIntent,
    maximum_rtt_ms: Option<u32>,
}

impl AcceleratorRequirement {
    pub fn new(
        vendor: AcceleratorVendor,
        api: AcceleratorApi,
        minimum_memory_bytes: u64,
        intent: AcceleratorIntent,
        maximum_rtt_ms: Option<u32>,
    ) -> Result<Self, AcceleratorBurstError> {
        if !(GIB..=MAX_ACCELERATOR_MEMORY_BYTES).contains(&minimum_memory_bytes) {
            return Err(AcceleratorBurstError::new(
                "minimum_memory_bytes",
                "invalid_accelerator_memory_requirement",
                "accelerator memory requirement must be between 1 GiB and 1 TiB",
            ));
        }
        match (intent, maximum_rtt_ms) {
            (AcceleratorIntent::Interactive, Some(value))
                if (1..=MAX_RTT_MILLISECONDS).contains(&value) => {}
            (AcceleratorIntent::Interactive, _) => {
                return Err(AcceleratorBurstError::new(
                    "maximum_rtt_ms",
                    "interactive_rtt_required",
                    "interactive accelerator requests require a bounded positive RTT ceiling",
                ));
            }
            (AcceleratorIntent::Batch, None) => {}
            (AcceleratorIntent::Batch, Some(_)) => {
                return Err(AcceleratorBurstError::new(
                    "maximum_rtt_ms",
                    "batch_rtt_must_be_absent",
                    "batch accelerator requests must not carry an interactive RTT ceiling",
                ));
            }
        }
        Ok(Self {
            schema_version: ACCELERATOR_BURST_SCHEMA_VERSION,
            vendor,
            api,
            minimum_memory_bytes,
            intent,
            maximum_rtt_ms,
        })
    }

    #[must_use]
    pub const fn vendor(&self) -> AcceleratorVendor {
        self.vendor
    }

    #[must_use]
    pub const fn api(&self) -> AcceleratorApi {
        self.api
    }

    #[must_use]
    pub const fn minimum_memory_bytes(&self) -> u64 {
        self.minimum_memory_bytes
    }

    #[must_use]
    pub const fn intent(&self) -> AcceleratorIntent {
        self.intent
    }

    #[must_use]
    pub const fn maximum_rtt_ms(&self) -> Option<u32> {
        self.maximum_rtt_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceleratorCandidateObservation {
    schema_version: u8,
    target_id: AcceleratorTargetId,
    region_id: AcceleratorRegionId,
    vendor: AcceleratorVendor,
    api: AcceleratorApi,
    memory_bytes: u64,
    availability: AcceleratorAvailability,
    lease_state: AcceleratorLeaseState,
    observed_rtt_ms: Option<u32>,
    startup_to_useful_ms: Option<u64>,
    predicted_work_ms: Option<u64>,
    hourly_price_micro_usd: u64,
}

impl AcceleratorCandidateObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target_id: AcceleratorTargetId,
        region_id: AcceleratorRegionId,
        vendor: AcceleratorVendor,
        api: AcceleratorApi,
        memory_bytes: u64,
        availability: AcceleratorAvailability,
        lease_state: AcceleratorLeaseState,
        observed_rtt_ms: Option<u32>,
        startup_to_useful_ms: Option<u64>,
        predicted_work_ms: Option<u64>,
        hourly_price_micro_usd: u64,
    ) -> Result<Self, AcceleratorBurstError> {
        if !(GIB..=MAX_ACCELERATOR_MEMORY_BYTES).contains(&memory_bytes) {
            return Err(AcceleratorBurstError::new(
                "memory_bytes",
                "invalid_accelerator_memory_observation",
                "observed accelerator memory must be between 1 GiB and 1 TiB",
            ));
        }
        if observed_rtt_ms.is_some_and(|value| value > MAX_RTT_MILLISECONDS) {
            return Err(AcceleratorBurstError::new(
                "observed_rtt_ms",
                "invalid_accelerator_rtt_observation",
                "observed accelerator RTT exceeds the bounded maximum",
            ));
        }
        if startup_to_useful_ms.is_some_and(|value| value > MAX_EXPECTED_MILLISECONDS) {
            return Err(AcceleratorBurstError::new(
                "startup_to_useful_ms",
                "invalid_accelerator_startup_observation",
                "startup-to-useful estimate exceeds the bounded maximum",
            ));
        }
        if predicted_work_ms.is_some_and(|value| value == 0 || value > MAX_EXPECTED_MILLISECONDS) {
            return Err(AcceleratorBurstError::new(
                "predicted_work_ms",
                "invalid_accelerator_work_prediction",
                "predicted useful-work duration must be positive and within the bounded maximum",
            ));
        }
        if hourly_price_micro_usd > MAX_HOURLY_PRICE_MICRO_USD {
            return Err(AcceleratorBurstError::new(
                "hourly_price_micro_usd",
                "invalid_accelerator_hourly_price",
                "hourly accelerator price exceeds the bounded maximum",
            ));
        }
        Ok(Self {
            schema_version: ACCELERATOR_BURST_SCHEMA_VERSION,
            target_id,
            region_id,
            vendor,
            api,
            memory_bytes,
            availability,
            lease_state,
            observed_rtt_ms,
            startup_to_useful_ms,
            predicted_work_ms,
            hourly_price_micro_usd,
        })
    }

    #[must_use]
    pub fn target_id(&self) -> &AcceleratorTargetId {
        &self.target_id
    }

    #[must_use]
    pub const fn lease_state(&self) -> AcceleratorLeaseState {
        self.lease_state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorRefusalReason {
    Unavailable,
    VendorMismatch,
    ApiMismatch,
    InsufficientMemory,
    MissingInteractiveRtt,
    InteractiveRttTooHigh,
    MissingStartupPrediction,
    MissingWorkPrediction,
    PredictionOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum AcceleratorCandidatePlan {
    Eligible {
        target: AcceleratorCandidateObservation,
        predicted_completion_ms: u64,
        estimated_cost_micro_usd: u64,
    },
    Refused {
        target: AcceleratorCandidateObservation,
        reasons: Vec<AcceleratorRefusalReason>,
    },
}

impl AcceleratorCandidatePlan {
    #[must_use]
    pub fn target(&self) -> &AcceleratorCandidateObservation {
        match self {
            Self::Eligible { target, .. } | Self::Refused { target, .. } => target,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceleratorBurstPlan {
    schema_version: u8,
    request: AcceleratorRequirement,
    candidates: Vec<AcceleratorCandidatePlan>,
}

impl AcceleratorBurstPlan {
    pub fn build(
        request: AcceleratorRequirement,
        candidates: Vec<AcceleratorCandidateObservation>,
    ) -> Result<Self, AcceleratorBurstError> {
        if candidates.len() > MAX_TARGETS {
            return Err(AcceleratorBurstError::new(
                "candidates",
                "too_many_accelerator_candidates",
                "accelerator plan exceeds the bounded candidate maximum",
            ));
        }
        let mut planned = candidates
            .into_iter()
            .map(|target| plan_candidate(&request, target))
            .collect::<Vec<_>>();
        planned.sort_by(compare_candidate_plans);
        Ok(Self {
            schema_version: ACCELERATOR_BURST_SCHEMA_VERSION,
            request,
            candidates: planned,
        })
    }

    #[must_use]
    pub fn candidates(&self) -> &[AcceleratorCandidatePlan] {
        &self.candidates
    }

    #[must_use]
    pub fn selected(&self) -> Option<&AcceleratorCandidatePlan> {
        self.candidates
            .iter()
            .find(|candidate| matches!(candidate, AcceleratorCandidatePlan::Eligible { .. }))
    }
}

fn plan_candidate(
    request: &AcceleratorRequirement,
    target: AcceleratorCandidateObservation,
) -> AcceleratorCandidatePlan {
    let mut reasons = Vec::new();
    if target.availability != AcceleratorAvailability::Available {
        reasons.push(AcceleratorRefusalReason::Unavailable);
    }
    if target.vendor != request.vendor() {
        reasons.push(AcceleratorRefusalReason::VendorMismatch);
    }
    if target.api != request.api() {
        reasons.push(AcceleratorRefusalReason::ApiMismatch);
    }
    if target.memory_bytes < request.minimum_memory_bytes() {
        reasons.push(AcceleratorRefusalReason::InsufficientMemory);
    }
    if request.intent() == AcceleratorIntent::Interactive {
        match (target.observed_rtt_ms, request.maximum_rtt_ms()) {
            (None, _) => reasons.push(AcceleratorRefusalReason::MissingInteractiveRtt),
            (Some(observed), Some(maximum)) if observed > maximum => {
                reasons.push(AcceleratorRefusalReason::InteractiveRttTooHigh);
            }
            _ => {}
        }
    }
    if target.startup_to_useful_ms.is_none() {
        reasons.push(AcceleratorRefusalReason::MissingStartupPrediction);
    }
    if target.predicted_work_ms.is_none() {
        reasons.push(AcceleratorRefusalReason::MissingWorkPrediction);
    }
    if !reasons.is_empty() {
        return AcceleratorCandidatePlan::Refused { target, reasons };
    }

    let startup_ms = target
        .startup_to_useful_ms
        .expect("validated candidate has startup prediction");
    let work_ms = target
        .predicted_work_ms
        .expect("validated candidate has work prediction");
    let Some(predicted_completion_ms) = startup_ms.checked_add(work_ms) else {
        return AcceleratorCandidatePlan::Refused {
            target,
            reasons: vec![AcceleratorRefusalReason::PredictionOverflow],
        };
    };
    let estimated_cost_micro_usd =
        estimated_cost_micro_usd(predicted_completion_ms, target.hourly_price_micro_usd);
    AcceleratorCandidatePlan::Eligible {
        target,
        predicted_completion_ms,
        estimated_cost_micro_usd,
    }
}

fn estimated_cost_micro_usd(milliseconds: u64, hourly_price_micro_usd: u64) -> u64 {
    const HOUR_MILLISECONDS: u128 = 3_600_000;
    let numerator = u128::from(milliseconds) * u128::from(hourly_price_micro_usd);
    let rounded = numerator.div_ceil(HOUR_MILLISECONDS);
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

fn compare_candidate_plans(
    left: &AcceleratorCandidatePlan,
    right: &AcceleratorCandidatePlan,
) -> std::cmp::Ordering {
    use AcceleratorCandidatePlan::{Eligible, Refused};
    match (left, right) {
        (
            Eligible {
                target: left_target,
                predicted_completion_ms: left_completion,
                estimated_cost_micro_usd: left_cost,
            },
            Eligible {
                target: right_target,
                predicted_completion_ms: right_completion,
                estimated_cost_micro_usd: right_cost,
            },
        ) => left_target
            .lease_state()
            .cmp(&right_target.lease_state())
            .reverse()
            .then_with(|| left_completion.cmp(right_completion))
            .then_with(|| left_cost.cmp(right_cost))
            .then_with(|| left_target.target_id().cmp(right_target.target_id())),
        (Eligible { .. }, Refused { .. }) => std::cmp::Ordering::Less,
        (Refused { .. }, Eligible { .. }) => std::cmp::Ordering::Greater,
        (Refused { target: left, .. }, Refused { target: right, .. }) => {
            left.target_id().cmp(right.target_id())
        }
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), AcceleratorBurstError> {
    let bytes = value.as_bytes();
    let valid_edges = bytes
        .first()
        .zip(bytes.last())
        .is_some_and(|(first, last)| first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric());
    let valid_body = bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if value.is_empty() || value.len() > MAX_PUBLIC_ID_BYTES || !valid_edges || !valid_body {
        return Err(AcceleratorBurstError::new(
            field,
            "invalid_accelerator_identifier",
            "accelerator identifier must be bounded ASCII using letters, digits, '.', '_', or '-' with alphanumeric edges",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AcceleratorBurstError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl AcceleratorBurstError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for AcceleratorBurstError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for AcceleratorBurstError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> AcceleratorTargetId {
        AcceleratorTargetId::parse(value).unwrap()
    }

    fn region(value: &str) -> AcceleratorRegionId {
        AcceleratorRegionId::parse(value).unwrap()
    }

    fn interactive_requirement() -> AcceleratorRequirement {
        AcceleratorRequirement::new(
            AcceleratorVendor::Nvidia,
            AcceleratorApi::Cuda,
            24 * GIB,
            AcceleratorIntent::Interactive,
            Some(45),
        )
        .unwrap()
    }

    fn candidate(
        name: &str,
        memory_gib: u64,
        lease_state: AcceleratorLeaseState,
        rtt_ms: Option<u32>,
        startup_ms: Option<u64>,
        work_ms: Option<u64>,
        hourly_price_micro_usd: u64,
    ) -> AcceleratorCandidateObservation {
        AcceleratorCandidateObservation::new(
            id(name),
            region("us-west"),
            AcceleratorVendor::Nvidia,
            AcceleratorApi::Cuda,
            memory_gib * GIB,
            AcceleratorAvailability::Available,
            lease_state,
            rtt_ms,
            startup_ms,
            work_ms,
            hourly_price_micro_usd,
        )
        .unwrap()
    }

    #[test]
    fn interactive_requests_require_an_explicit_rtt_ceiling() {
        let error = AcceleratorRequirement::new(
            AcceleratorVendor::Nvidia,
            AcceleratorApi::Cuda,
            24 * GIB,
            AcceleratorIntent::Interactive,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code(), "interactive_rtt_required");
    }

    #[test]
    fn batch_requests_do_not_smuggle_an_interactive_latency_policy() {
        let error = AcceleratorRequirement::new(
            AcceleratorVendor::Nvidia,
            AcceleratorApi::Cuda,
            24 * GIB,
            AcceleratorIntent::Batch,
            Some(100),
        )
        .unwrap_err();
        assert_eq!(error.code(), "batch_rtt_must_be_absent");
    }

    #[test]
    fn warm_eligible_lease_beats_a_faster_cold_candidate() {
        let plan = AcceleratorBurstPlan::build(
            interactive_requirement(),
            vec![
                candidate(
                    "cold-5090",
                    32,
                    AcceleratorLeaseState::Cold,
                    Some(20),
                    Some(2_000),
                    Some(5_000),
                    740_000,
                ),
                candidate(
                    "warm-5090",
                    32,
                    AcceleratorLeaseState::Warm,
                    Some(20),
                    Some(500),
                    Some(8_000),
                    800_000,
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            plan.selected().unwrap().target().target_id().as_str(),
            "warm-5090"
        );
    }

    #[test]
    fn cold_candidates_rank_by_time_to_result_then_cost() {
        let plan = AcceleratorBurstPlan::build(
            interactive_requirement(),
            vec![
                candidate(
                    "slower-cheap",
                    32,
                    AcceleratorLeaseState::Cold,
                    Some(20),
                    Some(4_000),
                    Some(8_000),
                    500_000,
                ),
                candidate(
                    "faster-expensive",
                    32,
                    AcceleratorLeaseState::Cold,
                    Some(20),
                    Some(1_000),
                    Some(8_000),
                    900_000,
                ),
                candidate(
                    "faster-cheaper",
                    32,
                    AcceleratorLeaseState::Cold,
                    Some(20),
                    Some(1_000),
                    Some(8_000),
                    700_000,
                ),
            ],
        )
        .unwrap();
        let ordered = plan
            .candidates()
            .iter()
            .map(|candidate| candidate.target().target_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec!["faster-cheaper", "faster-expensive", "slower-cheap"]
        );
    }

    #[test]
    fn interactive_candidates_fail_closed_on_missing_or_excessive_rtt() {
        let plan = AcceleratorBurstPlan::build(
            interactive_requirement(),
            vec![
                candidate(
                    "missing-rtt",
                    32,
                    AcceleratorLeaseState::Cold,
                    None,
                    Some(1_000),
                    Some(1_000),
                    500_000,
                ),
                candidate(
                    "slow-rtt",
                    32,
                    AcceleratorLeaseState::Cold,
                    Some(80),
                    Some(1_000),
                    Some(1_000),
                    500_000,
                ),
            ],
        )
        .unwrap();
        assert!(matches!(
            &plan.candidates()[0],
            AcceleratorCandidatePlan::Refused { reasons, .. }
                if reasons.contains(&AcceleratorRefusalReason::MissingInteractiveRtt)
        ));
        assert!(matches!(
            &plan.candidates()[1],
            AcceleratorCandidatePlan::Refused { reasons, .. }
                if reasons.contains(&AcceleratorRefusalReason::InteractiveRttTooHigh)
        ));
    }

    #[test]
    fn insufficient_vram_and_missing_predictions_are_explicit_refusals() {
        let plan = AcceleratorBurstPlan::build(
            interactive_requirement(),
            vec![candidate(
                "small",
                16,
                AcceleratorLeaseState::Cold,
                Some(20),
                None,
                None,
                500_000,
            )],
        )
        .unwrap();
        let AcceleratorCandidatePlan::Refused { reasons, .. } = &plan.candidates()[0] else {
            panic!("candidate must refuse");
        };
        assert!(reasons.contains(&AcceleratorRefusalReason::InsufficientMemory));
        assert!(reasons.contains(&AcceleratorRefusalReason::MissingStartupPrediction));
        assert!(reasons.contains(&AcceleratorRefusalReason::MissingWorkPrediction));
    }

    #[test]
    fn estimated_cost_is_rounded_up_and_bounded_integer_math() {
        assert_eq!(estimated_cost_micro_usd(3_600_000, 740_000), 740_000);
        assert_eq!(estimated_cost_micro_usd(1, 740_000), 1);
    }

    #[test]
    fn public_json_contains_only_bounded_plan_facts() {
        let plan = AcceleratorBurstPlan::build(
            interactive_requirement(),
            vec![candidate(
                "warm-5090",
                32,
                AcceleratorLeaseState::Warm,
                Some(18),
                Some(250),
                Some(4_000),
                740_000,
            )],
        )
        .unwrap();
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("warm-5090"));
        assert!(json.contains("predicted_completion_ms"));
        for forbidden in ["token", "credential", "/home/", "/Users/", "ssh_command"] {
            assert!(!json.contains(forbidden));
        }
    }
}
