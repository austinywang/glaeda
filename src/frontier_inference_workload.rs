//! Pure model-derived frontier-inference workload vocabulary and synthetic sensitivity fixtures.
//!
//! Recorded tokens are an accounting quantity. This module deterministically separates them into
//! generated decode, fresh prefill, and cache-hit input before deriving burst-window rates. It also
//! keeps active context/state and host-tool phases explicit so later contention experiments can
//! model accelerator work and ordinary host work independently.
//!
//! The receipt is model evidence only. It grants zero serving, placement, admission, scheduling,
//! capacity-claim, residency, cache-validity, lifecycle, mutation, provider, purchase, or result
//! authority. Provider/model identities, current prices, physical benchmark claims, and serving
//! implementation details deliberately live outside this first workload slice.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::compute_workload::{
    ComputeCapabilityId, ComputeCapabilitySet, ComputeInputIdentity, ComputeOutputContractIdentity,
    ComputeSemanticGeneration, ComputeTrustClass, ComputeWorkloadFamilyId, ComputeWorkloadIdentity,
};
use crate::execution_capacity::{CapacityAmounts, CapacityDimension};

pub const FRONTIER_INFERENCE_WORKLOAD_SCHEMA_VERSION: u8 = 1;
pub const FRONTIER_INFERENCE_WORKLOAD_FAMILY_ID: &str = "frontier_inference.v1";
pub const FRONTIER_INFERENCE_COMPUTE_SEMANTIC_GENERATION: u64 = 1;
pub const FRONTIER_INFERENCE_BASIS_POINTS: u16 = 10_000;
pub const FRONTIER_INFERENCE_SECONDS_PER_WEEK: u64 = 7 * 24 * 60 * 60;
pub const MAX_FRONTIER_INFERENCE_RECORDED_TOKENS: u64 = 1_000_000_000_000_000;
pub const MAX_FRONTIER_INFERENCE_CONTEXT_TOKENS: u64 = 100_000_000;
pub const MAX_FRONTIER_INFERENCE_STATE_BYTES: u64 = 1 << 50;
pub const MAX_FRONTIER_INFERENCE_HOST_TOOL_PHASES: usize = 32;
pub const MAX_FRONTIER_INFERENCE_HOST_TOOL_PHASE_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;

const MILLI_TOKENS_PER_TOKEN: u64 = 1_000;
const REFERENCE_INPUT_CACHE_HIT_BASIS_POINTS: u16 = 9_500;
const EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS: u64 = 7 * 8 * 60 * 60;
const INPUT_IDENTITY_DOCUMENT_TYPE: &str = "frontier_inference_input_v1";
const SYNTHETIC_OUTPUT_CONTRACT_DOCUMENT_TYPE: &str =
    "frontier_inference_synthetic_output_contract_v1";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierInferenceReceiptAuthority {
    ModelOnly,
}

impl FrontierInferenceReceiptAuthority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelOnly => "model_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierInferenceDerivationClass {
    ModelDerived,
    SyntheticFixture,
}

impl FrontierInferenceDerivationClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelDerived => "model_derived",
            Self::SyntheticFixture => "synthetic_fixture",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceTokenModel {
    recorded_tokens: u64,
    generated_share_basis_points: u16,
    input_cache_hit_share_basis_points: u16,
}

impl FrontierInferenceTokenModel {
    pub fn new(
        recorded_tokens: u64,
        generated_share_basis_points: u16,
        input_cache_hit_share_basis_points: u16,
    ) -> Result<Self, FrontierInferenceWorkloadError> {
        if !(1..=MAX_FRONTIER_INFERENCE_RECORDED_TOKENS).contains(&recorded_tokens) {
            return Err(FrontierInferenceWorkloadError::new(
                "recorded_tokens",
                "invalid_frontier_inference_recorded_tokens",
                "recorded token volume must be within the bounded positive range",
            ));
        }
        validate_share("generated_share_basis_points", generated_share_basis_points)?;
        validate_share(
            "input_cache_hit_share_basis_points",
            input_cache_hit_share_basis_points,
        )?;

        Ok(Self {
            recorded_tokens,
            generated_share_basis_points,
            input_cache_hit_share_basis_points,
        })
    }

    fn derive(self) -> Result<FrontierInferenceTokenBreakdown, FrontierInferenceWorkloadError> {
        let decode_tokens = scale_by_basis_points_floor(
            self.recorded_tokens,
            self.generated_share_basis_points,
            "generated_share_basis_points",
        )?;
        let input_tokens = self.recorded_tokens - decode_tokens;
        let cache_hit_input_tokens = scale_by_basis_points_floor(
            input_tokens,
            self.input_cache_hit_share_basis_points,
            "input_cache_hit_share_basis_points",
        )?;
        let fresh_prefill_tokens = input_tokens - cache_hit_input_tokens;

        Ok(FrontierInferenceTokenBreakdown {
            recorded_tokens: self.recorded_tokens,
            generated_share_basis_points: self.generated_share_basis_points,
            input_cache_hit_share_basis_points: self.input_cache_hit_share_basis_points,
            input_tokens,
            fresh_prefill_tokens,
            cache_hit_input_tokens,
            decode_tokens,
        })
    }
}

