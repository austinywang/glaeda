//! Pure aggregate PID reservation policy for the personal-worker queue.
//!
//! This module defines a conservative queue-admission ceiling and checked arithmetic only. The
//! fixed values are policy, not an observation of a host or cgroup `pids.max`. Runtime/cgroup
//! enforcement remains a separate boundary.

use serde::Serialize;

pub const PERSONAL_WORKER_PID_CAPACITY_POLICY_SCHEMA_VERSION: u8 = 1;
pub const PERSONAL_WORKER_TOTAL_PID_CAPACITY: u32 = 4_096;
pub const PERSONAL_WORKER_RESERVED_RUNTIME_PIDS: u32 = 1_024;
pub const PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY: u32 =
    PERSONAL_WORKER_TOTAL_PID_CAPACITY - PERSONAL_WORKER_RESERVED_RUNTIME_PIDS;

/// Fixed queue-level PID capacity policy.
///
/// The 1,024-PID reserve is intentionally unavailable to queued workloads so guest/runtime control
/// and cleanup retain headroom even when queue reservations fill the schedulable envelope. The
/// policy is independent from the queue's active-job cardinality and from any one verification
/// profile's per-job PID request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerPidCapacityPolicy {
    schema_version: u8,
    total_pid_capacity: u32,
    reserved_runtime_pids: u32,
    schedulable_pid_capacity: u32,
}

impl PersonalWorkerPidCapacityPolicy {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn total_pid_capacity(self) -> u32 {
        self.total_pid_capacity
    }

    #[must_use]
    pub const fn reserved_runtime_pids(self) -> u32 {
        self.reserved_runtime_pids
    }

    #[must_use]
    pub const fn schedulable_pid_capacity(self) -> u32 {
        self.schedulable_pid_capacity
    }
}

/// Return the reviewed first personal-worker PID admission policy.
#[must_use]
pub const fn personal_worker_pid_capacity_policy() -> PersonalWorkerPidCapacityPolicy {
    PersonalWorkerPidCapacityPolicy {
        schema_version: PERSONAL_WORKER_PID_CAPACITY_POLICY_SCHEMA_VERSION,
        total_pid_capacity: PERSONAL_WORKER_TOTAL_PID_CAPACITY,
        reserved_runtime_pids: PERSONAL_WORKER_RESERVED_RUNTIME_PIDS,
        schedulable_pid_capacity: PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY,
    }
}

/// Bounded reasons an aggregate PID reservation is refused before queue selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerPidAdmissionRefusalReason {
    ArithmeticOverflow,
    ExistingReservationsOvercommitted,
    InsufficientSchedulablePids,
}

/// Accepted aggregate PID reservation arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerPidAdmission {
    schema_version: u8,
    existing_reserved_pids: u32,
    candidate_reserved_pids: u32,
    projected_reserved_pids: u32,
    schedulable_pid_capacity: u32,
}

impl PersonalWorkerPidAdmission {
    #[must_use]
    pub const fn existing_reserved_pids(self) -> u32 {
        self.existing_reserved_pids
    }

    #[must_use]
    pub const fn candidate_reserved_pids(self) -> u32 {
        self.candidate_reserved_pids
    }

    #[must_use]
    pub const fn projected_reserved_pids(self) -> u32 {
        self.projected_reserved_pids
    }

    #[must_use]
    pub const fn schedulable_pid_capacity(self) -> u32 {
        self.schedulable_pid_capacity
    }
}

/// Check one candidate PID reservation against exact already-active reservations.
///
/// Reservation values are expected to come from the queue's already-validated
/// `ExecutionResourceLimits`; this helper owns only aggregate arithmetic and the queue-level ceiling.
/// Existing overcommit fails before candidate capacity is considered. Every addition is checked, so
/// saturation can never hide an invalid reservation set.
///
/// # Errors
///
/// Returns a bounded refusal when existing arithmetic overflows, existing reservations already
/// exceed the schedulable ceiling, candidate arithmetic overflows, or the projected reservation
/// exceeds the schedulable ceiling.
pub fn admit_personal_worker_pid_reservation(
    existing_reservations: &[u32],
    candidate_reserved_pids: u32,
) -> Result<PersonalWorkerPidAdmission, PersonalWorkerPidAdmissionRefusalReason> {
    let policy = personal_worker_pid_capacity_policy();
    let mut existing_reserved_pids = 0_u32;
    for reservation in existing_reservations {
        existing_reserved_pids = existing_reserved_pids
            .checked_add(*reservation)
            .ok_or(PersonalWorkerPidAdmissionRefusalReason::ArithmeticOverflow)?;
    }
    if existing_reserved_pids > policy.schedulable_pid_capacity() {
        return Err(PersonalWorkerPidAdmissionRefusalReason::ExistingReservationsOvercommitted);
    }

    let projected_reserved_pids = existing_reserved_pids
        .checked_add(candidate_reserved_pids)
        .ok_or(PersonalWorkerPidAdmissionRefusalReason::ArithmeticOverflow)?;
    if projected_reserved_pids > policy.schedulable_pid_capacity() {
        return Err(PersonalWorkerPidAdmissionRefusalReason::InsufficientSchedulablePids);
    }

    Ok(PersonalWorkerPidAdmission {
        schema_version: PERSONAL_WORKER_PID_CAPACITY_POLICY_SCHEMA_VERSION,
        existing_reserved_pids,
        candidate_reserved_pids,
        projected_reserved_pids,
        schedulable_pid_capacity: policy.schedulable_pid_capacity(),
    })
}
