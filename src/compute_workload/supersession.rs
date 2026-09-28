//! Pure semantic currentness, supersession, and result-precedence rules.
//!
//! This module keeps semantic work replacement separate from physical attempt settlement.
//! Superseding generation N immediately makes N historical for result publication, while any
//! physical attempt for N remains in its existing settlement state until its owning lifecycle
//! proves quiescence. No function in this module signals processes, releases capacity or leases,
//! retries mutations, persists state, or starts execution.

use std::fmt;

use serde::Serialize;

use crate::compute_workload::{
    ComputeSemanticGeneration, ComputeWorkloadIdentity, MAX_COMPUTE_SEMANTIC_GENERATION,
};

pub const WORK_SUPERSESSION_SCHEMA_VERSION: u8 = 1;
pub const MAX_WORK_ATTEMPT_GENERATION: u64 = 1_000_000_000_000;

const MAX_WORK_ID_BYTES: usize = 96;
const OPAQUE_ID_HEX_BYTES: usize = 64;
const WORK_LINEAGE_ID_PREFIX: &str = "work-lineage-v1-";
const WORK_SUPERSESSION_REQUEST_ID_PREFIX: &str = "work-supersession-v1-";

macro_rules! opaque_identity_type {
    ($name:ident, $field:literal, $code:literal, $prefix:expr) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, WorkSupersessionError> {
                validate_opaque_identity($field, $code, value, $prefix)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(<opaque>)"))
            }
        }
    };
}

