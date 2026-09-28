//! Bounded reclamation planning for one checkout-local Cargo target.
//!
//! Authority here comes from reconstructibility, never from proven idleness. Holder observation is
//! positive-only: no count it reports can prove a target is unused, so this module never claims
//! that. A Cargo target rebuilds from the checkout and its lockfile, so reclaiming one that turns
//! out to be in use costs a failed build and a rebuild -- time, never truth. Positive holder
//! evidence is still a veto, because paying a rebuild for nothing has no upside.
//!
//! Planning performs no mutation and emits no paths. It consumes observations the caller took and
//! returns one decision with every veto that applied, so a refusal explains itself completely
//! instead of surfacing only the first reason.

use std::ffi::{CStr, CString};
use std::fmt;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::Path;

use rustix::fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::cargo_target_holder_observation::observe_cargo_target_holders;
use crate::cargo_target_holder_observation::{
    CargoTargetHolderDisposition, CargoTargetHolderObservation, CargoTargetHolderState,
};
use crate::cargo_target_observation::{
    CargoTargetHardlinkCoverage, CargoTargetObservation, CargoTargetObservationErrorKind,
    CargoTargetState, RustcInfoObservation, cargo_target_materialization_id, observe_cargo_target,
};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub const CARGO_TARGET_RECLAIM_SCHEMA_VERSION: u8 = 1;

/// Smallest idle window a policy may require.
///
/// A zero window would let a target be reclaimed in the same second a build wrote to it, which
/// makes the common accident -- reclaiming state a live build is still producing -- reachable
/// through ordinary configuration rather than through an explicit override.
pub const MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS: i64 = 3_600;

/// What the positive-only holder scan actually managed to look at.
///
/// Incomplete process coverage is the normal case, not a fault: on an ordinary multi-user Linux
/// host most `/proc` entries belong to other users and cannot be read, and the process table
/// changes while it is being walked. Measured on one developer host, 370 of 501 process entries
/// were unreadable and the rescan never matched. Treating that as a veto would make reclaim
/// permanently ineligible everywhere, which is why only a positive holder observation refuses.
/// These counts travel with the decision so an operator can see exactly how much the absence of
/// observed holders is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CargoTargetHolderEvidence {
    process_entries_started: u64,
    process_entries_completed: u64,
    process_entries_incomplete: u64,
    process_table_rescan_equal: bool,
    universal_absence_proven: bool,
}

impl CargoTargetHolderEvidence {
    #[must_use]
    pub const fn process_entries_incomplete(self) -> u64 {
        self.process_entries_incomplete
    }

    #[must_use]
    pub const fn universal_absence_proven(self) -> bool {
        self.universal_absence_proven
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetReclaimAuthority {
    /// The target is reconstructible from the checkout; losing it costs time, never truth.
    ReconstructibleFromCheckout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetReclaimVeto {
    /// No `target` directory was observed, so there is nothing to reclaim.
    TargetAbsent,
    /// The target is owned by a different user than the checkout that would rebuild it.
    TargetOwnerDiffersFromCheckout,
    /// At least one entry inside the target belongs to another user.
    ForeignEntriesPresent,
    /// No usable `.rustc_info.json`, so nothing establishes that Cargo produced this directory.
    NotCargoProduced,
    /// The newest entry is inside the policy's idle window.
    RecentlyModified,
    /// A process or mount namespace was observed referencing the target.
    HoldersObserved,
    /// Holder evidence describes a different physical target than the cost observation.
    HolderIdentityMismatch,
}

impl CargoTargetReclaimVeto {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TargetAbsent => "target_absent",
            Self::TargetOwnerDiffersFromCheckout => "target_owner_differs_from_checkout",
            Self::ForeignEntriesPresent => "foreign_entries_present",
            Self::NotCargoProduced => "not_cargo_produced",
            Self::RecentlyModified => "recently_modified",
            Self::HoldersObserved => "holders_observed",
            Self::HolderIdentityMismatch => "holder_identity_mismatch",
        }
    }
}

impl fmt::Display for CargoTargetReclaimVeto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CargoTargetReclaimPolicy {
    minimum_idle_seconds: i64,
}

impl CargoTargetReclaimPolicy {
    /// Build a policy from an explicit idle window.
    ///
    /// # Errors
    ///
    /// Refuses a window below [`MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS`].
    pub const fn new(minimum_idle_seconds: i64) -> Result<Self, CargoTargetReclaimError> {
        if minimum_idle_seconds < MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS {
            return Err(idle_window_too_small());
        }
        Ok(Self {
            minimum_idle_seconds,
        })
    }

