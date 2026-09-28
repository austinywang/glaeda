#![cfg(target_os = "linux")]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use glaeda::durable_journal::StateStoreJournalCheckpoint;
use glaeda::host_preparation_execution::{
    HostPreparationExecutionDisposition, execute_confirmed_host_preparation,
};
use glaeda::host_preparation_plan::HostPreparationResult;
use glaeda::journal::ActionOutcome;
use glaeda::lane_command::LaneCommandKind;
use glaeda::linux_state::LinuxStateRoot;
use glaeda::process::ProcessExecutor;
use glaeda::runner_account_observation::{RunnerAccountObservationPaths, observe_runner_account};
use glaeda::runner_user_observation::{
    FreshRunnerUserEvidenceErrorKind, observe_verified_runner_user,
};

#[path = "host_prepare_support/mod.rs"]
mod support;

use support::{
    HOME, IsolatedMigrationRunner, PRIVATE_SENTINEL, RecordingRunner, SUBID_COUNT, TempStateRoot,
    acceptance_enabled, confirmed_migration, fresh_ready_proposal, mapping_decision,
    prepare_matching_host, read_journal,
};

#[test]
fn migration_only_phase_uses_fresh_evidence_and_persists_one_terminal_journal() {
    if !acceptance_enabled() {
        return;
    }

    let (desired, observed) = prepare_matching_host();
    let decision = confirmed_migration(&desired, &observed);
    let verified = observe_verified_runner_user(&desired, &ProcessExecutor)
        .expect("acquire fresh runner-user evidence");
    assert_eq!(verified.subordinate_uid_count(), u64::from(SUBID_COUNT));
    assert_eq!(verified.subordinate_gid_count(), u64::from(SUBID_COUNT));

    let state = TempStateRoot::new("success");
    let journal_id = state.journal_id("migration-success");
    let sentinel = state.path.join("private-migration-sentinel");
    let mut store = LinuxStateRoot::open(&state.path).expect("open state root");
    let mut checkpoint = StateStoreJournalCheckpoint::new(
        &mut store,
        state.installation_id.clone(),
        journal_id.clone(),
    );
    let mut runner = IsolatedMigrationRunner::new(&verified, sentinel.clone(), false);
    let report = execute_confirmed_host_preparation(decision, &mut runner, &mut checkpoint)
        .expect("execute confirmed migration");
    drop(checkpoint);

    assert_eq!(runner.calls, 1);
    assert!(sentinel.exists());
    assert_eq!(
        report.disposition,
        HostPreparationExecutionDisposition::Completed
    );
    assert_eq!(report.phase_id, "host-preparation-runner-migration-phase");
    assert_eq!(report.journal.records.len(), 1);
    assert_eq!(report.journal.records[0].outcome, ActionOutcome::Completed);
    assert!(report.continuation_barriers.is_empty());
    assert!(report.deferred_actions.is_empty());

    let durable = read_journal(&store, &state.installation_id, &journal_id);
    assert_eq!(durable.journal(), &report.journal);
    let public = serde_json::to_string(&report).expect("serialize report");
    assert!(!public.contains(PRIVATE_SENTINEL));
    assert!(!public.contains(HOME));
    assert!(!public.contains(sentinel.to_string_lossy().as_ref()));

    let fresh = observe_runner_account(
        &desired,
        &ProcessExecutor,
        &RunnerAccountObservationPaths::system_default(),
    )
    .expect("fresh post-execution observation");
    assert!(matches!(
        fresh_ready_proposal(&desired, fresh),
        HostPreparationResult::Ready
    ));
}

