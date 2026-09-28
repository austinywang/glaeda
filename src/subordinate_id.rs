use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
use crate::lane_command::{LaneCommand, LaneCommandError, LinuxAccountName, RunnerUserContext};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PreparationObservationState, RunnerAccountObservations,
};
use crate::runner_user::MIN_SUBORDINATE_ID_COUNT;

pub const MAX_SUBORDINATE_AUTHORITY_BYTES: usize = 1_048_576;
const MAX_SUBORDINATE_RECORDS: usize = 16_384;
const MAX_SUBORDINATE_LINE_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SubordinateIdRange {
    start: u32,
    count: u32,
}

impl SubordinateIdRange {
    /// Build one nonempty subordinate-ID range inside the Linux 32-bit ID space.
    ///
    /// # Errors
    ///
    /// Returns an error when the range starts at zero, is empty, or overflows.
    pub fn new(start: u32, count: u32) -> Result<Self, SubordinateAuthorityError> {
        let end_exclusive = u64::from(start) + u64::from(count);
        if start == 0 || count == 0 || end_exclusive > u64::from(u32::MAX) + 1 {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                None,
                "subordinate-ID range must begin above zero, be nonempty, and remain within the 32-bit ID space",
            ));
        }
        Ok(Self { start, count })
    }

    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    #[must_use]
    pub const fn count(self) -> u32 {
        self.count
    }

    #[must_use]
    pub const fn end_exclusive(self) -> u64 {
        self.start as u64 + self.count as u64
    }

    #[must_use]
    pub fn end_inclusive(self) -> u32 {
        u32::try_from(self.end_exclusive() - 1)
            .expect("validated subordinate-ID range ends within u32")
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        u64::from(self.start) < other.end_exclusive()
            && u64::from(other.start) < self.end_exclusive()
    }

    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        self.start <= other.start && self.end_exclusive() >= other.end_exclusive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SubordinateIdOwner(String);