/// Deterministic split of one blended recorded-token quantity.
///
/// Generated and cache-hit shares use floor rounding. Fresh prefill receives the integer remainder,
/// preserving the exact invariant `recorded = fresh_prefill + cache_hit_input + decode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceTokenBreakdown {
    recorded_tokens: u64,
    generated_share_basis_points: u16,
    input_cache_hit_share_basis_points: u16,
    input_tokens: u64,
    fresh_prefill_tokens: u64,
    cache_hit_input_tokens: u64,
    decode_tokens: u64,
}

impl FrontierInferenceTokenBreakdown {
    #[must_use]
    pub const fn recorded_tokens(&self) -> u64 {
        self.recorded_tokens
    }

    #[must_use]
    pub const fn generated_share_basis_points(&self) -> u16 {
        self.generated_share_basis_points
    }

    #[must_use]
    pub const fn input_cache_hit_share_basis_points(&self) -> u16 {
        self.input_cache_hit_share_basis_points
    }

    #[must_use]
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    #[must_use]
    pub const fn fresh_prefill_tokens(&self) -> u64 {
        self.fresh_prefill_tokens
    }

    #[must_use]
    pub const fn cache_hit_input_tokens(&self) -> u64 {
        self.cache_hit_input_tokens
    }

    #[must_use]
    pub const fn decode_tokens(&self) -> u64 {
        self.decode_tokens
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceWindow {
    window_seconds: u64,
    active_inference_seconds: u64,
}

impl FrontierInferenceWindow {
    pub fn new(
        window_seconds: u64,
        active_inference_seconds: u64,
    ) -> Result<Self, FrontierInferenceWorkloadError> {
        if !(1..=FRONTIER_INFERENCE_SECONDS_PER_WEEK).contains(&window_seconds) {
            return Err(FrontierInferenceWorkloadError::new(
                "window_seconds",
                "invalid_frontier_inference_window",
                "workload window must be within one bounded week",
            ));
        }
        if active_inference_seconds == 0 || active_inference_seconds > window_seconds {
            return Err(FrontierInferenceWorkloadError::new(
                "active_inference_seconds",
                "invalid_frontier_inference_active_window",
                "active inference seconds must be positive and no greater than the workload window",
            ));
        }
        Ok(Self {
            window_seconds,
            active_inference_seconds,
        })
    }

    #[must_use]
    pub const fn window_seconds(&self) -> u64 {
        self.window_seconds
    }

    #[must_use]
    pub const fn active_inference_seconds(&self) -> u64 {
        self.active_inference_seconds
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceContextState {
    active_context_tokens: u64,
    active_state_bytes: u64,
}

impl FrontierInferenceContextState {
    pub fn new(
        active_context_tokens: u64,
        active_state_bytes: u64,
    ) -> Result<Self, FrontierInferenceWorkloadError> {
        if active_context_tokens > MAX_FRONTIER_INFERENCE_CONTEXT_TOKENS {
            return Err(FrontierInferenceWorkloadError::new(
                "active_context_tokens",
                "invalid_frontier_inference_context_tokens",
                "active context tokens exceed the bounded model envelope",
            ));
        }
        if active_state_bytes > MAX_FRONTIER_INFERENCE_STATE_BYTES {
            return Err(FrontierInferenceWorkloadError::new(
                "active_state_bytes",
                "invalid_frontier_inference_state_bytes",
                "active state bytes exceed the bounded model envelope",
            ));
        }
        Ok(Self {
            active_context_tokens,
            active_state_bytes,
        })
    }

    #[must_use]
    pub const fn active_context_tokens(&self) -> u64 {
        self.active_context_tokens
    }

    #[must_use]
    pub const fn active_state_bytes(&self) -> u64 {
        self.active_state_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierHostToolPhaseKind {
    RepositoryIo,
    BrowserComputer,
    BuildTest,
    IndexDataTransform,
    NetworkToolIo,
}

impl FrontierHostToolPhaseKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepositoryIo => "repository_io",
            Self::BrowserComputer => "browser_computer",
            Self::BuildTest => "build_test",
            Self::IndexDataTransform => "index_data_transform",
            Self::NetworkToolIo => "network_tool_io",
        }
    }
}

/// One ordinary host-compute phase adjacent to model bursts.
///
/// `host_capacity_demand` reuses the generic CPU/RAM/disk/PID amount vocabulary. It is demand
/// metadata for a model fixture, never a `CapacityClaim` and never ownership or admission evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontierHostToolPhase {
    kind: FrontierHostToolPhaseKind,
    elapsed_millis: u64,
    host_capacity_demand: CapacityAmounts,
}

impl FrontierHostToolPhase {
    pub fn new(
        kind: FrontierHostToolPhaseKind,
        elapsed_millis: u64,
        host_capacity_demand: CapacityAmounts,
    ) -> Result<Self, FrontierInferenceWorkloadError> {
        if !(1..=MAX_FRONTIER_INFERENCE_HOST_TOOL_PHASE_MILLIS).contains(&elapsed_millis) {
            return Err(FrontierInferenceWorkloadError::new(
                "host_tool_phases.elapsed_millis",
                "invalid_frontier_inference_host_phase_duration",
                "host-tool phase duration must be within the bounded positive range",
            ));
        }
        Ok(Self {
            kind,
            elapsed_millis,
            host_capacity_demand,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> FrontierHostToolPhaseKind {
        self.kind
    }

    #[must_use]
    pub const fn elapsed_millis(&self) -> u64 {
        self.elapsed_millis
    }

    #[must_use]
    pub const fn host_capacity_demand(&self) -> &CapacityAmounts {
        &self.host_capacity_demand
    }
}

#[derive(Serialize)]
struct FrontierInferenceInputIdentityDocument<'a> {
    document_type: &'static str,
    schema_version: u8,
    token_model: FrontierInferenceTokenModel,
    window: FrontierInferenceWindow,
    context_state: FrontierInferenceContextState,
    host_tool_phases: &'a [FrontierHostToolPhase],
}

/// Derive the exact generic compute input identity for this family-owned workload model.
///
/// Token volume/shares, burst window, active context/state, and ordered host-tool phases all bind
/// the identity. Trust, required capabilities, and output acceptance remain separate fields in the
/// generic compute identity.
#[must_use]
pub fn frontier_inference_compute_input_identity(
    token_model: FrontierInferenceTokenModel,
    window: FrontierInferenceWindow,
    context_state: FrontierInferenceContextState,
    host_tool_phases: &[FrontierHostToolPhase],
) -> ComputeInputIdentity {
    let document = FrontierInferenceInputIdentityDocument {
        document_type: INPUT_IDENTITY_DOCUMENT_TYPE,
        schema_version: FRONTIER_INFERENCE_WORKLOAD_SCHEMA_VERSION,
        token_model,
        window,
        context_state,
        host_tool_phases,
    };
    let bytes = serde_json::to_vec(&document)
        .expect("typed frontier inference input identity document must serialize");
    ComputeInputIdentity::new(sha256_digest(&bytes))
}

/// Build the generic compute identity using this family's exact semantic-input derivation.
#[must_use]
pub fn frontier_inference_compute_workload_identity(
    trust_class: ComputeTrustClass,
    required_capabilities: ComputeCapabilitySet,
    output_contract: ComputeOutputContractIdentity,
    token_model: FrontierInferenceTokenModel,
    window: FrontierInferenceWindow,
    context_state: FrontierInferenceContextState,
    host_tool_phases: &[FrontierHostToolPhase],
) -> ComputeWorkloadIdentity {
    ComputeWorkloadIdentity::new(
        ComputeWorkloadFamilyId::parse(FRONTIER_INFERENCE_WORKLOAD_FAMILY_ID)
            .expect("fixed frontier workload family must validate"),
        ComputeSemanticGeneration::new(FRONTIER_INFERENCE_COMPUTE_SEMANTIC_GENERATION)
            .expect("fixed frontier semantic generation must validate"),
        frontier_inference_compute_input_identity(
            token_model,
            window,
            context_state,
            host_tool_phases,
        ),
        trust_class,
        required_capabilities,
        output_contract,
    )
}

/// Active-window rates at one-millisecond-token precision.
///
/// Separate fields deliberately prevent one generic `tokens_per_second` value from collapsing
/// recorded accounting volume, prefill, cache-hit input, and aggregate decode into one quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrontierInferencePhaseRates {
    recorded_milli_tokens_per_second: u64,
    fresh_prefill_milli_tokens_per_second: u64,
    cache_hit_input_milli_tokens_per_second: u64,
    aggregate_decode_milli_tokens_per_second: u64,
}

impl FrontierInferencePhaseRates {
    fn derive(
        tokens: FrontierInferenceTokenBreakdown,
        window: FrontierInferenceWindow,
    ) -> Result<Self, FrontierInferenceWorkloadError> {
        Ok(Self {
            recorded_milli_tokens_per_second: milli_token_rate(
                tokens.recorded_tokens,
                window.active_inference_seconds,
            )?,
            fresh_prefill_milli_tokens_per_second: milli_token_rate(
                tokens.fresh_prefill_tokens,
                window.active_inference_seconds,
            )?,
            cache_hit_input_milli_tokens_per_second: milli_token_rate(
                tokens.cache_hit_input_tokens,
                window.active_inference_seconds,
            )?,
            aggregate_decode_milli_tokens_per_second: milli_token_rate(
                tokens.decode_tokens,
                window.active_inference_seconds,
            )?,
        })
    }

    #[must_use]
    pub const fn recorded_milli_tokens_per_second(&self) -> u64 {
        self.recorded_milli_tokens_per_second
    }

    #[must_use]
    pub const fn fresh_prefill_milli_tokens_per_second(&self) -> u64 {
        self.fresh_prefill_milli_tokens_per_second
    }

    #[must_use]
    pub const fn cache_hit_input_milli_tokens_per_second(&self) -> u64 {
        self.cache_hit_input_milli_tokens_per_second
    }

    #[must_use]
    pub const fn aggregate_decode_milli_tokens_per_second(&self) -> u64 {
        self.aggregate_decode_milli_tokens_per_second
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceWorkloadReceiptV1 {
    schema_version: u8,
    authority: FrontierInferenceReceiptAuthority,
    derivation: FrontierInferenceDerivationClass,
    workload: ComputeWorkloadIdentity,
    tokens: FrontierInferenceTokenBreakdown,
    window: FrontierInferenceWindow,
    rates: FrontierInferencePhaseRates,
    context_state: FrontierInferenceContextState,
    host_tool_phases: Vec<FrontierHostToolPhase>,
}

impl FrontierInferenceWorkloadReceiptV1 {
    pub fn new(
        workload: ComputeWorkloadIdentity,
        derivation: FrontierInferenceDerivationClass,
        token_model: FrontierInferenceTokenModel,
        window: FrontierInferenceWindow,
        context_state: FrontierInferenceContextState,
        host_tool_phases: Vec<FrontierHostToolPhase>,
    ) -> Result<Self, FrontierInferenceWorkloadError> {
        if workload.family().as_str() != FRONTIER_INFERENCE_WORKLOAD_FAMILY_ID {
            return Err(FrontierInferenceWorkloadError::new(
                "workload.family",
                "frontier_inference_workload_family_mismatch",
                "frontier inference receipt requires the exact frontier-inference compute family",
            ));
        }
        if workload.semantic_generation().get() != FRONTIER_INFERENCE_COMPUTE_SEMANTIC_GENERATION {
            return Err(FrontierInferenceWorkloadError::new(
                "workload.semantic_generation",
                "frontier_inference_semantic_generation_mismatch",
                "frontier inference receipt requires the exact family semantic generation",
            ));
        }
        if host_tool_phases.len() > MAX_FRONTIER_INFERENCE_HOST_TOOL_PHASES {
            return Err(FrontierInferenceWorkloadError::new(
                "host_tool_phases",
                "too_many_frontier_inference_host_tool_phases",
                "frontier inference workload exceeds the bounded host-tool phase count",
            ));
        }
        let expected_input_identity = frontier_inference_compute_input_identity(
            token_model,
            window,
            context_state,
            &host_tool_phases,
        );
        if workload.input_identity() != &expected_input_identity {
            return Err(FrontierInferenceWorkloadError::new(
                "workload.input_identity",
                "frontier_inference_input_identity_mismatch",
                "frontier inference compute input identity does not bind the receipt inputs",
            ));
        }

        let tokens = token_model.derive()?;
        let rates = FrontierInferencePhaseRates::derive(tokens, window)?;
        Ok(Self {
            schema_version: FRONTIER_INFERENCE_WORKLOAD_SCHEMA_VERSION,
            authority: FrontierInferenceReceiptAuthority::ModelOnly,
            derivation,
            workload,
            tokens,
            window,
            rates,
            context_state,
            host_tool_phases,
        })
    }

    #[must_use]
    pub const fn authority(&self) -> FrontierInferenceReceiptAuthority {
        self.authority
    }

    #[must_use]
    pub const fn derivation(&self) -> FrontierInferenceDerivationClass {
        self.derivation
    }

    #[must_use]
    pub const fn workload(&self) -> &ComputeWorkloadIdentity {
        &self.workload
    }

    #[must_use]
    pub const fn tokens(&self) -> &FrontierInferenceTokenBreakdown {
        &self.tokens
    }

    #[must_use]
    pub const fn window(&self) -> FrontierInferenceWindow {
        self.window
    }

    #[must_use]
    pub const fn rates(&self) -> FrontierInferencePhaseRates {
        self.rates
    }

    #[must_use]
    pub const fn context_state(&self) -> FrontierInferenceContextState {
        self.context_state
    }

    #[must_use]
    pub fn host_tool_phases(&self) -> &[FrontierHostToolPhase] {
        &self.host_tool_phases
    }

    #[must_use]
    pub fn render_human(&self) -> String {
        format!(
            "frontier inference {}: recorded={} fresh_prefill={} cache_hit_input={} decode={} active={}/{}s context={}tok state={}B host_phases={}",
            self.derivation.as_str(),
            self.tokens.recorded_tokens,
            self.tokens.fresh_prefill_tokens,
            self.tokens.cache_hit_input_tokens,
            self.tokens.decode_tokens,
            self.window.active_inference_seconds,
            self.window.window_seconds,
            self.context_state.active_context_tokens,
            self.context_state.active_state_bytes,
            self.host_tool_phases.len(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierInferenceSensitivityCase {
    Reference10B2PctFullWeek,
    Reference10B5PctFullWeek,
    Reference12B2PctFullWeek,
    Reference12B5PctFullWeek,
    Reference10B2PctEightHourDays,
    Reference10B5PctEightHourDays,
    Reference12B2PctEightHourDays,
    Reference12B5PctEightHourDays,
    Forward20B5PctEightHourDays,
    Forward20B10PctEightHourDays,
    Forward30B5PctEightHourDays,
    Forward30B10PctEightHourDays,
    Forward30B20PctEightHourDays,
    Forward50B5PctEightHourDays,
    Forward50B10PctEightHourDays,
    Forward50B20PctEightHourDays,
}

impl FrontierInferenceSensitivityCase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reference10B2PctFullWeek => "reference_10b_2pct_full_week",
            Self::Reference10B5PctFullWeek => "reference_10b_5pct_full_week",
            Self::Reference12B2PctFullWeek => "reference_12b_2pct_full_week",
            Self::Reference12B5PctFullWeek => "reference_12b_5pct_full_week",
            Self::Reference10B2PctEightHourDays => "reference_10b_2pct_eight_hour_days",
            Self::Reference10B5PctEightHourDays => "reference_10b_5pct_eight_hour_days",
            Self::Reference12B2PctEightHourDays => "reference_12b_2pct_eight_hour_days",
            Self::Reference12B5PctEightHourDays => "reference_12b_5pct_eight_hour_days",
            Self::Forward20B5PctEightHourDays => "forward_20b_5pct_eight_hour_days",
            Self::Forward20B10PctEightHourDays => "forward_20b_10pct_eight_hour_days",
            Self::Forward30B5PctEightHourDays => "forward_30b_5pct_eight_hour_days",
            Self::Forward30B10PctEightHourDays => "forward_30b_10pct_eight_hour_days",
            Self::Forward30B20PctEightHourDays => "forward_30b_20pct_eight_hour_days",
            Self::Forward50B5PctEightHourDays => "forward_50b_5pct_eight_hour_days",
            Self::Forward50B10PctEightHourDays => "forward_50b_10pct_eight_hour_days",
            Self::Forward50B20PctEightHourDays => "forward_50b_20pct_eight_hour_days",
        }
    }

    const fn parameters(self) -> (u64, u16, u64) {
        match self {
            Self::Reference10B2PctFullWeek => {
                (10_000_000_000, 200, FRONTIER_INFERENCE_SECONDS_PER_WEEK)
            }
            Self::Reference10B5PctFullWeek => {
                (10_000_000_000, 500, FRONTIER_INFERENCE_SECONDS_PER_WEEK)
            }
            Self::Reference12B2PctFullWeek => {
                (12_000_000_000, 200, FRONTIER_INFERENCE_SECONDS_PER_WEEK)
            }
            Self::Reference12B5PctFullWeek => {
                (12_000_000_000, 500, FRONTIER_INFERENCE_SECONDS_PER_WEEK)
            }
            Self::Reference10B2PctEightHourDays => {
                (10_000_000_000, 200, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Reference10B5PctEightHourDays => {
                (10_000_000_000, 500, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Reference12B2PctEightHourDays => {
                (12_000_000_000, 200, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Reference12B5PctEightHourDays => {
                (12_000_000_000, 500, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward20B5PctEightHourDays => {
                (20_000_000_000, 500, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward20B10PctEightHourDays => {
                (20_000_000_000, 1_000, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward30B5PctEightHourDays => {
                (30_000_000_000, 500, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward30B10PctEightHourDays => {
                (30_000_000_000, 1_000, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward30B20PctEightHourDays => {
                (30_000_000_000, 2_000, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward50B5PctEightHourDays => {
                (50_000_000_000, 500, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward50B10PctEightHourDays => {
                (50_000_000_000, 1_000, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
            Self::Forward50B20PctEightHourDays => {
                (50_000_000_000, 2_000, EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS)
            }
        }
    }
}

const FRONTIER_INFERENCE_SENSITIVITY_CASES: [FrontierInferenceSensitivityCase; 16] = [
    FrontierInferenceSensitivityCase::Reference10B2PctFullWeek,
    FrontierInferenceSensitivityCase::Reference10B5PctFullWeek,
    FrontierInferenceSensitivityCase::Reference12B2PctFullWeek,
    FrontierInferenceSensitivityCase::Reference12B5PctFullWeek,
    FrontierInferenceSensitivityCase::Reference10B2PctEightHourDays,
    FrontierInferenceSensitivityCase::Reference10B5PctEightHourDays,
    FrontierInferenceSensitivityCase::Reference12B2PctEightHourDays,
    FrontierInferenceSensitivityCase::Reference12B5PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward20B5PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward20B10PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward30B5PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward30B10PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward30B20PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward50B5PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward50B10PctEightHourDays,
    FrontierInferenceSensitivityCase::Forward50B20PctEightHourDays,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceSyntheticFixture {
    case: FrontierInferenceSensitivityCase,
    receipt: FrontierInferenceWorkloadReceiptV1,
}

impl FrontierInferenceSyntheticFixture {
    #[must_use]
    pub const fn case(&self) -> FrontierInferenceSensitivityCase {
        self.case
    }

    #[must_use]
    pub const fn receipt(&self) -> &FrontierInferenceWorkloadReceiptV1 {
        &self.receipt
    }
}

/// Reusable model-only fixtures for the #777 10/12B reference and 20/30/50B sensitivity bands.
///
/// Every case uses 95% cache-hit input. Full-week and eight-active-hours-per-day variants keep
/// burst concentration explicit. Context/state and host demands are intentionally synthetic fixed
/// values whose only purpose is to exercise independent resource dimensions in later experiments.
#[must_use]
pub fn frontier_inference_synthetic_sensitivity_fixtures() -> Vec<FrontierInferenceSyntheticFixture>
{
    let context_state = synthetic_context_state();
    let host_tool_phases = synthetic_host_tool_phases();

    FRONTIER_INFERENCE_SENSITIVITY_CASES
        .into_iter()
        .map(|case| {
            let (recorded_tokens, generated_share_basis_points, active_inference_seconds) =
                case.parameters();
            let token_model = FrontierInferenceTokenModel::new(
                recorded_tokens,
                generated_share_basis_points,
                REFERENCE_INPUT_CACHE_HIT_BASIS_POINTS,
            )
            .expect("fixed frontier sensitivity token model must validate");
            let window = FrontierInferenceWindow::new(
                FRONTIER_INFERENCE_SECONDS_PER_WEEK,
                active_inference_seconds,
            )
            .expect("fixed frontier sensitivity window must validate");
            let capabilities = ComputeCapabilitySet::new(vec![
                ComputeCapabilityId::parse("accelerator.frontier-inference")
                    .expect("fixed accelerator capability must validate"),
                ComputeCapabilityId::parse("cpu.general")
                    .expect("fixed CPU capability must validate"),
                ComputeCapabilityId::parse("storage.local")
                    .expect("fixed storage capability must validate"),
            ])
            .expect("fixed synthetic capabilities must validate");
            let output_contract = ComputeOutputContractIdentity::new(sha256_digest(
                SYNTHETIC_OUTPUT_CONTRACT_DOCUMENT_TYPE.as_bytes(),
            ));
            let workload = frontier_inference_compute_workload_identity(
                ComputeTrustClass::Trusted,
                capabilities,
                output_contract,
                token_model,
                window,
                context_state,
                &host_tool_phases,
            );
            let receipt = FrontierInferenceWorkloadReceiptV1::new(
                workload,
                FrontierInferenceDerivationClass::SyntheticFixture,
                token_model,
                window,
                context_state,
                host_tool_phases.clone(),
            )
            .expect("fixed frontier sensitivity receipt must validate");
            FrontierInferenceSyntheticFixture { case, receipt }
        })
        .collect()
}

fn synthetic_context_state() -> FrontierInferenceContextState {
    FrontierInferenceContextState::new(262_144, 16 * 1024 * 1024 * 1024)
        .expect("fixed synthetic context/state must validate")
}

fn synthetic_host_tool_phases() -> Vec<FrontierHostToolPhase> {
    [
        (
            FrontierHostToolPhaseKind::RepositoryIo,
            2_500,
            [1_000, 512 * 1024 * 1024, 4 * 1024 * 1024 * 1024, 16],
        ),
        (
            FrontierHostToolPhaseKind::BuildTest,
            20_000,
            [4_000, 4 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024, 128],
        ),
        (
            FrontierHostToolPhaseKind::BrowserComputer,
            8_000,
            [2_000, 2 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024, 64],
        ),
        (
            FrontierHostToolPhaseKind::IndexDataTransform,
            12_000,
            [3_000, 3 * 1024 * 1024 * 1024, 4 * 1024 * 1024 * 1024, 64],
        ),
    ]
    .into_iter()
    .map(|(kind, elapsed_millis, amounts)| {
        let host_capacity_demand = CapacityAmounts::new(&[
            (CapacityDimension::CpuMillis, amounts[0]),
            (CapacityDimension::MemoryBytes, amounts[1]),
            (CapacityDimension::DiskBytes, amounts[2]),
            (CapacityDimension::Pids, amounts[3]),
        ])
        .expect("fixed synthetic host capacity demand must validate");
        FrontierHostToolPhase::new(kind, elapsed_millis, host_capacity_demand)
            .expect("fixed synthetic host phase must validate")
    })
    .collect()
}

fn sha256_digest(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Sha256Digest::parse(&value).expect("SHA-256 encoder must produce a canonical digest")
}

fn validate_share(field: &'static str, value: u16) -> Result<(), FrontierInferenceWorkloadError> {
    if value > FRONTIER_INFERENCE_BASIS_POINTS {
        return Err(FrontierInferenceWorkloadError::new(
            field,
            "invalid_frontier_inference_share",
            "frontier inference share must be between zero and 10,000 basis points",
        ));
    }
    Ok(())
}

fn scale_by_basis_points_floor(
    value: u64,
    share_basis_points: u16,
    field: &'static str,
) -> Result<u64, FrontierInferenceWorkloadError> {
    value
        .checked_mul(u64::from(share_basis_points))
        .map(|scaled| scaled / u64::from(FRONTIER_INFERENCE_BASIS_POINTS))
        .ok_or_else(|| {
            FrontierInferenceWorkloadError::new(
                field,
                "frontier_inference_arithmetic_overflow",
                "frontier inference token-share arithmetic overflowed",
            )
        })
}

fn milli_token_rate(
    tokens: u64,
    active_seconds: u64,
) -> Result<u64, FrontierInferenceWorkloadError> {
    tokens
        .checked_mul(MILLI_TOKENS_PER_TOKEN)
        .map(|scaled| scaled / active_seconds)
        .ok_or_else(|| {
            FrontierInferenceWorkloadError::new(
                "rates",
                "frontier_inference_arithmetic_overflow",
                "frontier inference rate arithmetic overflowed",
            )
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceWorkloadError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl FrontierInferenceWorkloadError {
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

impl fmt::Display for FrontierInferenceWorkloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for FrontierInferenceWorkloadError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(case: FrontierInferenceSensitivityCase) -> FrontierInferenceSyntheticFixture {
        frontier_inference_synthetic_sensitivity_fixtures()
            .into_iter()
            .find(|fixture| fixture.case() == case)
            .expect("fixture case must exist")
    }

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn recorded_tokens_split_into_prefill_cache_and_decode_exactly() {
        let fixture = fixture(FrontierInferenceSensitivityCase::Reference10B2PctFullWeek);
        let tokens = fixture.receipt().tokens();

        assert_eq!(tokens.recorded_tokens(), 10_000_000_000);
        assert_eq!(tokens.decode_tokens(), 200_000_000);
        assert_eq!(tokens.input_tokens(), 9_800_000_000);
        assert_eq!(tokens.cache_hit_input_tokens(), 9_310_000_000);
        assert_eq!(tokens.fresh_prefill_tokens(), 490_000_000);
        assert_eq!(
            tokens.fresh_prefill_tokens()
                + tokens.cache_hit_input_tokens()
                + tokens.decode_tokens(),
            tokens.recorded_tokens()
        );
        assert_eq!(tokens.generated_share_basis_points(), 200);
        assert_eq!(tokens.input_cache_hit_share_basis_points(), 9_500);
    }

    #[test]
    fn active_window_rates_match_reference_and_forward_sensitivity() {
        let reference = fixture(FrontierInferenceSensitivityCase::Reference10B2PctFullWeek);
        assert_eq!(
            reference
                .receipt()
                .rates()
                .recorded_milli_tokens_per_second(),
            16_534_391
        );
        assert_eq!(
            reference
                .receipt()
                .rates()
                .aggregate_decode_milli_tokens_per_second(),
            330_687
        );
        assert_eq!(
            reference
                .receipt()
                .rates()
                .fresh_prefill_milli_tokens_per_second(),
            810_185
        );

        let forward = fixture(FrontierInferenceSensitivityCase::Forward20B5PctEightHourDays);
        assert_eq!(
            forward
                .receipt()
                .rates()
                .aggregate_decode_milli_tokens_per_second(),
            4_960_317
        );
        assert_eq!(
            forward
                .receipt()
                .rates()
                .fresh_prefill_milli_tokens_per_second(),
            4_712_301
        );
        assert_eq!(
            forward.receipt().window().active_inference_seconds(),
            EIGHT_ACTIVE_HOURS_PER_DAY_SECONDS
        );
    }

    #[test]
    fn generated_share_sensitivity_moves_decode_and_fresh_prefill_independently() {
        let five = fixture(FrontierInferenceSensitivityCase::Forward30B5PctEightHourDays);
        let ten = fixture(FrontierInferenceSensitivityCase::Forward30B10PctEightHourDays);
        let twenty = fixture(FrontierInferenceSensitivityCase::Forward30B20PctEightHourDays);

        assert!(five.receipt().tokens().decode_tokens() < ten.receipt().tokens().decode_tokens());
        assert!(ten.receipt().tokens().decode_tokens() < twenty.receipt().tokens().decode_tokens());
        assert!(
            five.receipt().tokens().fresh_prefill_tokens()
                > ten.receipt().tokens().fresh_prefill_tokens()
        );
        assert!(
            ten.receipt().tokens().fresh_prefill_tokens()
                > twenty.receipt().tokens().fresh_prefill_tokens()
        );
        assert_ne!(
            five.receipt().workload().input_identity(),
            ten.receipt().workload().input_identity()
        );
    }

    #[test]
    fn synthetic_fixture_keeps_context_state_and_host_phases_separate() {
        let fixture = fixture(FrontierInferenceSensitivityCase::Forward50B20PctEightHourDays);
        let receipt = fixture.receipt();

        assert_eq!(receipt.context_state().active_context_tokens(), 262_144);
        assert_eq!(
            receipt.context_state().active_state_bytes(),
            16 * 1024 * 1024 * 1024
        );
        assert_eq!(receipt.host_tool_phases().len(), 4);
        assert_eq!(
            receipt.host_tool_phases()[0].kind(),
            FrontierHostToolPhaseKind::RepositoryIo
        );
        assert_eq!(
            receipt.host_tool_phases()[1]
                .host_capacity_demand()
                .amount(CapacityDimension::CpuMillis),
            Some(4_000)
        );
        assert_eq!(
            receipt.rates().aggregate_decode_milli_tokens_per_second(),
            49_603_174
        );
        assert_eq!(
            receipt.rates().fresh_prefill_milli_tokens_per_second(),
            9_920_634
        );
    }

    #[test]
    fn serialized_receipt_has_no_provider_price_or_single_speed_surface() {
        let fixture = fixture(FrontierInferenceSensitivityCase::Reference12B5PctEightHourDays);
        let json = serde_json::to_string(fixture.receipt()).unwrap();

        for field in [
            "recorded_tokens",
            "fresh_prefill_tokens",
            "cache_hit_input_tokens",
            "decode_tokens",
            "active_context_tokens",
            "active_state_bytes",
            "host_tool_phases",
            "aggregate_decode_milli_tokens_per_second",
        ] {
            assert!(json.contains(field));
        }
        for forbidden in [
            "product_surface",
            "provider",
            "model_name",
            "price",
            "per_stream",
            "\"tokens_per_second\":",
        ] {
            assert!(!json.contains(forbidden));
        }
        assert!(fixture.receipt().render_human().contains("fresh_prefill="));
        assert_eq!(fixture.receipt().authority().as_str(), "model_only");
        assert_eq!(fixture.receipt().derivation().as_str(), "synthetic_fixture");
        assert_eq!(
            fixture.receipt().workload().family().as_str(),
            FRONTIER_INFERENCE_WORKLOAD_FAMILY_ID
        );
        assert_eq!(
            fixture.receipt().workload().semantic_generation().get(),
            FRONTIER_INFERENCE_COMPUTE_SEMANTIC_GENERATION
        );
    }

    #[test]
    fn invalid_bounds_and_family_fail_closed() {
        assert_eq!(
            FrontierInferenceTokenModel::new(0, 200, 9_500)
                .unwrap_err()
                .code(),
            "invalid_frontier_inference_recorded_tokens"
        );
        assert_eq!(
            FrontierInferenceTokenModel::new(1_000, 10_001, 9_500)
                .unwrap_err()
                .code(),
            "invalid_frontier_inference_share"
        );
        assert_eq!(
            FrontierInferenceWindow::new(60, 61).unwrap_err().code(),
            "invalid_frontier_inference_active_window"
        );
        assert_eq!(
            FrontierInferenceContextState::new(MAX_FRONTIER_INFERENCE_CONTEXT_TOKENS + 1, 0)
                .unwrap_err()
                .code(),
            "invalid_frontier_inference_context_tokens"
        );

        let phase_capacity = CapacityAmounts::new(&[(CapacityDimension::CpuMillis, 1)]).unwrap();
        assert_eq!(
            FrontierHostToolPhase::new(FrontierHostToolPhaseKind::BuildTest, 0, phase_capacity)
                .unwrap_err()
                .code(),
            "invalid_frontier_inference_host_phase_duration"
        );

        let wrong_workload = ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse("dataset_transform.v1").unwrap(),
            ComputeSemanticGeneration::new(1).unwrap(),
            ComputeInputIdentity::new(digest('a')),
            ComputeTrustClass::Trusted,
            ComputeCapabilitySet::empty(),
            ComputeOutputContractIdentity::new(digest('b')),
        );
        let error = FrontierInferenceWorkloadReceiptV1::new(
            wrong_workload,
            FrontierInferenceDerivationClass::ModelDerived,
            FrontierInferenceTokenModel::new(1_000, 100, 9_500).unwrap(),
            FrontierInferenceWindow::new(60, 60).unwrap(),
            FrontierInferenceContextState::new(1, 0).unwrap(),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "frontier_inference_workload_family_mismatch");
    }

    #[test]
    fn receipt_requires_exact_family_generation_and_input_identity() {
        let token_model = FrontierInferenceTokenModel::new(1_000_000, 500, 9_500).unwrap();
        let window = FrontierInferenceWindow::new(3_600, 1_800).unwrap();
        let context_state = FrontierInferenceContextState::new(16_384, 1_048_576).unwrap();
        let output_contract = ComputeOutputContractIdentity::new(digest('c'));

        let wrong_generation = ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse(FRONTIER_INFERENCE_WORKLOAD_FAMILY_ID).unwrap(),
            ComputeSemanticGeneration::new(2).unwrap(),
            frontier_inference_compute_input_identity(token_model, window, context_state, &[]),
            ComputeTrustClass::Trusted,
            ComputeCapabilitySet::empty(),
            output_contract.clone(),
        );
        let error = FrontierInferenceWorkloadReceiptV1::new(
            wrong_generation,
            FrontierInferenceDerivationClass::ModelDerived,
            token_model,
            window,
            context_state,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.code(),
            "frontier_inference_semantic_generation_mismatch"
        );

        let wrong_input = ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse(FRONTIER_INFERENCE_WORKLOAD_FAMILY_ID).unwrap(),
            ComputeSemanticGeneration::new(FRONTIER_INFERENCE_COMPUTE_SEMANTIC_GENERATION).unwrap(),
            ComputeInputIdentity::new(digest('d')),
            ComputeTrustClass::Trusted,
            ComputeCapabilitySet::empty(),
            output_contract,
        );
        let error = FrontierInferenceWorkloadReceiptV1::new(
            wrong_input,
            FrontierInferenceDerivationClass::ModelDerived,
            token_model,
            window,
            context_state,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "frontier_inference_input_identity_mismatch");
    }

    #[test]
    fn fixture_set_covers_reference_and_forward_cases() {
        let fixtures = frontier_inference_synthetic_sensitivity_fixtures();
        assert_eq!(fixtures.len(), FRONTIER_INFERENCE_SENSITIVITY_CASES.len());
        assert_eq!(
            fixtures.first().unwrap().case().as_str(),
            "reference_10b_2pct_full_week"
        );
        assert_eq!(
            fixtures.last().unwrap().case().as_str(),
            "forward_50b_20pct_eight_hour_days"
        );
    }
}