    #[must_use]
    pub const fn minimum_idle_seconds(self) -> i64 {
        self.minimum_idle_seconds
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CargoTargetReclaimDecision {
    Refused {
        vetoes: Vec<CargoTargetReclaimVeto>,
    },
    Eligible {
        authority: CargoTargetReclaimAuthority,
        allocated_bytes: u64,
        logical_bytes: u64,
        entry_count: u64,
        idle_seconds: i64,
        holder_evidence: CargoTargetHolderEvidence,
        /// Hardlinks reach outside the tree, so unlinking frees fewer bytes than observed.
        /// Cargo hardlinks its own binaries, so links are ordinary here; an external one costs
        /// accuracy in the reported figure and nothing else, because unlinking one link never
        /// destroys the other.
        released_bytes_are_upper_bound: bool,
    },
}

impl CargoTargetReclaimDecision {
    #[must_use]
    pub const fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible { .. })
    }

    #[must_use]
    pub fn vetoes(&self) -> &[CargoTargetReclaimVeto] {
        match self {
            Self::Refused { vetoes } => vetoes,
            Self::Eligible { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetReclaimPlan {
    schema_version: u8,
    decision: CargoTargetReclaimDecision,
    minimum_idle_seconds: i64,
}

impl CargoTargetReclaimPlan {
    #[must_use]
    pub const fn decision(&self) -> &CargoTargetReclaimDecision {
        &self.decision
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }
}

/// Decide whether one observed Cargo target may be reclaimed.
///
/// Every applicable veto is collected, so one refusal reports all of its reasons. `now_seconds` is
/// supplied rather than read, which keeps the decision a pure function of its observations.
///
/// # Errors
///
/// Refuses an observation whose newest entry is in the future relative to `now_seconds`, or whose
/// idle arithmetic would overflow. A future timestamp means the clock, the filesystem, or the
/// observation disagree, and no reclaim decision should be made from disagreeing evidence.
pub fn plan_cargo_target_reclaim(
    observation: &CargoTargetObservation,
    holders: &CargoTargetHolderObservation,
    policy: CargoTargetReclaimPolicy,
    now_seconds: i64,
) -> Result<CargoTargetReclaimPlan, CargoTargetReclaimError> {
    let mut vetoes = Vec::new();
    let mut evidence: Option<CargoTargetHolderEvidence> = None;

    let CargoTargetState::Present {
        target_id,
        entry_count,
        logical_bytes,
        allocated_bytes,
        latest_modified,
        target_owner_matches_checkout,
        all_entries_match_target_owner,
        hardlink_coverage,
        rustc_info,
        ..
    } = observation.state()
    else {
        return Ok(CargoTargetReclaimPlan {
            schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
            decision: CargoTargetReclaimDecision::Refused {
                vetoes: vec![CargoTargetReclaimVeto::TargetAbsent],
            },
            minimum_idle_seconds: policy.minimum_idle_seconds,
        });
    };

    if !target_owner_matches_checkout {
        vetoes.push(CargoTargetReclaimVeto::TargetOwnerDiffersFromCheckout);
    }
    if !all_entries_match_target_owner {
        vetoes.push(CargoTargetReclaimVeto::ForeignEntriesPresent);
    }
    if !matches!(rustc_info, RustcInfoObservation::Observed { .. }) {
        vetoes.push(CargoTargetReclaimVeto::NotCargoProduced);
    }

    let idle_seconds = now_seconds
        .checked_sub(latest_modified.seconds())
        .ok_or_else(idle_overflow)?;
    if idle_seconds < 0 {
        return Err(future_timestamp());
    }
    if idle_seconds < policy.minimum_idle_seconds {
        vetoes.push(CargoTargetReclaimVeto::RecentlyModified);
    }

    match holders.state() {
        CargoTargetHolderState::Absent => {
            vetoes.push(CargoTargetReclaimVeto::HolderIdentityMismatch);
        }
        CargoTargetHolderState::Present {
            target_id: holder_target_id,
            disposition,
            coverage,
            ..
        } => {
            if holder_target_id != target_id {
                vetoes.push(CargoTargetReclaimVeto::HolderIdentityMismatch);
            }
            if *disposition == CargoTargetHolderDisposition::HoldersObserved {
                vetoes.push(CargoTargetReclaimVeto::HoldersObserved);
            }
            evidence = Some(CargoTargetHolderEvidence {
                process_entries_started: coverage.process_entries_started(),
                process_entries_completed: coverage.process_entries_completed(),
                process_entries_incomplete: coverage.process_entries_incomplete(),
                process_table_rescan_equal: coverage.process_table_rescan_equal(),
                universal_absence_proven: coverage.universal_absence_proven(),
            });
        }
    }

    vetoes.sort_unstable();
    vetoes.dedup();

    let decision = if vetoes.is_empty() {
        CargoTargetReclaimDecision::Eligible {
            authority: CargoTargetReclaimAuthority::ReconstructibleFromCheckout,
            allocated_bytes: *allocated_bytes,
            logical_bytes: *logical_bytes,
            entry_count: *entry_count,
            idle_seconds,
            holder_evidence: evidence.ok_or_else(missing_holder_evidence)?,
            released_bytes_are_upper_bound: *hardlink_coverage
                == CargoTargetHardlinkCoverage::ExternalLinksPresent,
        }
    } else {
        CargoTargetReclaimDecision::Refused { vetoes }
    };

    Ok(CargoTargetReclaimPlan {
        schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
        decision,
        minimum_idle_seconds: policy.minimum_idle_seconds,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetReclaimErrorKind {
    InvalidPolicy,
    DisagreeingEvidence,
    Overflow,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetReclaimError {
    kind: CargoTargetReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl CargoTargetReclaimError {
    #[must_use]
    pub const fn kind(&self) -> CargoTargetReclaimErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for CargoTargetReclaimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for CargoTargetReclaimError {}

const fn error(
    kind: CargoTargetReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
) -> CargoTargetReclaimError {
    CargoTargetReclaimError {
        kind,
        code,
        problem,
    }
}

const fn idle_window_too_small() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::InvalidPolicy,
        "idle_window_too_small",
        "the reclaim idle window is below the reviewed minimum",
    )
}

const fn future_timestamp() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::DisagreeingEvidence,
        "future_timestamp",
        "the observed target is newer than the supplied clock reading",
    )
}

const fn missing_holder_evidence() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::DisagreeingEvidence,
        "missing_holder_evidence",
        "the target was observed but holder evidence was absent",
    )
}

const fn idle_overflow() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::Overflow,
        "idle_overflow",
        "the idle window could not be computed without overflow",
    )
}

// --- Execution -------------------------------------------------------------
//
// Retire by rename, then delete descriptor-relative. The rename makes the target vanish from its
// name in one atomic step, so a build that starts mid-delete creates a fresh `target` instead of
// racing a half-deleted one. Everything after the rename is bounded and resumable: an interrupted
// pass leaves a retiring directory that the next pass finishes, which is why no recovery journal
// is needed to avoid leaking bytes.
//
// Deletion is irreversible. Its accurate compensation is cold reconstruction -- Cargo rebuilds the
// tree from the checkout and its lockfile -- not rollback, and nothing here pretends otherwise.

/// Entries one pass may unlink. Bounds work per invocation; leftovers resume on the next pass.
pub const MAX_CARGO_TARGET_RECLAIM_ENTRIES: u64 = 4_000_000;

const RECLAIMING_PREFIX: &[u8] = b".glaeda-reclaiming-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CargoTargetReclaimOutcome {
    Refused {
        vetoes: Vec<CargoTargetReclaimVeto>,
    },
    Reclaimed {
        entries_removed: u64,
        released_bytes: u64,
        released_bytes_are_upper_bound: bool,
    },
    /// The entry budget ran out. The retiring directory remains and the next pass resumes it.
    Incomplete {
        entries_removed: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetReclaimReceipt {
    schema_version: u8,
    mutation_performed: bool,
    resumed_entries_removed: u64,
    outcome: CargoTargetReclaimOutcome,
}

impl CargoTargetReclaimReceipt {
    #[must_use]
    pub const fn outcome(&self) -> &CargoTargetReclaimOutcome {
        &self.outcome
    }

    #[must_use]
    pub const fn mutation_performed(&self) -> bool {
        self.mutation_performed
    }

    #[must_use]
    pub const fn resumed_entries_removed(&self) -> u64 {
        self.resumed_entries_removed
    }
}

struct DeleteBudget {
    remaining: u64,
    removed: u64,
}

impl DeleteBudget {
    const fn new(limit: u64) -> Self {
        Self {
            remaining: limit,
            removed: 0,
        }
    }

    fn spend(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        self.removed += 1;
        true
    }
}

/// Reclaim one checkout-local Cargo target, resuming any interrupted earlier pass first.
///
/// # Errors
///
/// Refuses a relative checkout path, an unreadable or replaced target, a cross-device or
/// non-directory target, and any entry whose owner or device changes mid-delete. Errors never
/// contain the supplied path or a child name.
pub fn reclaim_cargo_target(
    checkout: &Path,
    policy: CargoTargetReclaimPolicy,
    now_seconds: i64,
) -> Result<CargoTargetReclaimReceipt, CargoTargetReclaimError> {
    if !checkout.is_absolute() {
        return Err(checkout_unavailable());
    }
    let checkout_fd = open_directory(checkout)?;
    let mut budget = DeleteBudget::new(MAX_CARGO_TARGET_RECLAIM_ENTRIES);

    // Finish anything a previous pass left behind before measuring, so a resumed tree is not
    // observed as if it were a live target.
    let resume_complete = resume_retiring_directories(&checkout_fd, &mut budget)?;
    let resumed = budget.removed;
    if !resume_complete {
        // The budget is shared with the primary phase. Retiring the live target now would rename it
        // away with nothing left to delete it with, so stop and report the partial pass instead.
        return Ok(CargoTargetReclaimReceipt {
            schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
            mutation_performed: resumed > 0,
            resumed_entries_removed: resumed,
            outcome: CargoTargetReclaimOutcome::Incomplete { entries_removed: 0 },
        });
    }

    let observation = observe_cargo_target(checkout).map_err(|error| {
        // A tree past the observer's bound cannot be planned, even though the delete below has no
        // depth ceiling of its own. Name that distinctly: "unreadable" would send someone looking
        // for a permissions fault that is not there.
        if error.kind() == CargoTargetObservationErrorKind::TooLarge {
            target_exceeds_observation_bound()
        } else {
            target_unreadable()
        }
    })?;
    let holders = observe_cargo_target_holders(checkout).map_err(|_| target_unreadable())?;
    let plan = plan_cargo_target_reclaim(&observation, &holders, policy, now_seconds)?;

    let CargoTargetReclaimDecision::Eligible {
        allocated_bytes,
        released_bytes_are_upper_bound,
        ..
    } = plan.decision()
    else {
        return Ok(CargoTargetReclaimReceipt {
            schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
            mutation_performed: resumed > 0,
            resumed_entries_removed: resumed,
            outcome: CargoTargetReclaimOutcome::Refused {
                vetoes: plan.decision().vetoes().to_vec(),
            },
        });
    };
    let released_bytes = *allocated_bytes;
    let upper_bound = *released_bytes_are_upper_bound;

    // The plan was made about one materialization of `<checkout>/target`. Carry its identity down
    // to the rename so the two name resolutions cannot end up being about different directories.
    let CargoTargetState::Present {
        target_id: planned_target_id,
        ..
    } = observation.state()
    else {
        return Err(target_changed());
    };

    let target_name = c"target";
    let before = rustix_fs::statat(checkout_fd.as_fd(), target_name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| target_unreadable())?;
    if !FileType::from_raw_mode(before.st_mode).is_dir() {
        return Err(target_unsafe_shape());
    }
    // Same name, second resolution. Everything the plan cleared - owner, holders, idleness,
    // reconstructibility - is evidence about the measured inode, so a directory that is not that
    // inode has been vouched for by nothing and must not be renamed away and deleted.
    let materialization =
        cargo_target_materialization_id(&before).map_err(|_| target_unreadable())?;
    if materialization != *planned_target_id {
        return Err(target_changed());
    }

    let retiring = retiring_name(before.st_ino);
    let retiring_name = CString::new(retiring).map_err(|_| target_unsafe_shape())?;
    rustix_fs::renameat(
        checkout_fd.as_fd(),
        target_name,
        checkout_fd.as_fd(),
        retiring_name.as_c_str(),
    )
    .map_err(|_| retire_failed())?;

    // The rename moved an inode, not a name. Prove the directory now under the retiring name is
    // the one that was measured, so a concurrent replacement cannot redirect the delete.
    let retiring_fd = open_child_directory(&checkout_fd, retiring_name.as_c_str())?;
    let pinned = rustix_fs::fstat(retiring_fd.as_fd()).map_err(|_| target_unreadable())?;
    if pinned.st_ino != before.st_ino || pinned.st_dev != before.st_dev {
        return Err(target_changed());
    }

    let complete = delete_directory_contents(&retiring_fd, pinned.st_dev, &mut budget)?;
    drop(retiring_fd);

    if !complete {
        return Ok(CargoTargetReclaimReceipt {
            schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
            mutation_performed: true,
            resumed_entries_removed: resumed,
            outcome: CargoTargetReclaimOutcome::Incomplete {
                entries_removed: budget.removed - resumed,
            },
        });
    }
    rustix_fs::unlinkat(
        checkout_fd.as_fd(),
        retiring_name.as_c_str(),
        AtFlags::REMOVEDIR,
    )
    .map_err(|_| retire_failed())?;

    Ok(CargoTargetReclaimReceipt {
        schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
        mutation_performed: true,
        resumed_entries_removed: resumed,
        outcome: CargoTargetReclaimOutcome::Reclaimed {
            entries_removed: budget.removed - resumed,
            released_bytes,
            released_bytes_are_upper_bound: upper_bound,
        },
    })
}

fn retiring_name(inode: u64) -> Vec<u8> {
    let mut name = RECLAIMING_PREFIX.to_vec();
    name.extend_from_slice(inode.to_string().as_bytes());
    name
}

/// Finish earlier passes' retiring directories. Returns false when the budget ran out first.
fn resume_retiring_directories(
    checkout_fd: &OwnedFd,
    budget: &mut DeleteBudget,
) -> Result<bool, CargoTargetReclaimError> {
    let mut pending: Vec<CString> = Vec::new();
    {
        let mut entries = Dir::read_from(checkout_fd).map_err(|_| target_unreadable())?;
        for entry in &mut entries {
            let entry = entry.map_err(|_| target_unreadable())?;
            let name = entry.file_name();
            if !name.to_bytes().starts_with(RECLAIMING_PREFIX) {
                continue;
            }
            pending.push(name.to_owned());
        }
    }
    for name in pending {
        let stat = match rustix_fs::statat(
            checkout_fd.as_fd(),
            name.as_c_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => continue,
            Err(_) => return Err(target_unreadable()),
        };
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            continue;
        }
        // The name is not the authority: this pass wrote it, and anyone with write access to the
        // checkout could write one too. The inode it encodes survives the rename that created it,
        // so a directory whose own inode does not reproduce its name was never retired by us.
        // Leave it exactly where it is rather than recursively deleting a stranger's tree.
        if retiring_name(stat.st_ino) != name.to_bytes() {
            continue;
        }
        let fd = open_child_directory(checkout_fd, name.as_c_str())?;
        let complete = delete_directory_contents(&fd, stat.st_dev, budget)?;
        drop(fd);
        if !complete {
            return Ok(false);
        }
        rustix_fs::unlinkat(checkout_fd.as_fd(), name.as_c_str(), AtFlags::REMOVEDIR)
            .map_err(|_| retire_failed())?;
    }
    Ok(true)
}

/// Delete one directory's contents depth-first, holding a descriptor per level.
///
/// There is no depth ceiling: a valid tree deeper than an arbitrary limit would otherwise be
/// unreclaimable on every future pass. Work is bounded by the entry budget instead, which always
/// makes progress.
fn delete_directory_contents(
    root: &OwnedFd,
    expected_device: u64,
    budget: &mut DeleteBudget,
) -> Result<bool, CargoTargetReclaimError> {
    let uid = rustix::process::getuid().as_raw();
    let mut stack: Vec<(OwnedFd, Option<CString>)> = vec![(
        rustix_fs::openat(root.as_fd(), c".", DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| target_unreadable())?,
        None,
    )];

    while let Some((fd, _)) = stack.last() {
        let mut descend: Option<(OwnedFd, CString)> = None;
        let mut emptied = true;
        {
            let mut entries = Dir::read_from(fd).map_err(|_| target_unreadable())?;
            for entry in &mut entries {
                let entry = entry.map_err(|_| target_unreadable())?;
                let name = entry.file_name();
                let bytes = name.to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                let stat = match rustix_fs::statat(fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(stat) => stat,
                    Err(Errno::NOENT) => continue,
                    Err(_) => return Err(target_unreadable()),
                };
                if stat.st_uid != uid || stat.st_dev != expected_device {
                    return Err(foreign_entry());
                }
                let kind = FileType::from_raw_mode(stat.st_mode);
                if kind.is_dir() {
                    let child = rustix_fs::openat(fd.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
                        .map_err(|_| target_unreadable())?;
                    let pinned =
                        rustix_fs::fstat(child.as_fd()).map_err(|_| target_unreadable())?;
                    if pinned.st_ino != stat.st_ino
                        || pinned.st_dev != expected_device
                        || pinned.st_uid != uid
                    {
                        return Err(target_changed());
                    }
                    descend = Some((child, name.to_owned()));
                    emptied = false;
                    break;
                }
                if !budget.spend() {
                    return Ok(false);
                }
                match rustix_fs::unlinkat(fd.as_fd(), name, AtFlags::empty()) {
                    Ok(()) | Err(Errno::NOENT) => {}
                    Err(_) => return Err(delete_failed()),
                }
            }
        }
        if let Some((child, name)) = descend {
            stack.push((child, Some(name)));
            continue;
        }
        if !emptied {
            continue;
        }
        let (_, name) = stack.pop().expect("stack is non-empty inside the loop");
        let Some(name) = name else {
            return Ok(true);
        };
        let Some((parent, _)) = stack.last() else {
            return Err(target_changed());
        };
        if !budget.spend() {
            return Ok(false);
        }
        match rustix_fs::unlinkat(parent.as_fd(), name.as_c_str(), AtFlags::REMOVEDIR) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(_) => return Err(delete_failed()),
        }
    }
    Ok(true)
}

fn open_directory(path: &Path) -> Result<OwnedFd, CargoTargetReclaimError> {
    rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| checkout_unavailable())
}

fn open_child_directory(parent: &OwnedFd, name: &CStr) -> Result<OwnedFd, CargoTargetReclaimError> {
    rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| target_unreadable())
}

const fn checkout_unavailable() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::Unavailable,
        "checkout_unavailable",
        "the checkout root could not be opened as a directory",
    )
}

const fn target_unreadable() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::Unavailable,
        "target_unreadable",
        "the target could not be read",
    )
}

const fn target_exceeds_observation_bound() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::Unavailable,
        "target_exceeds_observation_bound",
        "the target is larger or deeper than the observer's reviewed bound",
    )
}

const fn target_unsafe_shape() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::DisagreeingEvidence,
        "target_unsafe_shape",
        "the target is not a plain directory",
    )
}

const fn target_changed() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::DisagreeingEvidence,
        "target_changed",
        "the target changed identity while it was being reclaimed",
    )
}

const fn foreign_entry() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::DisagreeingEvidence,
        "foreign_entry",
        "an entry inside the target is owned by another user or filesystem",
    )
}

const fn retire_failed() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::Unavailable,
        "retire_failed",
        "the target could not be retired",
    )
}

const fn delete_failed() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::Unavailable,
        "delete_failed",
        "an entry inside the retired target could not be removed",
    )
}
