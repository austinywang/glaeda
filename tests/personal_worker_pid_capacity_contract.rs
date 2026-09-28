#[path = "../src/personal_worker_pid_capacity.rs"]
mod personal_worker_pid_capacity;

use personal_worker_pid_capacity::{
    PERSONAL_WORKER_PID_CAPACITY_POLICY_SCHEMA_VERSION, PERSONAL_WORKER_RESERVED_RUNTIME_PIDS,
    PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY, PERSONAL_WORKER_TOTAL_PID_CAPACITY,
    PersonalWorkerPidAdmissionRefusalReason, admit_personal_worker_pid_reservation,
    personal_worker_pid_capacity_policy,
};

#[test]
fn current_pid_policy_reserves_control_and_cleanup_headroom() {
    let policy = personal_worker_pid_capacity_policy();

    assert_eq!(policy.schema_version(), 1);
    assert_eq!(
        policy.schema_version(),
        PERSONAL_WORKER_PID_CAPACITY_POLICY_SCHEMA_VERSION
    );
    assert_eq!(policy.total_pid_capacity(), 4_096);
    assert_eq!(
        policy.total_pid_capacity(),
        PERSONAL_WORKER_TOTAL_PID_CAPACITY
    );
    assert_eq!(policy.reserved_runtime_pids(), 1_024);
    assert_eq!(
        policy.reserved_runtime_pids(),
        PERSONAL_WORKER_RESERVED_RUNTIME_PIDS
    );
    assert_eq!(policy.schedulable_pid_capacity(), 3_072);
    assert_eq!(
        policy.schedulable_pid_capacity(),
        PERSONAL_WORKER_SCHEDULABLE_PID_CAPACITY
    );
    assert_eq!(
        policy.total_pid_capacity(),
        policy.reserved_runtime_pids() + policy.schedulable_pid_capacity()
    );
    assert!(policy.reserved_runtime_pids() > 0);
}

#[test]
fn current_required_verification_envelope_fits_with_schedulable_headroom() {
    let admission = admit_personal_worker_pid_reservation(&[], 2_048)
        .expect("current required verification PID envelope must fit the queue policy");

    assert_eq!(admission.existing_reserved_pids(), 0);
    assert_eq!(admission.candidate_reserved_pids(), 2_048);
    assert_eq!(admission.projected_reserved_pids(), 2_048);
    assert_eq!(admission.schedulable_pid_capacity(), 3_072);
    assert_eq!(
        admission.schedulable_pid_capacity() - admission.projected_reserved_pids(),
        1_024
    );
}

#[test]
fn exact_schedulable_pid_boundary_is_accepted() {
    let admission = admit_personal_worker_pid_reservation(&[2_048], 1_024)
        .expect("exact aggregate PID boundary must be accepted");

    assert_eq!(admission.existing_reserved_pids(), 2_048);
    assert_eq!(admission.candidate_reserved_pids(), 1_024);
    assert_eq!(admission.projected_reserved_pids(), 3_072);
}

#[test]
fn candidate_above_remaining_pid_capacity_is_refused() {
    let refusal = admit_personal_worker_pid_reservation(&[2_048], 1_025)
        .expect_err("candidate must not exceed remaining schedulable PID capacity");

    assert_eq!(
        refusal,
        PersonalWorkerPidAdmissionRefusalReason::InsufficientSchedulablePids
    );
}

#[test]
fn already_overcommitted_pid_reservations_fail_before_candidate_admission() {
    let refusal = admit_personal_worker_pid_reservation(&[2_048, 1_025], 1)
        .expect_err("an already-overcommitted active set must fail closed");

    assert_eq!(
        refusal,
        PersonalWorkerPidAdmissionRefusalReason::ExistingReservationsOvercommitted
    );
}

#[test]
fn checked_pid_arithmetic_refuses_overflow_in_existing_or_projected_totals() {
    let existing_overflow = admit_personal_worker_pid_reservation(&[u32::MAX, 1], 1)
        .expect_err("existing PID aggregation must use checked arithmetic");
    assert_eq!(
        existing_overflow,
        PersonalWorkerPidAdmissionRefusalReason::ArithmeticOverflow
    );

    let projected_overflow = admit_personal_worker_pid_reservation(&[1], u32::MAX)
        .expect_err("candidate projection must use checked arithmetic");
    assert_eq!(
        projected_overflow,
        PersonalWorkerPidAdmissionRefusalReason::ArithmeticOverflow
    );
}

#[test]
fn public_reports_are_bounded_resource_counts_only() {
    let policy_json =
        serde_json::to_string(&personal_worker_pid_capacity_policy()).expect("policy serializes");
    let admission = admit_personal_worker_pid_reservation(&[512], 512).expect("PID admission");
    let admission_json = serde_json::to_string(&admission).expect("admission serializes");
    let refusal_json = serde_json::to_string(
        &PersonalWorkerPidAdmissionRefusalReason::InsufficientSchedulablePids,
    )
    .expect("refusal serializes");

    assert!(policy_json.contains("\"total_pid_capacity\":4096"));
    assert!(policy_json.contains("\"reserved_runtime_pids\":1024"));
    assert!(policy_json.contains("\"schedulable_pid_capacity\":3072"));
    assert!(admission_json.contains("\"projected_reserved_pids\":1024"));
    assert_eq!(refusal_json, "\"insufficient_schedulable_pids\"");

    for output in [&policy_json, &admission_json, &refusal_json] {
        assert!(!output.contains('/'));
        assert!(!output.contains("argv"));
        assert!(!output.contains("environment"));
        assert!(!output.contains("process"));
    }
}
