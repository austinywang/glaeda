//! Pure, path-free classification of explicitly supplied hot-state observations.
//!
//! This module does not discover files, infer ownership, or mutate host state. It accepts one
//! bounded observation document and fails closed unless every reclaim-safety condition is explicit.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const CACHE_INVENTORY_SCHEMA_VERSION: u8 = 1;
pub const CACHE_INVENTORY_REPORT_SCHEMA_VERSION: u8 = 1;
pub const MAX_CACHE_INVENTORY_DOCUMENT_BYTES: usize = 1_048_576;
pub const MAX_CACHE_INVENTORY_STATES: usize = 1_024;
const MAX_CACHE_STATE_ID_BYTES: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CacheStateId(String);

impl CacheStateId {
    /// Parse one bounded, path-free public state identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the identity is empty, oversized, or contains non-token bytes.
    pub fn parse(value: &str) -> Result<Self, CacheInventoryError> {
        let valid_length = !value.is_empty() && value.len() <= MAX_CACHE_STATE_ID_BYTES;
        let mut bytes = value.bytes();
        let valid_first = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric());
        let valid_rest =
            bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !valid_length || !valid_first || !valid_rest {
            return Err(error(
                CacheInventoryErrorKind::InvalidState,
                "cache state identity is invalid",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OwnershipObservation {
    ExactGlaedaOwned,
    Unmanaged,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GenerationObservation {
    Current,
    Retired,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorktreeObservation {
    Present,
    Detached,
    Removed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReconstructionObservation {
    Proven,
    Unproven,
    Unavailable,
    Unknown,
}

// Wrapping Option makes the field itself mandatory while admitting an explicit JSON null for
// unknown evidence. A missing field is therefore malformed rather than silently interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
struct RequiredObservedBool(Option<bool>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
struct RequiredObservedCount(Option<u64>);

#[derive(Debug, Deserialize)]
struct CacheInventoryVersionWire {
    schema_version: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheInventoryWire {
    schema_version: u8,
    states: Vec<CacheStateWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheStateWire {
    state_id: String,
    ownership: OwnershipObservation,
    generation: GenerationObservation,
    worktree: WorktreeObservation,
    reconstruction: ReconstructionObservation,
    logical_bytes: u64,
    allocated_bytes: u64,
    active_lease: RequiredObservedBool,
    active_lock: RequiredObservedBool,
    mounted: RequiredObservedBool,
    open_file_count: RequiredObservedCount,
    live_owned_process_count: RequiredObservedCount,
    interrupted_cleanup: RequiredObservedBool,
    quarantined: RequiredObservedBool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheInventoryDocument {
    states: Vec<CacheStateObservation>,
}

#[cfg(target_os = "linux")]
impl CacheInventoryDocument {
    pub(crate) fn from_observed_hot_run_states(
        states: Vec<(CacheStateId, u64, u64, Option<bool>)>,
    ) -> Result<Self, CacheInventoryError> {
        if states.len() > MAX_CACHE_INVENTORY_STATES {
            return Err(error(
                CacheInventoryErrorKind::TooManyStates,
                "cache inventory exceeds the reviewed state bound",
            ));
        }
        let mut identities = BTreeSet::new();
        let mut observations = Vec::with_capacity(states.len());
        for (state_id, logical_bytes, allocated_bytes, active_lock) in states {
            if !identities.insert(state_id.clone()) {
                return Err(error(
                    CacheInventoryErrorKind::DuplicateState,
                    "cache inventory contains a duplicate state identity",
                ));
            }
            observations.push(CacheStateObservation {
                state_id,
                ownership: OwnershipObservation::Unknown,
                generation: GenerationObservation::Unknown,
                worktree: WorktreeObservation::Unknown,
                reconstruction: ReconstructionObservation::Unknown,
                logical_bytes,
                allocated_bytes,
                active_lease: None,
                active_lock,
                mounted: None,
                open_file_count: None,
                live_owned_process_count: None,
                interrupted_cleanup: None,
                quarantined: None,
            });
        }
        observations.sort_by(|left, right| left.state_id.cmp(&right.state_id));
        Ok(Self {
            states: observations,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheStateObservation {
    state_id: CacheStateId,
    ownership: OwnershipObservation,
    generation: GenerationObservation,
    worktree: WorktreeObservation,
    reconstruction: ReconstructionObservation,
    logical_bytes: u64,
    allocated_bytes: u64,
    active_lease: Option<bool>,
    active_lock: Option<bool>,
    mounted: Option<bool>,
    open_file_count: Option<u64>,
    live_owned_process_count: Option<u64>,
    interrupted_cleanup: Option<bool>,
    quarantined: Option<bool>,
}

/// Decode and validate one strict, bounded, path-free cache observation document.
///
/// # Errors
///
/// Returns an error for oversized, malformed, future-version, duplicate, or invalid state input.
pub fn decode_cache_inventory(bytes: &[u8]) -> Result<CacheInventoryDocument, CacheInventoryError> {
    if bytes.len() > MAX_CACHE_INVENTORY_DOCUMENT_BYTES {
        return Err(error(
            CacheInventoryErrorKind::DocumentTooLarge,
            "cache inventory exceeds the reviewed byte limit",
        ));
    }
    let version: CacheInventoryVersionWire = serde_json::from_slice(bytes).map_err(|_| {
        error(
            CacheInventoryErrorKind::InvalidDocument,
            "cache inventory JSON is invalid",
        )
    })?;
    if version.schema_version != CACHE_INVENTORY_SCHEMA_VERSION {
        return Err(error(
            CacheInventoryErrorKind::VersionIncompatible,
            "cache inventory schema version is unsupported",
        ));
    }
    let wire: CacheInventoryWire = serde_json::from_slice(bytes).map_err(|_| {
        error(
            CacheInventoryErrorKind::InvalidDocument,
            "cache inventory JSON is invalid",
        )
    })?;
    if wire.schema_version != CACHE_INVENTORY_SCHEMA_VERSION {
        return Err(error(
            CacheInventoryErrorKind::VersionIncompatible,
            "cache inventory schema version is unsupported",
        ));
    }
    if wire.states.len() > MAX_CACHE_INVENTORY_STATES {
        return Err(error(
            CacheInventoryErrorKind::TooManyStates,
            "cache inventory exceeds the reviewed state bound",
        ));
    }

    let mut identities = BTreeSet::new();
    let mut states = Vec::with_capacity(wire.states.len());
    for state in wire.states {
        let state_id = CacheStateId::parse(&state.state_id)?;
        if !identities.insert(state_id.clone()) {
            return Err(error(
                CacheInventoryErrorKind::DuplicateState,
                "cache inventory contains a duplicate state identity",
            ));
        }
        states.push(CacheStateObservation {
            state_id,
            ownership: state.ownership,
            generation: state.generation,
            worktree: state.worktree,
            reconstruction: state.reconstruction,
            logical_bytes: state.logical_bytes,
            allocated_bytes: state.allocated_bytes,
            active_lease: state.active_lease.0,
            active_lock: state.active_lock.0,
            mounted: state.mounted.0,
            open_file_count: state.open_file_count.0,
            live_owned_process_count: state.live_owned_process_count.0,
            interrupted_cleanup: state.interrupted_cleanup.0,
            quarantined: state.quarantined.0,
        });
    }
    states.sort_by(|left, right| left.state_id.cmp(&right.state_id));
    Ok(CacheInventoryDocument { states })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheReportRequest {
    Status,
    Explain(CacheStateId),
    ReclaimDryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheReportOperation {
    Status,
    Explain,
    ReclaimDryRun,
}

impl CacheReportOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Explain => "explain",
            Self::ReclaimDryRun => "reclaim_dry_run",
        }
    }
}

/// Authority carried by one cache classification report.
///
/// The current CLI classifies a caller-supplied observation document. This value explicitly
/// prevents that classification from being promoted into family ownership or cleanup authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheInventoryAuthority {
    SuppliedObservationOnly,
    LocalHotRunFilesystemObservation,
}

impl CacheInventoryAuthority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SuppliedObservationOnly => "supplied_observation_only",
            Self::LocalHotRunFilesystemObservation => "local_hot_run_filesystem_observation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStateClassification {
    InUse,
    Warm,
    Reclaimable,
    Quarantined,
    Unknown,
}

impl CacheStateClassification {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InUse => "in_use",
            Self::Warm => "warm",
            Self::Reclaimable => "reclaimable",
            Self::Quarantined => "quarantined",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStateReason {
    Unmanaged,
    OwnershipUnknown,
    ActiveLease,
    ActiveLeaseUnknown,
    ActiveLock,
    ActiveLockUnknown,
    Mounted,
    MountStatusUnknown,
    OpenFiles,
    OpenFilesUnknown,
    LiveOwnedProcesses,
    LiveOwnedProcessesUnknown,
    CurrentGeneration,
    GenerationUnknown,
    WorktreePresent,
    WorktreeUnknown,
    InterruptedCleanup,
    InterruptedCleanupUnknown,
    ColdReconstructionUnproven,
    ColdReconstructionUnavailable,
    ColdReconstructionUnknown,
    Quarantined,
    QuarantineStatusUnknown,
}

impl CacheStateReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unmanaged => "unmanaged",
            Self::OwnershipUnknown => "ownership_unknown",
            Self::ActiveLease => "active_lease",
            Self::ActiveLeaseUnknown => "active_lease_unknown",
            Self::ActiveLock => "active_lock",
            Self::ActiveLockUnknown => "active_lock_unknown",
            Self::Mounted => "mounted",
            Self::MountStatusUnknown => "mount_status_unknown",
            Self::OpenFiles => "open_files",
            Self::OpenFilesUnknown => "open_files_unknown",
            Self::LiveOwnedProcesses => "live_owned_processes",
            Self::LiveOwnedProcessesUnknown => "live_owned_processes_unknown",
            Self::CurrentGeneration => "current_generation",
            Self::GenerationUnknown => "generation_unknown",
            Self::WorktreePresent => "worktree_present",
            Self::WorktreeUnknown => "worktree_unknown",
            Self::InterruptedCleanup => "interrupted_cleanup",
            Self::InterruptedCleanupUnknown => "interrupted_cleanup_unknown",
            Self::ColdReconstructionUnproven => "cold_reconstruction_unproven",
            Self::ColdReconstructionUnavailable => "cold_reconstruction_unavailable",
            Self::ColdReconstructionUnknown => "cold_reconstruction_unknown",
            Self::Quarantined => "quarantined",
            Self::QuarantineStatusUnknown => "quarantine_status_unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheStateReport {
    state_id: CacheStateId,
    classification: CacheStateClassification,
    reasons: Vec<CacheStateReason>,
    logical_bytes: u64,
    allocated_bytes: u64,
}

impl CacheStateReport {
    #[must_use]
    pub const fn state_id(&self) -> &CacheStateId {
        &self.state_id
    }

    #[must_use]
    pub const fn classification(&self) -> CacheStateClassification {
        self.classification
    }

    #[must_use]
    pub fn reasons(&self) -> &[CacheStateReason] {
        &self.reasons
    }

    #[must_use]
    pub const fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheInventorySummary {
    state_count: u32,
    in_use_count: u32,
    warm_count: u32,
    reclaimable_count: u32,
    quarantined_count: u32,
    unknown_count: u32,
    logical_bytes: u64,
    allocated_bytes: u64,
    reclaimable_allocated_bytes: u64,
}

impl CacheInventorySummary {
    #[must_use]
    pub const fn state_count(&self) -> u32 {
        self.state_count
    }

    #[must_use]
    pub const fn in_use_count(&self) -> u32 {
        self.in_use_count
    }

    #[must_use]
    pub const fn warm_count(&self) -> u32 {
        self.warm_count
    }

    #[must_use]
    pub const fn reclaimable_count(&self) -> u32 {
        self.reclaimable_count
    }

    #[must_use]
    pub const fn quarantined_count(&self) -> u32 {
        self.quarantined_count
    }

    #[must_use]
    pub const fn unknown_count(&self) -> u32 {
        self.unknown_count
    }

    #[must_use]
    pub const fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    #[must_use]
    pub const fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes
    }

    #[must_use]
    pub const fn reclaimable_allocated_bytes(&self) -> u64 {
        self.reclaimable_allocated_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheInventoryReport {
    schema_version: u8,
    authority: CacheInventoryAuthority,
    operation: CacheReportOperation,
    mutation_performed: bool,
    summary: CacheInventorySummary,
    states: Vec<CacheStateReport>,
}

impl CacheInventoryReport {
    #[must_use]
    pub const fn authority(&self) -> CacheInventoryAuthority {
        self.authority
    }

    #[must_use]
    pub const fn operation(&self) -> CacheReportOperation {
        self.operation
    }

    #[must_use]
    pub const fn summary(&self) -> &CacheInventorySummary {
        &self.summary
    }

    #[must_use]
    pub fn states(&self) -> &[CacheStateReport] {
        &self.states
    }
}

/// Build one deterministic report for status, explain, or reclaim dry-run output.
///
/// # Errors
///
/// Returns an error when an explained identity does not exist or aggregate byte arithmetic
/// overflows.
pub fn build_cache_inventory_report(
    document: &CacheInventoryDocument,
    request: &CacheReportRequest,
) -> Result<CacheInventoryReport, CacheInventoryError> {
    build_cache_inventory_report_with_authority(
        document,
        request,
        CacheInventoryAuthority::SuppliedObservationOnly,
    )
}

/// Build an observation-only report from the local hot-run filesystem producer.
///
/// This authority describes only where the byte observations came from. The producer leaves every
/// ownership and lifecycle fact unknown, so this function cannot make a state reclaimable.
#[cfg(target_os = "linux")]
pub fn build_local_hot_run_cache_report(
    document: &CacheInventoryDocument,
) -> Result<CacheInventoryReport, CacheInventoryError> {
    build_cache_inventory_report_with_authority(
        document,
        &CacheReportRequest::Status,
        CacheInventoryAuthority::LocalHotRunFilesystemObservation,
    )
}

fn build_cache_inventory_report_with_authority(
    document: &CacheInventoryDocument,
    request: &CacheReportRequest,
    authority: CacheInventoryAuthority,
) -> Result<CacheInventoryReport, CacheInventoryError> {
    let operation = match request {
        CacheReportRequest::Status => CacheReportOperation::Status,
        CacheReportRequest::Explain(_) => CacheReportOperation::Explain,
        CacheReportRequest::ReclaimDryRun => CacheReportOperation::ReclaimDryRun,
    };
    let mut states = document
        .states
        .iter()
        .filter(|state| match request {
            CacheReportRequest::Explain(expected) => &state.state_id == expected,
            CacheReportRequest::Status | CacheReportRequest::ReclaimDryRun => true,
        })
        .map(classify_state)
        .collect::<Vec<_>>();
    if matches!(request, CacheReportRequest::Explain(_)) && states.is_empty() {
        return Err(error(
            CacheInventoryErrorKind::StateNotFound,
            "cache state identity was not found",
        ));
    }
    states.sort_by(|left, right| left.state_id.cmp(&right.state_id));
    let summary = summarize(&states)?;
    Ok(CacheInventoryReport {
        schema_version: CACHE_INVENTORY_REPORT_SCHEMA_VERSION,
        authority,
        operation,
        mutation_performed: false,
        summary,
        states,
    })
}

fn classify_state(state: &CacheStateObservation) -> CacheStateReport {
    let mut unknown_reasons = Vec::new();
    match state.ownership {
        OwnershipObservation::ExactGlaedaOwned => {}
        OwnershipObservation::Unmanaged => unknown_reasons.push(CacheStateReason::Unmanaged),
        OwnershipObservation::Unknown => {
            unknown_reasons.push(CacheStateReason::OwnershipUnknown);
        }
    }
    collect_unknown_bool(
        state.active_lease,
        CacheStateReason::ActiveLeaseUnknown,
        &mut unknown_reasons,
    );
    collect_unknown_bool(
        state.active_lock,
        CacheStateReason::ActiveLockUnknown,
        &mut unknown_reasons,
    );
    collect_unknown_bool(
        state.mounted,
        CacheStateReason::MountStatusUnknown,
        &mut unknown_reasons,
    );
    if state.open_file_count.is_none() {
        unknown_reasons.push(CacheStateReason::OpenFilesUnknown);
    }
    if state.live_owned_process_count.is_none() {
        unknown_reasons.push(CacheStateReason::LiveOwnedProcessesUnknown);
    }
    if state.generation == GenerationObservation::Unknown {
        unknown_reasons.push(CacheStateReason::GenerationUnknown);
    }
    if state.worktree == WorktreeObservation::Unknown {
        unknown_reasons.push(CacheStateReason::WorktreeUnknown);
    }
    collect_unknown_bool(
        state.interrupted_cleanup,
        CacheStateReason::InterruptedCleanupUnknown,
        &mut unknown_reasons,
    );
    if state.reconstruction == ReconstructionObservation::Unknown {
        unknown_reasons.push(CacheStateReason::ColdReconstructionUnknown);
    }
    collect_unknown_bool(
        state.quarantined,
        CacheStateReason::QuarantineStatusUnknown,
        &mut unknown_reasons,
    );

    let active_reasons = known_active_reasons(state);
    let (classification, reasons) = if state.quarantined == Some(true) {
        (
            CacheStateClassification::Quarantined,
            vec![CacheStateReason::Quarantined],
        )
    } else if !active_reasons.is_empty() {
        let mut reasons = active_reasons;
        reasons.extend(unknown_reasons);
        (CacheStateClassification::InUse, reasons)
    } else if !unknown_reasons.is_empty() {
        (CacheStateClassification::Unknown, unknown_reasons)
    } else {
        classify_complete_state(state)
    };

    CacheStateReport {
        state_id: state.state_id.clone(),
        classification,
        reasons,
        logical_bytes: state.logical_bytes,
        allocated_bytes: state.allocated_bytes,
    }
}

fn collect_unknown_bool(
    observation: Option<bool>,
    reason: CacheStateReason,
    reasons: &mut Vec<CacheStateReason>,
) {
    if observation.is_none() {
        reasons.push(reason);
    }
}

fn classify_complete_state(
    state: &CacheStateObservation,
) -> (CacheStateClassification, Vec<CacheStateReason>) {
    debug_assert!(known_active_reasons(state).is_empty());

    let mut warm = Vec::new();
    if state.generation == GenerationObservation::Current {
        warm.push(CacheStateReason::CurrentGeneration);
    }
    if state.worktree == WorktreeObservation::Present {
        warm.push(CacheStateReason::WorktreePresent);
    }
    if !warm.is_empty() {
        return (CacheStateClassification::Warm, warm);
    }

    let mut vetoes = Vec::new();
    if state.interrupted_cleanup == Some(true) {
        vetoes.push(CacheStateReason::InterruptedCleanup);
    }
    match state.reconstruction {
        ReconstructionObservation::Proven => {}
        ReconstructionObservation::Unproven => {
            vetoes.push(CacheStateReason::ColdReconstructionUnproven);
        }
        ReconstructionObservation::Unavailable => {
            vetoes.push(CacheStateReason::ColdReconstructionUnavailable);
        }
        ReconstructionObservation::Unknown => unreachable!("unknown handled before classification"),
    }
    if !vetoes.is_empty() {
        return (CacheStateClassification::Unknown, vetoes);
    }

    if state.ownership == OwnershipObservation::ExactGlaedaOwned
        && state.generation == GenerationObservation::Retired
        && matches!(
            state.worktree,
            WorktreeObservation::Detached | WorktreeObservation::Removed
        )
    {
        (CacheStateClassification::Reclaimable, Vec::new())
    } else {
        (CacheStateClassification::Unknown, Vec::new())
    }
}

fn known_active_reasons(state: &CacheStateObservation) -> Vec<CacheStateReason> {
    let mut active = Vec::new();
    if state.active_lease == Some(true) {
        active.push(CacheStateReason::ActiveLease);
    }
    if state.active_lock == Some(true) {
        active.push(CacheStateReason::ActiveLock);
    }
    if state.mounted == Some(true) {
        active.push(CacheStateReason::Mounted);
    }
    if state.open_file_count.is_some_and(|count| count > 0) {
        active.push(CacheStateReason::OpenFiles);
    }
    if state
        .live_owned_process_count
        .is_some_and(|count| count > 0)
    {
        active.push(CacheStateReason::LiveOwnedProcesses);
    }
    active
}

fn summarize(states: &[CacheStateReport]) -> Result<CacheInventorySummary, CacheInventoryError> {
    let mut summary = CacheInventorySummary {
        state_count: u32::try_from(states.len()).expect("state bound fits u32"),
        in_use_count: 0,
        warm_count: 0,
        reclaimable_count: 0,
        quarantined_count: 0,
        unknown_count: 0,
        logical_bytes: 0,
        allocated_bytes: 0,
        reclaimable_allocated_bytes: 0,
    };
    for state in states {
        summary.logical_bytes = checked_bytes(summary.logical_bytes, state.logical_bytes)?;
        summary.allocated_bytes = checked_bytes(summary.allocated_bytes, state.allocated_bytes)?;
        match state.classification {
            CacheStateClassification::InUse => summary.in_use_count += 1,
            CacheStateClassification::Warm => summary.warm_count += 1,
            CacheStateClassification::Reclaimable => {
                summary.reclaimable_count += 1;
                summary.reclaimable_allocated_bytes =
                    checked_bytes(summary.reclaimable_allocated_bytes, state.allocated_bytes)?;
            }
            CacheStateClassification::Quarantined => summary.quarantined_count += 1,
            CacheStateClassification::Unknown => summary.unknown_count += 1,
        }
    }
    Ok(summary)
}

fn checked_bytes(left: u64, right: u64) -> Result<u64, CacheInventoryError> {
    left.checked_add(right).ok_or_else(|| {
        error(
            CacheInventoryErrorKind::AggregateOverflow,
            "cache inventory byte aggregate overflows the reviewed representation",
        )
    })
}

#[must_use]
pub fn render_cache_inventory_human(report: &CacheInventoryReport) -> String {
    let mut output = format!(
        "cache {}\nauthority: {}\nstates={}, reclaimable={}, reclaimable_allocated_bytes={}, mutation_performed=false\n",
        report.operation.as_str(),
        report.authority.as_str(),
        report.summary.state_count,
        report.summary.reclaimable_count,
        report.summary.reclaimable_allocated_bytes,
    );
    for state in &report.states {
        let reasons = if state.reasons.is_empty() {
            "none".to_owned()
        } else {
            state
                .reasons
                .iter()
                .map(|reason| reason.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        output.push_str(&format!(
            "{}: {} (allocated_bytes={}, reasons={})\n",
            state.state_id.as_str(),
            state.classification.as_str(),
            state.allocated_bytes,
            reasons,
        ));
    }
    output
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheInventoryErrorKind {
    InvalidDocument,
    VersionIncompatible,
    DocumentTooLarge,
    TooManyStates,
    InvalidState,
    DuplicateState,
    StateNotFound,
    AggregateOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheInventoryError {
    kind: CacheInventoryErrorKind,
    message: &'static str,
}

impl CacheInventoryError {
    #[must_use]
    pub const fn kind(&self) -> CacheInventoryErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.kind {
            CacheInventoryErrorKind::InvalidDocument => "cache_inventory_invalid_document",
            CacheInventoryErrorKind::VersionIncompatible => "cache_inventory_version_incompatible",
            CacheInventoryErrorKind::DocumentTooLarge => "cache_inventory_document_too_large",
            CacheInventoryErrorKind::TooManyStates => "cache_inventory_too_many_states",
            CacheInventoryErrorKind::InvalidState => "cache_inventory_invalid_state",
            CacheInventoryErrorKind::DuplicateState => "cache_inventory_duplicate_state",
            CacheInventoryErrorKind::StateNotFound => "cache_inventory_state_not_found",
            CacheInventoryErrorKind::AggregateOverflow => "cache_inventory_aggregate_overflow",
        }
    }
}

impl fmt::Display for CacheInventoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CacheInventoryError {}

const fn error(kind: CacheInventoryErrorKind, message: &'static str) -> CacheInventoryError {
    CacheInventoryError { kind, message }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        CacheInventoryAuthority, CacheInventoryErrorKind, CacheReportRequest,
        CacheStateClassification, CacheStateId, CacheStateReason, build_cache_inventory_report,
        decode_cache_inventory, render_cache_inventory_human,
    };

    fn document(states: Vec<Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({"schema_version": 1, "states": states}))
            .expect("encode test document")
    }

    fn state(id: &str, overrides: &[(&str, Value)]) -> Value {
        let mut value = json!({
            "state_id": id,
            "ownership": "exact_glaeda_owned",
            "generation": "retired",
            "worktree": "removed",
            "reconstruction": "proven",
            "logical_bytes": 100,
            "allocated_bytes": 80,
            "active_lease": false,
            "active_lock": false,
            "mounted": false,
            "open_file_count": 0,
            "live_owned_process_count": 0,
            "interrupted_cleanup": false,
            "quarantined": false,
        });
        let object = value.as_object_mut().expect("state fixture is an object");
        for (key, replacement) in overrides {
            object.insert((*key).to_owned(), replacement.clone());
        }
        value
    }

    #[test]
    fn exact_retired_inactive_reconstructable_state_is_reclaimable() {
        let inventory = decode_cache_inventory(&document(vec![state("state-one", &[])]))
            .expect("decode inventory");
        let report = build_cache_inventory_report(&inventory, &CacheReportRequest::Status)
            .expect("build report");

        assert_eq!(report.states().len(), 1);
        assert_eq!(
            report.authority(),
            CacheInventoryAuthority::SuppliedObservationOnly
        );
        assert!(
            render_cache_inventory_human(&report)
                .contains("authority: supplied_observation_only\n")
        );
        assert_eq!(
            report.states()[0].classification(),
            CacheStateClassification::Reclaimable
        );
        assert!(report.states()[0].reasons().is_empty());
    }

    #[test]
    fn unmanaged_state_is_unknown_even_when_other_observations_look_safe() {
        let unsafe_state = state("cargo-target", &[("ownership", json!("unmanaged"))]);
        let inventory =
            decode_cache_inventory(&document(vec![unsafe_state])).expect("decode inventory");
        let report = build_cache_inventory_report(&inventory, &CacheReportRequest::Status)
            .expect("build report");

        assert_eq!(
            report.states()[0].classification(),
            CacheStateClassification::Unknown
        );
        assert_eq!(report.states()[0].reasons(), &[CacheStateReason::Unmanaged]);
    }

    #[test]
    fn unknown_active_evidence_fails_closed() {
        let unknown = state("state-one", &[("active_lease", Value::Null)]);
        let inventory = decode_cache_inventory(&document(vec![unknown])).expect("decode inventory");
        let report = build_cache_inventory_report(&inventory, &CacheReportRequest::Status)
            .expect("build report");

        assert_eq!(
            report.states()[0].classification(),
            CacheStateClassification::Unknown
        );
        assert_eq!(
            report.states()[0].reasons(),
            &[CacheStateReason::ActiveLeaseUnknown]
        );
    }

    #[test]
    fn definitely_active_evidence_dominates_unrelated_unknowns() {
        let active = state(
            "state-one",
            &[
                ("ownership", json!("unknown")),
                ("generation", json!("unknown")),
                ("worktree", json!("unknown")),
                ("reconstruction", json!("unknown")),
                ("active_lease", Value::Null),
                ("active_lock", json!(true)),
                ("mounted", Value::Null),
                ("open_file_count", Value::Null),
                ("live_owned_process_count", Value::Null),
                ("interrupted_cleanup", Value::Null),
                ("quarantined", Value::Null),
            ],
        );
        let inventory = decode_cache_inventory(&document(vec![active])).expect("decode inventory");
        let report = build_cache_inventory_report(&inventory, &CacheReportRequest::Status)
            .expect("build report");

        assert_eq!(
            report.states()[0].classification(),
            CacheStateClassification::InUse
        );
        assert_eq!(
            report.states()[0].reasons()[0],
            CacheStateReason::ActiveLock
        );
        assert!(
            report.states()[0]
                .reasons()
                .contains(&CacheStateReason::OwnershipUnknown)
        );
        assert!(
            report.states()[0]
                .reasons()
                .contains(&CacheStateReason::QuarantineStatusUnknown)
        );
    }

    #[test]
    fn active_and_current_states_are_never_reclaimable() {
        let active = state("active", &[("open_file_count", json!(2))]);
        let current = state("current", &[("generation", json!("current"))]);
        let inventory =
            decode_cache_inventory(&document(vec![active, current])).expect("decode inventory");
        let report = build_cache_inventory_report(&inventory, &CacheReportRequest::ReclaimDryRun)
            .expect("build report");

        assert_eq!(
            report.states()[0].classification(),
            CacheStateClassification::InUse
        );
        assert_eq!(
            report.states()[1].classification(),
            CacheStateClassification::Warm
        );
    }

    #[test]
    fn quarantine_has_explicit_precedence() {
        let quarantined = state(
            "state-one",
            &[
                ("ownership", json!("unknown")),
                ("active_lock", Value::Null),
                ("quarantined", json!(true)),
            ],
        );
        let inventory =
            decode_cache_inventory(&document(vec![quarantined])).expect("decode inventory");
        let report = build_cache_inventory_report(&inventory, &CacheReportRequest::Status)
            .expect("build report");

        assert_eq!(
            report.states()[0].classification(),
            CacheStateClassification::Quarantined
        );
        assert_eq!(
            report.states()[0].reasons(),
            &[CacheStateReason::Quarantined]
        );
    }

    #[test]
    fn decoder_rejects_unknown_missing_duplicate_and_future_input() {
        let unknown_field = br#"{"schema_version":1,"states":[],"path":"/secret"}"#;
        assert_eq!(
            decode_cache_inventory(unknown_field)
                .expect_err("unknown field must fail")
                .kind(),
            CacheInventoryErrorKind::InvalidDocument
        );

        let missing = br#"{"schema_version":1,"states":[{"state_id":"one"}]}"#;
        assert_eq!(
            decode_cache_inventory(missing)
                .expect_err("missing evidence must fail")
                .kind(),
            CacheInventoryErrorKind::InvalidDocument
        );

        let duplicate = document(vec![state("same", &[]), state("same", &[])]);
        assert_eq!(
            decode_cache_inventory(&duplicate)
                .expect_err("duplicate state must fail")
                .kind(),
            CacheInventoryErrorKind::DuplicateState
        );

        let future = br#"{"schema_version":2,"states":[]}"#;
        assert_eq!(
            decode_cache_inventory(future)
                .expect_err("future version must fail")
                .kind(),
            CacheInventoryErrorKind::VersionIncompatible
        );
    }

    #[test]
    fn explain_is_exact_and_reports_missing_identity() {
        let inventory = decode_cache_inventory(&document(vec![state("state-one", &[])]))
            .expect("decode inventory");
        let found = build_cache_inventory_report(
            &inventory,
            &CacheReportRequest::Explain(CacheStateId::parse("state-one").expect("valid ID")),
        )
        .expect("explain state");
        assert_eq!(found.states().len(), 1);

        let missing = build_cache_inventory_report(
            &inventory,
            &CacheReportRequest::Explain(CacheStateId::parse("missing").expect("valid ID")),
        )
        .expect_err("missing state must fail");
        assert_eq!(missing.kind(), CacheInventoryErrorKind::StateNotFound);
    }
}