opaque_identity_type!(
    WorkLineageId,
    "lineage_id",
    "invalid_work_lineage_id",
    WORK_LINEAGE_ID_PREFIX
);
opaque_identity_type!(
    WorkSupersessionRequestId,
    "supersession_request_id",
    "invalid_work_supersession_request_id",
    WORK_SUPERSESSION_REQUEST_ID_PREFIX
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkAttemptGeneration(u64);

impl WorkAttemptGeneration {
    pub fn new(value: u64) -> Result<Self, WorkSupersessionError> {
        if !(1..=MAX_WORK_ATTEMPT_GENERATION).contains(&value) {
            return Err(WorkSupersessionError::new(
                "attempt_generation",
                "invalid_work_attempt_generation",
                "work attempt generation must be within the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Semantic publication state for one workload generation.
///
/// Supersession changes only this axis. It never implies that an execution attempt stopped or that
/// its resources can be released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkSemanticCurrentness {
    Current,
    Superseded { by: ComputeSemanticGeneration },
}

/// Physical settlement state for one concrete execution attempt.
///
/// These values are observations/declarations only in this module. Family lifecycle owners decide
/// how an attempt enters or leaves them and retain all signal, cleanup, and release authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAttemptSettlement {
    Live,
    Cancelling,
    Terminal,
    CancelledTerminal,
    CancellationAmbiguous,
    RecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkEvidenceDisposition {
    CurrentEligible,
    HistoricalOnly,
}

/// One semantic generation inside a stable work lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkSemanticRecord {
    schema_version: u8,
    lineage_id: WorkLineageId,
    workload: ComputeWorkloadIdentity,
    currentness: WorkSemanticCurrentness,
}

impl WorkSemanticRecord {
    #[must_use]
    pub const fn current(lineage_id: WorkLineageId, workload: ComputeWorkloadIdentity) -> Self {
        Self {
            schema_version: WORK_SUPERSESSION_SCHEMA_VERSION,
            lineage_id,
            workload,
            currentness: WorkSemanticCurrentness::Current,
        }
    }

    #[must_use]
    pub const fn lineage_id(&self) -> &WorkLineageId {
        &self.lineage_id
    }

    #[must_use]
    pub const fn workload(&self) -> &ComputeWorkloadIdentity {
        &self.workload
    }

    #[must_use]
    pub const fn currentness(&self) -> WorkSemanticCurrentness {
        self.currentness
    }
}

/// One physical attempt for one exact semantic workload generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkAttemptRecord {
    schema_version: u8,
    lineage_id: WorkLineageId,
    workload: ComputeWorkloadIdentity,
    attempt_generation: WorkAttemptGeneration,
    settlement: WorkAttemptSettlement,
}

impl WorkAttemptRecord {
    #[must_use]
    pub const fn new(
        lineage_id: WorkLineageId,
        workload: ComputeWorkloadIdentity,
        attempt_generation: WorkAttemptGeneration,
        settlement: WorkAttemptSettlement,
    ) -> Self {
        Self {
            schema_version: WORK_SUPERSESSION_SCHEMA_VERSION,
            lineage_id,
            workload,
            attempt_generation,
            settlement,
        }
    }

    #[must_use]
    pub const fn lineage_id(&self) -> &WorkLineageId {
        &self.lineage_id
    }

    #[must_use]
    pub const fn workload(&self) -> &ComputeWorkloadIdentity {
        &self.workload
    }

    #[must_use]
    pub const fn attempt_generation(&self) -> WorkAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn settlement(&self) -> WorkAttemptSettlement {
        self.settlement
    }
}

/// Declarative semantic replacement request.
///
/// The durable owner later binds this request ID to one exact predecessor/successor pair. This
/// value itself grants no persistence, execution, cancellation, retry, or release authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkSupersessionRequest {
    schema_version: u8,
    request_id: WorkSupersessionRequestId,
    lineage_id: WorkLineageId,
    expected_current: ComputeWorkloadIdentity,
    successor: ComputeWorkloadIdentity,
}

impl WorkSupersessionRequest {
    #[must_use]
    pub const fn new(
        request_id: WorkSupersessionRequestId,
        lineage_id: WorkLineageId,
        expected_current: ComputeWorkloadIdentity,
        successor: ComputeWorkloadIdentity,
    ) -> Self {
        Self {
            schema_version: WORK_SUPERSESSION_SCHEMA_VERSION,
            request_id,
            lineage_id,
            expected_current,
            successor,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> &WorkSupersessionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn lineage_id(&self) -> &WorkLineageId {
        &self.lineage_id
    }

    #[must_use]
    pub const fn expected_current(&self) -> &ComputeWorkloadIdentity {
        &self.expected_current
    }

    #[must_use]
    pub const fn successor(&self) -> &ComputeWorkloadIdentity {
        &self.successor
    }
}

/// Durable idempotency binding supplied back to the pure reducer on request replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkSupersessionBinding {
    schema_version: u8,
    request_id: WorkSupersessionRequestId,
    lineage_id: WorkLineageId,
    predecessor: ComputeWorkloadIdentity,
    successor: ComputeWorkloadIdentity,
}

impl WorkSupersessionBinding {
    #[must_use]
    pub const fn request_id(&self) -> &WorkSupersessionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn lineage_id(&self) -> &WorkLineageId {
        &self.lineage_id
    }

    #[must_use]
    pub const fn predecessor(&self) -> &ComputeWorkloadIdentity {
        &self.predecessor
    }

    #[must_use]
    pub const fn successor(&self) -> &ComputeWorkloadIdentity {
        &self.successor
    }

    fn matches_request(&self, request: &WorkSupersessionRequest) -> bool {
        self.request_id == request.request_id
            && self.lineage_id == request.lineage_id
            && self.predecessor == request.expected_current
            && self.successor == request.successor
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum WorkSupersessionDecision {
    Apply {
        binding: WorkSupersessionBinding,
        predecessor: Box<WorkSemanticRecord>,
        successor: Box<WorkSemanticRecord>,
    },
    Duplicate {
        binding: WorkSupersessionBinding,
    },
}

/// Plan one exact semantic supersession using expected-current compare-and-swap semantics.
///
/// `existing_request_binding` comes from the future durable idempotency owner. Exact request replay
/// returns `Duplicate` and performs no semantic transition. Reusing a request ID with changed
/// semantics fails closed.
///
/// This reducer changes semantic currentness only. It never receives or returns an attempt record,
/// so physical settlement/capacity state cannot be implicitly advanced by supersession.
pub fn plan_work_supersession(
    current: &WorkSemanticRecord,
    request: &WorkSupersessionRequest,
    existing_request_binding: Option<&WorkSupersessionBinding>,
) -> Result<WorkSupersessionDecision, WorkSupersessionError> {
    if let Some(binding) = existing_request_binding {
        if binding.request_id() != request.request_id() {
            return Err(WorkSupersessionError::new(
                "existing_request_binding.request_id",
                "work_supersession_binding_request_mismatch",
                "existing supersession binding does not match the supplied request identity",
            ));
        }
        if binding.matches_request(request) {
            return Ok(WorkSupersessionDecision::Duplicate {
                binding: binding.clone(),
            });
        }
        return Err(WorkSupersessionError::new(
            "supersession_request_id",
            "work_supersession_request_conflict",
            "supersession request identity is already bound to different semantics",
        ));
    }

    if current.currentness != WorkSemanticCurrentness::Current {
        return Err(WorkSupersessionError::new(
            "current.currentness",
            "work_generation_not_current",
            "only the durable current semantic generation may be superseded",
        ));
    }
    if current.lineage_id != request.lineage_id {
        return Err(WorkSupersessionError::new(
            "lineage_id",
            "work_lineage_mismatch",
            "supersession request lineage does not match the current semantic record",
        ));
    }
    if current.workload != request.expected_current {
        return Err(WorkSupersessionError::new(
            "expected_current",
            "work_expected_current_mismatch",
            "supersession request does not match the exact current workload identity",
        ));
    }
    if current.workload.family() != request.successor.family() {
        return Err(WorkSupersessionError::new(
            "successor.family",
            "work_lineage_family_mismatch",
            "successor workload family must remain within the same work lineage",
        ));
    }

    let expected_successor_generation = current
        .workload
        .semantic_generation()
        .get()
        .checked_add(1)
        .filter(|value| *value <= MAX_COMPUTE_SEMANTIC_GENERATION)
        .ok_or_else(|| {
            WorkSupersessionError::new(
                "successor.semantic_generation",
                "work_semantic_generation_exhausted",
                "work semantic generation cannot advance within the bounded range",
            )
        })?;
    if request.successor.semantic_generation().get() != expected_successor_generation {
        return Err(WorkSupersessionError::new(
            "successor.semantic_generation",
            "work_successor_generation_mismatch",
            "successor semantic generation must advance the current generation exactly once",
        ));
    }

    let successor_generation = request.successor.semantic_generation();
    let predecessor = WorkSemanticRecord {
        schema_version: WORK_SUPERSESSION_SCHEMA_VERSION,
        lineage_id: current.lineage_id.clone(),
        workload: current.workload.clone(),
        currentness: WorkSemanticCurrentness::Superseded {
            by: successor_generation,
        },
    };
    let successor =
        WorkSemanticRecord::current(current.lineage_id.clone(), request.successor.clone());
    let binding = WorkSupersessionBinding {
        schema_version: WORK_SUPERSESSION_SCHEMA_VERSION,
        request_id: request.request_id.clone(),
        lineage_id: current.lineage_id.clone(),
        predecessor: current.workload.clone(),
        successor: request.successor.clone(),
    };

    Ok(WorkSupersessionDecision::Apply {
        binding,
        predecessor: Box::new(predecessor),
        successor: Box::new(successor),
    })
}

/// Classify whether an attempt result may be considered for current-result publication.
///
/// This is intentionally independent from settlement. A terminal predecessor can still be
/// historical-only; a live/current attempt is not a successful result merely because it is
/// current. Workload-family result validators retain final result authority.
#[must_use]
pub fn classify_attempt_result(
    current: &WorkSemanticRecord,
    attempt: &WorkAttemptRecord,
) -> WorkEvidenceDisposition {
    if current.currentness == WorkSemanticCurrentness::Current
        && current.lineage_id == attempt.lineage_id
        && current.workload == attempt.workload
    {
        WorkEvidenceDisposition::CurrentEligible
    } else {
        WorkEvidenceDisposition::HistoricalOnly
    }
}

fn validate_opaque_identity(
    field: &'static str,
    code: &'static str,
    value: &str,
    prefix: &str,
) -> Result<(), WorkSupersessionError> {
    let Some(payload) = value.strip_prefix(prefix) else {
        return Err(WorkSupersessionError::new(
            field,
            code,
            "work identity must use its versioned opaque identity form",
        ));
    };
    if value.len() > MAX_WORK_ID_BYTES
        || payload.len() != OPAQUE_ID_HEX_BYTES
        || !payload
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(WorkSupersessionError::new(
            field,
            code,
            "work identity must use its versioned opaque identity form",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkSupersessionError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl WorkSupersessionError {
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

impl fmt::Display for WorkSupersessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for WorkSupersessionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::compute_workload::{
        ComputeCapabilitySet, ComputeInputIdentity, ComputeOutputContractIdentity,
        ComputeTrustClass, ComputeWorkloadFamilyId,
    };

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn lineage(hex: char) -> WorkLineageId {
        WorkLineageId::parse(&format!("work-lineage-v1-{}", hex.to_string().repeat(64))).unwrap()
    }

    fn request_id(hex: char) -> WorkSupersessionRequestId {
        WorkSupersessionRequestId::parse(&format!(
            "work-supersession-v1-{}",
            hex.to_string().repeat(64)
        ))
        .unwrap()
    }

    fn workload(
        family: &str,
        generation: u64,
        input_hex: char,
        output_hex: char,
    ) -> ComputeWorkloadIdentity {
        ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse(family).unwrap(),
            ComputeSemanticGeneration::new(generation).unwrap(),
            ComputeInputIdentity::new(digest(input_hex)),
            ComputeTrustClass::UltraTrusted,
            ComputeCapabilitySet::empty(),
            ComputeOutputContractIdentity::new(digest(output_hex)),
        )
    }

    fn attempt(
        lineage_id: &WorkLineageId,
        workload: &ComputeWorkloadIdentity,
        attempt_generation: u64,
        settlement: WorkAttemptSettlement,
    ) -> WorkAttemptRecord {
        WorkAttemptRecord::new(
            lineage_id.clone(),
            workload.clone(),
            WorkAttemptGeneration::new(attempt_generation).unwrap(),
            settlement,
        )
    }

    #[test]
    fn supersession_revokes_predecessor_currentness_without_touching_attempt_settlement() {
        let line = lineage('a');
        let generation_one = workload("repository_verification.v1", 1, '1', '2');
        let generation_two = workload("repository_verification.v1", 2, '3', '2');
        let current = WorkSemanticRecord::current(line.clone(), generation_one.clone());
        let predecessor_attempt = attempt(&line, &generation_one, 1, WorkAttemptSettlement::Live);
        let request = WorkSupersessionRequest::new(
            request_id('1'),
            line.clone(),
            generation_one,
            generation_two.clone(),
        );

        let decision = plan_work_supersession(&current, &request, None).unwrap();
        let WorkSupersessionDecision::Apply {
            predecessor,
            successor,
            ..
        } = decision
        else {
            panic!("first supersession must apply");
        };
        assert_eq!(
            predecessor.currentness(),
            WorkSemanticCurrentness::Superseded {
                by: ComputeSemanticGeneration::new(2).unwrap()
            }
        );
        assert_eq!(successor.workload(), &generation_two);
        assert_eq!(
            predecessor_attempt.settlement(),
            WorkAttemptSettlement::Live
        );
        assert_eq!(
            classify_attempt_result(&successor, &predecessor_attempt),
            WorkEvidenceDisposition::HistoricalOnly
        );
    }

    #[test]
    fn current_attempt_is_eligible_and_retry_attempt_generation_is_independent() {
        let line = lineage('b');
        let semantic = workload("dataset_transform.v1", 7, '4', '5');
        let current = WorkSemanticRecord::current(line.clone(), semantic.clone());
        let first = attempt(&line, &semantic, 1, WorkAttemptSettlement::Terminal);
        let retry = attempt(&line, &semantic, 2, WorkAttemptSettlement::Live);

        assert_eq!(
            classify_attempt_result(&current, &first),
            WorkEvidenceDisposition::CurrentEligible
        );
        assert_eq!(
            classify_attempt_result(&current, &retry),
            WorkEvidenceDisposition::CurrentEligible
        );
        assert_eq!(first.workload().semantic_generation().get(), 7);
        assert_eq!(retry.workload().semantic_generation().get(), 7);
        assert_eq!(first.attempt_generation().get(), 1);
        assert_eq!(retry.attempt_generation().get(), 2);
    }

    #[test]
    fn exact_request_replay_is_duplicate_and_conflicting_reuse_fails() {
        let line = lineage('c');
        let first = workload("dataset_transform.v1", 3, '6', '7');
        let second = workload("dataset_transform.v1", 4, '8', '7');
        let current = WorkSemanticRecord::current(line.clone(), first.clone());
        let request = WorkSupersessionRequest::new(
            request_id('2'),
            line.clone(),
            first.clone(),
            second.clone(),
        );
        let applied = plan_work_supersession(&current, &request, None).unwrap();
        let WorkSupersessionDecision::Apply {
            binding, successor, ..
        } = applied
        else {
            panic!("first request must apply");
        };

        assert_eq!(
            plan_work_supersession(&successor, &request, Some(&binding)).unwrap(),
            WorkSupersessionDecision::Duplicate {
                binding: binding.clone()
            }
        );

        let conflicting = WorkSupersessionRequest::new(
            request_id('2'),
            line,
            first,
            workload("dataset_transform.v1", 4, '9', '7'),
        );
        assert_eq!(
            plan_work_supersession(&successor, &conflicting, Some(&binding))
                .unwrap_err()
                .code(),
            "work_supersession_request_conflict"
        );
    }

    #[test]
    fn expected_current_and_generation_cas_fail_closed() {
        let line = lineage('d');
        let first = workload("repository_verification.v1", 9, 'a', 'b');
        let current = WorkSemanticRecord::current(line.clone(), first.clone());

        let stale_expected = WorkSupersessionRequest::new(
            request_id('3'),
            line.clone(),
            workload("repository_verification.v1", 8, 'a', 'b'),
            workload("repository_verification.v1", 10, 'c', 'b'),
        );
        assert_eq!(
            plan_work_supersession(&current, &stale_expected, None)
                .unwrap_err()
                .code(),
            "work_expected_current_mismatch"
        );

        let skipped = WorkSupersessionRequest::new(
            request_id('4'),
            line,
            first,
            workload("repository_verification.v1", 11, 'c', 'b'),
        );
        assert_eq!(
            plan_work_supersession(&current, &skipped, None)
                .unwrap_err()
                .code(),
            "work_successor_generation_mismatch"
        );
    }

    #[test]
    fn successor_cannot_change_workload_family_inside_one_lineage() {
        let line = lineage('e');
        let first = workload("repository_verification.v1", 1, 'c', 'd');
        let current = WorkSemanticRecord::current(line.clone(), first.clone());
        let request = WorkSupersessionRequest::new(
            request_id('5'),
            line,
            first,
            workload("dataset_transform.v1", 2, 'e', 'f'),
        );
        assert_eq!(
            plan_work_supersession(&current, &request, None)
                .unwrap_err()
                .code(),
            "work_lineage_family_mismatch"
        );
    }

    #[test]
    fn successor_failure_never_resurrects_predecessor_result() {
        let line = lineage('f');
        let first = workload("repository_verification.v1", 20, '1', '2');
        let second = workload("repository_verification.v1", 21, '3', '2');
        let current = WorkSemanticRecord::current(line.clone(), first.clone());
        let old_attempt = attempt(&line, &first, 1, WorkAttemptSettlement::Terminal);
        let request =
            WorkSupersessionRequest::new(request_id('6'), line.clone(), first, second.clone());
        let WorkSupersessionDecision::Apply { successor, .. } =
            plan_work_supersession(&current, &request, None).unwrap()
        else {
            panic!("supersession must apply");
        };
        let failed_successor = attempt(&line, &second, 1, WorkAttemptSettlement::CancelledTerminal);

        assert_eq!(
            classify_attempt_result(&successor, &failed_successor),
            WorkEvidenceDisposition::CurrentEligible
        );
        assert_eq!(
            classify_attempt_result(&successor, &old_attempt),
            WorkEvidenceDisposition::HistoricalOnly
        );
    }

    #[test]
    fn foreign_lineage_attempt_is_always_historical() {
        let semantic = workload("dataset_transform.v1", 1, '4', '5');
        let current = WorkSemanticRecord::current(lineage('1'), semantic.clone());
        let foreign = attempt(&lineage('2'), &semantic, 1, WorkAttemptSettlement::Terminal);
        assert_eq!(
            classify_attempt_result(&current, &foreign),
            WorkEvidenceDisposition::HistoricalOnly
        );
    }

    #[test]
    fn opaque_ids_and_attempt_generations_are_bounded() {
        for invalid in [
            "work-lineage-v1-nothex",
            "work-lineage-v1-ABCDEF",
            "/private/work-lineage",
            "cargo test",
        ] {
            assert_eq!(
                WorkLineageId::parse(invalid).unwrap_err().code(),
                "invalid_work_lineage_id"
            );
        }
        assert_eq!(
            WorkAttemptGeneration::new(0).unwrap_err().code(),
            "invalid_work_attempt_generation"
        );
        assert_eq!(
            WorkAttemptGeneration::new(MAX_WORK_ATTEMPT_GENERATION + 1)
                .unwrap_err()
                .code(),
            "invalid_work_attempt_generation"
        );
    }

    #[test]
    fn public_json_has_no_execution_or_release_surface() {
        let line = lineage('3');
        let first = workload("dataset_transform.v1", 1, '6', '7');
        let second = workload("dataset_transform.v1", 2, '8', '7');
        let current = WorkSemanticRecord::current(line.clone(), first.clone());
        let request = WorkSupersessionRequest::new(request_id('7'), line, first, second);
        let json =
            serde_json::to_string(&plan_work_supersession(&current, &request, None).unwrap())
                .unwrap();

        for forbidden in [
            "/private/",
            "cargo test",
            "command",
            "argv",
            "process",
            "pid",
            "signal",
            "kill",
            "capacity_claim",
            "release",
            "lease_release",
            "retry",
            "backend",
            "limactl",
        ] {
            assert!(
                !json.contains(forbidden),
                "unexpected public surface: {forbidden}"
            );
        }
    }
}