#[test]
fn migration_failure_is_terminal_bounded_and_durable() {
    if !acceptance_enabled() {
        return;
    }

    let (desired, observed) = prepare_matching_host();
    let decision = confirmed_migration(&desired, &observed);
    let verified = observe_verified_runner_user(&desired, &ProcessExecutor)
        .expect("acquire fresh runner-user evidence");
    let state = TempStateRoot::new("failure");
    let journal_id = state.journal_id("migration-failed");
    let sentinel = state.path.join("private-failure-sentinel");
    let mut store = LinuxStateRoot::open(&state.path).expect("open state root");
    let mut checkpoint = StateStoreJournalCheckpoint::new(
        &mut store,
        state.installation_id.clone(),
        journal_id.clone(),
    );
    let mut runner = IsolatedMigrationRunner::new(&verified, sentinel.clone(), true);
    let report = execute_confirmed_host_preparation(decision, &mut runner, &mut checkpoint)
        .expect("return bounded failed report");
    drop(checkpoint);

    assert_eq!(runner.calls, 1);
    assert_eq!(
        report.disposition,
        HostPreparationExecutionDisposition::ActionFailed
    );
    assert_eq!(report.journal.records[0].outcome, ActionOutcome::Failed);
    assert_eq!(
        report.journal.stopped_after.as_deref(),
        Some("migrate-runner-podman-after-subordinate-id-change")
    );
    let message = report.journal.records[0]
        .message
        .as_deref()
        .expect("failure message");
    assert!(message.contains("migration_failed"));
    assert!(!message.contains(PRIVATE_SENTINEL));
    assert!(!message.contains(sentinel.to_string_lossy().as_ref()));
    let durable = read_journal(&store, &state.installation_id, &journal_id);
    assert_eq!(durable.journal(), &report.journal);
}

#[test]
fn fresh_evidence_failure_happens_before_state_or_process_access() {
    if !acceptance_enabled() {
        return;
    }

    let (desired, observed) = prepare_matching_host();
    let _decision = confirmed_migration(&desired, &observed);
    let identity = observed.identity().expect("matching account identity");
    let runtime = PathBuf::from(format!("/run/user/{}", identity.uid()));
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o770))
        .expect("invalidate runtime evidence");

    let untouched = env::temp_dir().join(format!(
        "smolrunner-host-prepare-evidence-refusal-{}",
        std::process::id()
    ));
    if untouched.exists() {
        fs::remove_dir_all(&untouched).expect("remove stale refusal path");
    }
    let error = observe_verified_runner_user(&desired, &ProcessExecutor)
        .expect_err("invalid runtime mode must refuse fresh evidence");
    assert_eq!(error.kind(), FreshRunnerUserEvidenceErrorKind::Verification);
    assert!(!untouched.exists());

    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).expect("restore runtime mode");
}

#[test]
fn mapping_phase_stops_at_durable_barrier_without_running_migration() {
    if !acceptance_enabled() {
        return;
    }

    let (desired, observed) = prepare_matching_host();
    let decision = mapping_decision(&desired, &observed);
    let state = TempStateRoot::new("barrier");
    let journal_id = state.journal_id("mapping-barrier");
    let mut store = LinuxStateRoot::open(&state.path).expect("open state root");
    let mut checkpoint = StateStoreJournalCheckpoint::new(
        &mut store,
        state.installation_id.clone(),
        journal_id.clone(),
    );
    let mut runner = RecordingRunner::default();
    let report = execute_confirmed_host_preparation(decision, &mut runner, &mut checkpoint)
        .expect("execute mapping phase");
    drop(checkpoint);

    assert_eq!(
        report.disposition,
        HostPreparationExecutionDisposition::FreshObservationRequired
    );
    assert_eq!(runner.kinds, [LaneCommandKind::EnsureSubordinateUids]);
    assert!(!runner.kinds.contains(&LaneCommandKind::RunnerPodmanMigrate));
    assert_eq!(report.continuation_barriers.len(), 1);
    assert_eq!(report.deferred_actions.len(), 1);
    assert_eq!(
        report.deferred_actions[0].command_kind,
        LaneCommandKind::RunnerPodmanMigrate
    );
    let durable = read_journal(&store, &state.installation_id, &journal_id);
    assert_eq!(durable.journal(), &report.journal);
}