impl SubordinateIdOwner {
    /// Parse one authority-file owner as a reviewed account name or canonical positive numeric ID.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, option-shaped, noncanonical, or otherwise unsafe owners.
    pub fn parse(value: &str) -> Result<Self, SubordinateAuthorityError> {
        let valid_name = LinuxAccountName::parse(value).is_ok();
        let valid_numeric = canonical_u32(value).is_some_and(|identifier| identifier > 0);
        if valid_name || valid_numeric {
            Ok(Self(value.to_owned()))
        } else {
            Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                None,
                "subordinate-ID owner must be a reviewed account name or canonical positive numeric ID",
            ))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&LinuxAccountName> for SubordinateIdOwner {
    fn from(value: &LinuxAccountName) -> Self {
        Self(value.as_str().to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinateIdRecord {
    pub owner: SubordinateIdOwner,
    pub range: SubordinateIdRange,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinateIdAuthority {
    records: Vec<SubordinateIdRecord>,
}

impl SubordinateIdAuthority {
    #[must_use]
    pub fn records(&self) -> &[SubordinateIdRecord] {
        &self.records
    }

    #[must_use]
    pub fn range_for(&self, owner: &SubordinateIdOwner) -> Option<SubordinateIdRange> {
        self.records
            .iter()
            .find(|record| &record.owner == owner)
            .map(|record| record.range)
    }

    #[must_use]
    pub fn overlaps(&self, range: SubordinateIdRange) -> bool {
        self.records
            .iter()
            .any(|record| record.range.overlaps(range))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubordinateAuthorityErrorKind {
    SizeLimit,
    RecordLimit,
    LineLimit,
    MalformedRow,
    DuplicateOwner,
    Overlap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinateAuthorityError {
    kind: SubordinateAuthorityErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    public_message: String,
}

impl SubordinateAuthorityError {
    #[must_use]
    pub const fn kind(&self) -> SubordinateAuthorityErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn line(&self) -> Option<usize> {
        self.line
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(
        kind: SubordinateAuthorityErrorKind,
        line: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            line,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for SubordinateAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for SubordinateAuthorityError {}

/// Parse and validate one complete bounded `/etc/subuid` or `/etc/subgid` authority snapshot.
///
/// Empty files are valid. Every nonempty row must be canonical. Duplicate owners and any global
/// overlap are rejected, while exactly adjacent ranges are accepted.
///
/// # Errors
///
/// Returns a typed bounded error for oversized input, too many rows, malformed rows, duplicate
/// owners, or overlapping ranges.
pub fn parse_subordinate_authority(
    input: &str,
) -> Result<SubordinateIdAuthority, SubordinateAuthorityError> {
    if input.len() > MAX_SUBORDINATE_AUTHORITY_BYTES {
        return Err(SubordinateAuthorityError::new(
            SubordinateAuthorityErrorKind::SizeLimit,
            None,
            format!("subordinate-ID authority exceeds {MAX_SUBORDINATE_AUTHORITY_BYTES} bytes"),
        ));
    }
    if input.contains('\0') || (!input.is_empty() && !input.ends_with('\n')) {
        return Err(SubordinateAuthorityError::new(
            SubordinateAuthorityErrorKind::MalformedRow,
            None,
            "subordinate-ID authority must be NUL-free and newline-terminated",
        ));
    }

    let mut records = Vec::new();
    let mut owners = BTreeSet::new();
    for (index, line) in input.lines().enumerate() {
        let line_number = index + 1;
        if line.is_empty() {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                Some(line_number),
                format!("subordinate-ID authority line {line_number} is empty"),
            ));
        }
        if line.len() > MAX_SUBORDINATE_LINE_BYTES {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::LineLimit,
                Some(line_number),
                format!(
                    "subordinate-ID authority line {line_number} exceeds {MAX_SUBORDINATE_LINE_BYTES} bytes"
                ),
            ));
        }
        if line.chars().any(char::is_control) {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                Some(line_number),
                format!("subordinate-ID authority line {line_number} contains control characters"),
            ));
        }
        if records.len() == MAX_SUBORDINATE_RECORDS {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::RecordLimit,
                Some(line_number),
                format!(
                    "subordinate-ID authority contains more than {MAX_SUBORDINATE_RECORDS} records"
                ),
            ));
        }

        let mut fields = line.split(':');
        let owner = fields.next();
        let start = fields.next();
        let count = fields.next();
        if fields.next().is_some() || owner.is_none() || start.is_none() || count.is_none() {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                Some(line_number),
                format!("subordinate-ID authority line {line_number} must contain three fields"),
            ));
        }
        let owner =
            SubordinateIdOwner::parse(owner.expect("checked owner field")).map_err(|_| {
                SubordinateAuthorityError::new(
                    SubordinateAuthorityErrorKind::MalformedRow,
                    Some(line_number),
                    format!(
                        "subordinate-ID authority line {line_number} contains an invalid owner"
                    ),
                )
            })?;
        if !owners.insert(owner.clone()) {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::DuplicateOwner,
                Some(line_number),
                format!(
                    "subordinate-ID authority contains more than one record for owner {}",
                    owner.as_str()
                ),
            ));
        }
        let start = canonical_u32(start.expect("checked start field")).ok_or_else(|| {
            SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                Some(line_number),
                format!("subordinate-ID authority line {line_number} contains an invalid start"),
            )
        })?;
        let count = canonical_u32(count.expect("checked count field")).ok_or_else(|| {
            SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                Some(line_number),
                format!("subordinate-ID authority line {line_number} contains an invalid count"),
            )
        })?;
        let range = SubordinateIdRange::new(start, count).map_err(|_| {
            SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::MalformedRow,
                Some(line_number),
                format!("subordinate-ID authority line {line_number} contains an invalid range"),
            )
        })?;
        records.push(SubordinateIdRecord {
            owner,
            range,
            line: line_number,
        });
    }

    let mut sorted = records.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|record| (record.range.start(), record.range.count()));
    for pair in sorted.windows(2) {
        let previous = pair[0];
        let current = pair[1];
        if previous.range.overlaps(current.range) {
            return Err(SubordinateAuthorityError::new(
                SubordinateAuthorityErrorKind::Overlap,
                Some(current.line),
                format!(
                    "subordinate-ID ranges for {} and {} overlap",
                    previous.owner.as_str(),
                    current.owner.as_str()
                ),
            ));
        }
    }

    Ok(SubordinateIdAuthority { records })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SubordinateAllocationWindow {
    range: SubordinateIdRange,
}

impl SubordinateAllocationWindow {
    /// Build one caller-reviewed allocation window.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`SubordinateIdRange::new`].
    pub fn new(start: u32, count: u32) -> Result<Self, SubordinateAuthorityError> {
        Ok(Self {
            range: SubordinateIdRange::new(start, count)?,
        })
    }

    #[must_use]
    pub const fn range(self) -> SubordinateIdRange {
        self.range
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubordinateAllocationErrorKind {
    InvalidCount,
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinateAllocationError {
    kind: SubordinateAllocationErrorKind,
    public_message: String,
}

impl SubordinateAllocationError {
    #[must_use]
    pub const fn kind(&self) -> SubordinateAllocationErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }
}

impl fmt::Display for SubordinateAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for SubordinateAllocationError {}

/// Select the lowest deterministic non-overlapping range inside a caller-reviewed window.
///
/// # Errors
///
/// Returns `InvalidCount` below 65,536 IDs and `Exhausted` when the window has no fitting range.
pub fn select_free_subordinate_range(
    authority: &SubordinateIdAuthority,
    window: SubordinateAllocationWindow,
    count: u32,
) -> Result<SubordinateIdRange, SubordinateAllocationError> {
    if u64::from(count) < MIN_SUBORDINATE_ID_COUNT {
        return Err(SubordinateAllocationError {
            kind: SubordinateAllocationErrorKind::InvalidCount,
            public_message: "subordinate-ID allocation must contain at least 65536 IDs".to_owned(),
        });
    }
    let window = window.range();
    if u64::from(count) > u64::from(window.count()) {
        return Err(SubordinateAllocationError {
            kind: SubordinateAllocationErrorKind::Exhausted,
            public_message: "subordinate-ID allocation window is exhausted".to_owned(),
        });
    }

    let mut cursor = u64::from(window.start());
    let window_end = window.end_exclusive();
    let mut occupied = authority
        .records()
        .iter()
        .map(|record| record.range)
        .filter(|range| range.overlaps(window))
        .collect::<Vec<_>>();
    occupied.sort_by_key(|range| (range.start(), range.count()));

    for range in occupied {
        let range_start = u64::from(range.start()).max(u64::from(window.start()));
        if cursor + u64::from(count) <= range_start {
            return SubordinateIdRange::new(
                u32::try_from(cursor).expect("window cursor remains within u32"),
                count,
            )
            .map_err(|_| SubordinateAllocationError {
                kind: SubordinateAllocationErrorKind::Exhausted,
                public_message: "subordinate-ID allocation window is exhausted".to_owned(),
            });
        }
        cursor = cursor.max(range.end_exclusive());
        if cursor >= window_end {
            break;
        }
    }

    if cursor + u64::from(count) <= window_end {
        SubordinateIdRange::new(
            u32::try_from(cursor).expect("window cursor remains within u32"),
            count,
        )
        .map_err(|_| SubordinateAllocationError {
            kind: SubordinateAllocationErrorKind::Exhausted,
            public_message: "subordinate-ID allocation window is exhausted".to_owned(),
        })
    } else {
        Err(SubordinateAllocationError {
            kind: SubordinateAllocationErrorKind::Exhausted,
            public_message: "subordinate-ID allocation window is exhausted".to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubordinateIdRequest {
    Exact {
        range: SubordinateIdRange,
    },
    Allocate {
        window: SubordinateAllocationWindow,
        count: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubordinateMappingDisposition {
    Matching,
    Required,
    Conflicting,
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubordinateRangeSource {
    ExactPolicy,
    ExistingAuthority,
    DeterministicAllocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinateMappingDecision {
    pub disposition: SubordinateMappingDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<SubordinateIdRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SubordinateRangeSource>,
    pub evidence: Vec<String>,
}

#[must_use]
pub fn reconcile_subordinate_mapping(
    authority: &SubordinateIdAuthority,
    owner: &SubordinateIdOwner,
    request: SubordinateIdRequest,
) -> SubordinateMappingDecision {
    reconcile_subordinate_mapping_for_identity(authority, owner, None, request)
}

/// Reconcile one desired owner while treating a proven numeric UID as the same logical owner.
///
/// A username record and numeric-UID record for the same account are conflicting duplicate-owner
/// evidence. A single numeric record may be adopted only when the caller supplies that proven UID.
#[must_use]
pub fn reconcile_subordinate_mapping_for_identity(
    authority: &SubordinateIdAuthority,
    owner: &SubordinateIdOwner,
    numeric_uid: Option<u32>,
    request: SubordinateIdRequest,
) -> SubordinateMappingDecision {
    let numeric_owner = numeric_uid.map(|uid| SubordinateIdOwner(uid.to_string()));
    let matching = authority
        .records()
        .iter()
        .filter(|record| {
            &record.owner == owner
                || numeric_owner
                    .as_ref()
                    .is_some_and(|numeric_owner| &record.owner == numeric_owner)
        })
        .collect::<Vec<_>>();
    if matching.len() > 1 {
        return SubordinateMappingDecision {
            disposition: SubordinateMappingDisposition::Conflicting,
            range: None,
            source: Some(SubordinateRangeSource::ExistingAuthority),
            evidence: vec![
                "the desired account has duplicate username or numeric-UID authority records"
                    .to_owned(),
            ],
        };
    }

    if let Some(existing) = matching.first().map(|record| record.range) {
        return match request {
            SubordinateIdRequest::Exact { range } if existing == range => {
                SubordinateMappingDecision {
                    disposition: SubordinateMappingDisposition::Matching,
                    range: Some(existing),
                    source: Some(SubordinateRangeSource::ExistingAuthority),
                    evidence: vec![format!(
                        "owner {} already has exact range {}-{}",
                        owner.as_str(),
                        existing.start(),
                        existing.end_inclusive()
                    )],
                }
            }
            SubordinateIdRequest::Allocate { count, .. }
                if existing.count() >= count
                    && u64::from(existing.count()) >= MIN_SUBORDINATE_ID_COUNT =>
            {
                SubordinateMappingDecision {
                    disposition: SubordinateMappingDisposition::Matching,
                    range: Some(existing),
                    source: Some(SubordinateRangeSource::ExistingAuthority),
                    evidence: vec![format!(
                        "owner {} keeps existing valid range {}-{}",
                        owner.as_str(),
                        existing.start(),
                        existing.end_inclusive()
                    )],
                }
            }
            _ => SubordinateMappingDecision {
                disposition: SubordinateMappingDisposition::Conflicting,
                range: Some(existing),
                source: Some(SubordinateRangeSource::ExistingAuthority),
                evidence: vec![format!(
                    "owner {} has an incompatible existing range {}-{}",
                    owner.as_str(),
                    existing.start(),
                    existing.end_inclusive()
                )],
            },
        };
    }

    match request {
        SubordinateIdRequest::Exact { range } => {
            if authority.overlaps(range) {
                SubordinateMappingDecision {
                    disposition: SubordinateMappingDisposition::Conflicting,
                    range: Some(range),
                    source: Some(SubordinateRangeSource::ExactPolicy),
                    evidence: vec![format!(
                        "requested exact range {}-{} overlaps another owner",
                        range.start(),
                        range.end_inclusive()
                    )],
                }
            } else {
                SubordinateMappingDecision {
                    disposition: SubordinateMappingDisposition::Required,
                    range: Some(range),
                    source: Some(SubordinateRangeSource::ExactPolicy),
                    evidence: vec![format!(
                        "requested exact range {}-{} is free",
                        range.start(),
                        range.end_inclusive()
                    )],
                }
            }
        }
        SubordinateIdRequest::Allocate { window, count } => {
            match select_free_subordinate_range(authority, window, count) {
                Ok(range) => SubordinateMappingDecision {
                    disposition: SubordinateMappingDisposition::Required,
                    range: Some(range),
                    source: Some(SubordinateRangeSource::DeterministicAllocation),
                    evidence: vec![format!(
                        "selected lowest free range {}-{} in the reviewed allocation window",
                        range.start(),
                        range.end_inclusive()
                    )],
                },
                Err(error) => SubordinateMappingDecision {
                    disposition: SubordinateMappingDisposition::Exhausted,
                    range: None,
                    source: None,
                    evidence: vec![error.message().to_owned()],
                },
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubordinateIdKind {
    Uid,
    Gid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubordinatePlanDisposition {
    Satisfied,
    Required,
    NeedsInspection,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FreshAuthorityObservation {
    pub after_action_id: String,
    pub authority_path: String,
    pub required_owner: String,
    pub required_range: SubordinateIdRange,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinateMappingPlanItem {
    pub kind: SubordinateIdKind,
    pub disposition: SubordinatePlanDisposition,
    pub summary: String,
    pub evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_range: Option<SubordinateIdRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation: Option<PlannedMutation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<LaneCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fresh_observation: Option<FreshAuthorityObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum PodmanMigrationPlan {
    NotRequired,
    Required {
        mutation: PlannedMutation,
        command: LaneCommand,
        precondition_barriers: Vec<String>,
    },
    Blocked {
        evidence: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinateIdReconciliationPlan {
    pub subordinate_uids: SubordinateMappingPlanItem,
    pub subordinate_gids: SubordinateMappingPlanItem,
    pub podman_migration: PodmanMigrationPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubordinatePlanError {
    pub problems: Vec<String>,
}

impl From<LaneCommandError> for SubordinatePlanError {
    fn from(value: LaneCommandError) -> Self {
        Self {
            problems: value.problems,
        }
    }
}

impl fmt::Display for SubordinatePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "subordinate-ID reconciliation plan failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SubordinatePlanError {}

/// Build the exact-range subordinate-ID reconciliation slice used by read-only `host plan`.
///
/// Mapping mutations each carry their own mandatory fresh authority observation. A mapping change
/// also requires `podman system migrate` through the exact sealed runner-user lane. This function
/// only returns typed actions and never executes them.
///
/// # Errors
///
/// Returns an error only when a reviewed command cannot be represented safely.
pub fn build_exact_subordinate_id_plan(
    desired: &DesiredRunnerAccount,
    observations: &RunnerAccountObservations,
    runner_identity: Option<(u32, u32)>,
    subordinate_uid_path: &Path,
    subordinate_gid_path: &Path,
) -> Result<SubordinateIdReconciliationPlan, SubordinatePlanError> {
    let subordinate_uids = build_exact_mapping_item(
        SubordinateIdKind::Uid,
        observations.subordinate_uids.state(),
        desired,
        desired.subordinate_uids().start(),
        desired.subordinate_uids().count(),
        subordinate_uid_path,
    )?;
    let subordinate_gids = build_exact_mapping_item(
        SubordinateIdKind::Gid,
        observations.subordinate_gids.state(),
        desired,
        desired.subordinate_gids().start(),
        desired.subordinate_gids().count(),
        subordinate_gid_path,
    )?;

    let changed = [subordinate_uids.disposition, subordinate_gids.disposition]
        .contains(&SubordinatePlanDisposition::Required);
    let unresolved = [subordinate_uids.disposition, subordinate_gids.disposition]
        .iter()
        .any(|disposition| {
            matches!(
                disposition,
                SubordinatePlanDisposition::NeedsInspection | SubordinatePlanDisposition::Blocked
            )
        });
    let barriers = [
        subordinate_uids.fresh_observation.as_ref(),
        subordinate_gids.fresh_observation.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(|barrier| barrier.after_action_id.clone())
    .collect::<Vec<_>>();

    let podman_migration = if unresolved {
        PodmanMigrationPlan::Blocked {
            evidence: vec![
                "subordinate-ID authority must be matching or safely allocatable before Podman migration can be planned"
                    .to_owned(),
            ],
        }
    } else if !changed {
        PodmanMigrationPlan::NotRequired
    } else if let Some((uid, primary_gid)) = runner_identity {
        let runner =
            RunnerUserContext::new(desired.username().clone(), uid, primary_gid, desired.home())?;
        let mutation = PlannedMutation::new(
            "migrate-runner-podman-after-subordinate-id-change",
            ExecutionLane::RunnerUser,
            format!(
                "refresh rootless Podman namespace state for {}",
                desired.username().as_str()
            ),
            RollbackClass::Compensating,
            Preconditions::new([
                format!(
                    "fresh subordinate UID authority matches {}",
                    desired.username().as_str()
                ),
                format!(
                    "fresh subordinate GID authority matches {}",
                    desired.username().as_str()
                ),
                "runner-user identity and sealed runtime environment remain verified".to_owned(),
            ]),
        );
        let command = LaneCommand::runner_podman_migrate(&mutation, &runner)?;
        PodmanMigrationPlan::Required {
            mutation,
            command,
            precondition_barriers: barriers,
        }
    } else {
        PodmanMigrationPlan::Blocked {
            evidence: vec![
                "mapping changes require fresh exact runner identity before runner-user Podman migration"
                    .to_owned(),
            ],
        }
    };

    Ok(SubordinateIdReconciliationPlan {
        subordinate_uids,
        subordinate_gids,
        podman_migration,
    })
}

fn build_exact_mapping_item(
    kind: SubordinateIdKind,
    state: PreparationObservationState,
    desired: &DesiredRunnerAccount,
    start: u32,
    count: u32,
    authority_path: &Path,
) -> Result<SubordinateMappingPlanItem, SubordinatePlanError> {
    let range = SubordinateIdRange::new(start, count).map_err(|error| SubordinatePlanError {
        problems: vec![error.message().to_owned()],
    })?;
    let label = match kind {
        SubordinateIdKind::Uid => "UID",
        SubordinateIdKind::Gid => "GID",
    };
    let summary = format!(
        "reconcile subordinate {label} range {}-{} for {}",
        range.start(),
        range.end_inclusive(),
        desired.username().as_str()
    );
    let evidence = vec![format!(
        "complete {} authority was classified against exact range {}-{}",
        authority_path.display(),
        range.start(),
        range.end_inclusive()
    )];
    match state {
        PreparationObservationState::Matching => Ok(SubordinateMappingPlanItem {
            kind,
            disposition: SubordinatePlanDisposition::Satisfied,
            summary,
            evidence,
            selected_range: Some(range),
            mutation: None,
            command: None,
            fresh_observation: None,
        }),
        PreparationObservationState::Absent => {
            let action_id = match kind {
                SubordinateIdKind::Uid => "ensure-runner-subordinate-uids",
                SubordinateIdKind::Gid => "ensure-runner-subordinate-gids",
            };
            let mutation = PlannedMutation::new(
                action_id,
                ExecutionLane::Root,
                summary.clone(),
                RollbackClass::Compensating,
                Preconditions::new(evidence.clone()),
            );
            let command = match kind {
                SubordinateIdKind::Uid => LaneCommand::ensure_subordinate_uids(
                    &mutation,
                    desired.username(),
                    range.start(),
                    range.count(),
                )?,
                SubordinateIdKind::Gid => LaneCommand::ensure_subordinate_gids(
                    &mutation,
                    desired.username(),
                    range.start(),
                    range.count(),
                )?,
            };
            let fresh_observation = FreshAuthorityObservation {
                after_action_id: action_id.to_owned(),
                authority_path: authority_path.display().to_string(),
                required_owner: desired.username().as_str().to_owned(),
                required_range: range,
                summary: format!(
                    "re-read complete {} after {action_id}; command exit status alone cannot establish success",
                    authority_path.display()
                ),
            };
            Ok(SubordinateMappingPlanItem {
                kind,
                disposition: SubordinatePlanDisposition::Required,
                summary,
                evidence,
                selected_range: Some(range),
                mutation: Some(mutation),
                command: Some(command),
                fresh_observation: Some(fresh_observation),
            })
        }
        PreparationObservationState::Unknown => Ok(SubordinateMappingPlanItem {
            kind,
            disposition: SubordinatePlanDisposition::NeedsInspection,
            summary,
            evidence,
            selected_range: None,
            mutation: None,
            command: None,
            fresh_observation: None,
        }),
        PreparationObservationState::Conflicting => Ok(SubordinateMappingPlanItem {
            kind,
            disposition: SubordinatePlanDisposition::Blocked,
            summary,
            evidence,
            selected_range: None,
            mutation: None,
            command: None,
            fresh_observation: None,
        }),
    }
}

fn canonical_u32(value: &str) -> Option<u32> {
    let parsed = value.parse::<u32>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use crate::lane_command::LinuxAccountName;
    use crate::runner_account_plan::{
        DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
        PreparationObservationState, RunnerAccountObservations,
    };

    use super::{
        PodmanMigrationPlan, SubordinateAllocationErrorKind, SubordinateAllocationWindow,
        SubordinateAuthorityErrorKind, SubordinateIdOwner, SubordinateIdRange,
        SubordinateIdRequest, SubordinateMappingDisposition, SubordinatePlanDisposition,
        build_exact_subordinate_id_plan, parse_subordinate_authority,
        reconcile_subordinate_mapping, reconcile_subordinate_mapping_for_identity,
        select_free_subordinate_range,
    };

    fn account(value: &str) -> LinuxAccountName {
        LinuxAccountName::parse(value).expect("valid account")
    }

    fn owner(value: &str) -> SubordinateIdOwner {
        SubordinateIdOwner::parse(value).expect("valid owner")
    }

    fn range(start: u32, count: u32) -> SubordinateIdRange {
        SubordinateIdRange::new(start, count).expect("valid range")
    }

    fn desired() -> DesiredRunnerAccount {
        DesiredRunnerAccount::new(
            account("project-runner"),
            account("project-runner"),
            "/var/lib/project-runner",
            PlannedSubordinateRange::new(100_000, 65_536).expect("UID range"),
            PlannedSubordinateRange::new(200_000, 65_536).expect("GID range"),
        )
        .expect("desired runner")
    }

    fn observation(state: PreparationObservationState, label: &str) -> PreparationObservation {
        PreparationObservation::new(state, [format!("observed {label}")])
            .expect("bounded observation")
    }

    fn observations(
        uid: PreparationObservationState,
        gid: PreparationObservationState,
    ) -> RunnerAccountObservations {
        RunnerAccountObservations {
            group: observation(PreparationObservationState::Matching, "group"),
            user: observation(PreparationObservationState::Matching, "user"),
            home: observation(PreparationObservationState::Matching, "home"),
            subordinate_uids: observation(uid, "UID authority"),
            subordinate_gids: observation(gid, "GID authority"),
            linger: observation(PreparationObservationState::Matching, "linger"),
        }
    }

    #[test]
    fn empty_exact_multiple_and_adjacent_authorities_parse() {
        assert!(
            parse_subordinate_authority("")
                .expect("empty authority")
                .records()
                .is_empty()
        );
        let authority = parse_subordinate_authority(include_str!(
            "../tests/fixtures/subordinate_id/adjacent.txt"
        ))
        .expect("complete authority");
        assert_eq!(authority.records().len(), 3);
        assert_eq!(
            authority
                .range_for(&SubordinateIdOwner::parse("project-runner").expect("owner"))
                .expect("owned range"),
            SubordinateIdRange::new(400_000, 65_536).expect("range")
        );
    }

    #[test]
    fn malformed_duplicate_and_overlapping_rows_are_typed() {
        let malformed = parse_subordinate_authority(include_str!(
            "../tests/fixtures/subordinate_id/malformed.txt"
        ))
        .expect_err("malformed row");
        assert_eq!(
            malformed.kind(),
            SubordinateAuthorityErrorKind::MalformedRow
        );

        let duplicate = parse_subordinate_authority(include_str!(
            "../tests/fixtures/subordinate_id/duplicate-owner.txt"
        ))
        .expect_err("duplicate owner");
        assert_eq!(
            duplicate.kind(),
            SubordinateAuthorityErrorKind::DuplicateOwner
        );

        let overlap = parse_subordinate_authority(include_str!(
            "../tests/fixtures/subordinate_id/overlap.txt"
        ))
        .expect_err("overlap");
        assert_eq!(overlap.kind(), SubordinateAuthorityErrorKind::Overlap);
    }

    #[test]
    fn proven_numeric_uid_alias_can_match_exactly() {
        let authority =
            parse_subordinate_authority("1001:100000:65536\n").expect("numeric authority");
        let decision = reconcile_subordinate_mapping_for_identity(
            &authority,
            &owner("project-runner"),
            Some(1001),
            SubordinateIdRequest::Exact {
                range: range(100_000, 65_536),
            },
        );
        assert_eq!(
            decision.disposition,
            SubordinateMappingDisposition::Matching
        );
    }

    #[test]
    fn username_and_numeric_uid_aliases_are_conflicting_duplicates() {
        let authority =
            parse_subordinate_authority("project-runner:100000:65536\n1001:200000:65536\n")
                .expect("globally non-overlapping authority");
        let decision = reconcile_subordinate_mapping_for_identity(
            &authority,
            &owner("project-runner"),
            Some(1001),
            SubordinateIdRequest::Exact {
                range: range(100_000, 65_536),
            },
        );
        assert_eq!(
            decision.disposition,
            SubordinateMappingDisposition::Conflicting
        );
    }

    #[test]
    fn allocation_is_lowest_first_and_uses_the_supplied_window() {
        let authority = parse_subordinate_authority("alpha:900000:65536\nbeta:1031072:65536\n")
            .expect("authority");
        let selected = select_free_subordinate_range(
            &authority,
            SubordinateAllocationWindow::new(900_000, 300_000).expect("window"),
            65_536,
        )
        .expect("free range");
        assert_eq!(selected.start(), 965_536);
    }

    #[test]
    fn exhausted_window_is_typed() {
        let authority = parse_subordinate_authority(include_str!(
            "../tests/fixtures/subordinate_id/exhausted.txt"
        ))
        .expect("authority");
        let error = select_free_subordinate_range(
            &authority,
            SubordinateAllocationWindow::new(700_000, 131_072).expect("window"),
            65_536,
        )
        .expect_err("exhausted");
        assert_eq!(error.kind(), SubordinateAllocationErrorKind::Exhausted);
    }

    #[test]
    fn existing_valid_allocation_is_preserved_without_username_only_adoption() {
        let authority =
            parse_subordinate_authority("project-runner:500000:65536\n").expect("authority");
        let owner = SubordinateIdOwner::parse("project-runner").expect("owner");
        let preserved = reconcile_subordinate_mapping(
            &authority,
            &owner,
            SubordinateIdRequest::Allocate {
                window: SubordinateAllocationWindow::new(900_000, 200_000).expect("window"),
                count: 65_536,
            },
        );
        assert_eq!(
            preserved.disposition,
            SubordinateMappingDisposition::Matching
        );
        assert_eq!(preserved.range.expect("preserved range").start(), 500_000);

        let conflict = reconcile_subordinate_mapping(
            &authority,
            &owner,
            SubordinateIdRequest::Exact {
                range: SubordinateIdRange::new(600_000, 65_536).expect("range"),
            },
        );
        assert_eq!(
            conflict.disposition,
            SubordinateMappingDisposition::Conflicting
        );
    }

    #[test]
    fn mapping_changes_require_fresh_reads_and_runner_user_migration() {
        let plan = build_exact_subordinate_id_plan(
            &desired(),
            &observations(
                PreparationObservationState::Absent,
                PreparationObservationState::Absent,
            ),
            Some((1001, 1001)),
            std::path::Path::new("/etc/subuid"),
            std::path::Path::new("/etc/subgid"),
        )
        .expect("reconciliation plan");
        assert_eq!(
            plan.subordinate_uids.disposition,
            SubordinatePlanDisposition::Required
        );
        assert!(plan.subordinate_uids.fresh_observation.is_some());
        assert!(plan.subordinate_gids.fresh_observation.is_some());
        let PodmanMigrationPlan::Required {
            command,
            precondition_barriers,
            ..
        } = plan.podman_migration
        else {
            panic!("migration must be required");
        };
        let environment_program = command
            .runner_environment_program()
            .expect("reviewed environment program")
            .to_str()
            .expect("fixed reviewed path");
        assert_eq!(precondition_barriers.len(), 2);
        assert_eq!(
            command.spec().displayed_argv(),
            [
                "/usr/sbin/runuser",
                "--user",
                "project-runner",
                "--",
                environment_program,
                "--ignore-environment",
                "HOME=/var/lib/project-runner",
                "USER=project-runner",
                "LOGNAME=project-runner",
                "XDG_RUNTIME_DIR=/run/user/1001",
                "/usr/bin/podman",
                "system",
                "migrate",
            ]
        );
    }

    #[test]
    fn idempotent_mapping_plan_has_no_migration() {
        let plan = build_exact_subordinate_id_plan(
            &desired(),
            &observations(
                PreparationObservationState::Matching,
                PreparationObservationState::Matching,
            ),
            Some((1001, 1001)),
            std::path::Path::new("/etc/subuid"),
            std::path::Path::new("/etc/subgid"),
        )
        .expect("matching plan");
        assert!(matches!(
            plan.podman_migration,
            PodmanMigrationPlan::NotRequired
        ));
    }
}
