#![cfg(target_os = "linux")]

use glaeda::artifact::Sha256Digest;
use glaeda::debian_package_plan::{DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION, PackagePlanDisposition};
use glaeda::execution_receipt::{
    ExecutionReceiptActionOutcome, ExecutionReceiptDisposition, ReceiptTimestamp,
    encode_execution_receipt,
};
use glaeda::host_preparation_execution::{
    HOST_PREPARATION_EXECUTION_SCHEMA_VERSION, HostPreparationExecutionDisposition,
    HostPreparationExecutionReport,
};
use glaeda::host_preparation_plan::{
    DeferredActionReason, DeferredHostPreparationAction, FreshObservationBarrier,
    HostReadinessSourceIdentity, SourceExecutableIdentity, SourceRootlessPodmanIdentity,
    SourceRunnerAccountIdentity,
};
use glaeda::host_preparation_receipt::{
    HostPreparationReceiptContext, HostPreparationReceiptMappingErrorKind,
    map_host_preparation_execution_receipt,
};
use glaeda::host_readiness::HostObservationState;
use glaeda::journal::{
    ActionOutcome, ExecutionJournal, ExecutionLane, JOURNAL_SCHEMA_VERSION, JournalRecord,
    PlannedMutation, Preconditions, RollbackClass,
};
use glaeda::lane_command::LaneCommandKind;
use glaeda::rootless_podman_preflight::RootlessPodmanPreflightState;
use glaeda::state::JournalId;
use serde_json::Value;

const PRIVATE_EXECUTABLE_PATH: &str = "/private/host/usr/bin/git";
const PRIVATE_PRECONDITION: &str = "PRIVATE_PRECONDITION_EVIDENCE";
const PRIVATE_JOURNAL_MESSAGE: &str = "PRIVATE_JOURNAL_MESSAGE";
const PRIVATE_BARRIER_SUMMARY: &str = "PRIVATE_BARRIER_SUMMARY";
const PRIVATE_DEFERRED_SUMMARY: &str = "PRIVATE_DEFERRED_SUMMARY";

fn source(repository: &str) -> HostReadinessSourceIdentity {
    HostReadinessSourceIdentity {
        kind: "host_readiness".to_owned(),
        schema_version: 1,
        repository: repository.to_owned(),
        executables: vec![SourceExecutableIdentity {
            name: "git".to_owned(),
            path: PRIVATE_EXECUTABLE_PATH.to_owned(),
            state: HostObservationState::Matching,
        }],
        package_plan_schema_version: DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION,
        package_disposition: PackagePlanDisposition::Ready,
        runner_account: SourceRunnerAccountIdentity::NeedsConfiguration,
        rootless_podman: SourceRootlessPodmanIdentity::Deferred {
            state: RootlessPodmanPreflightState::Unknown,
        },
    }
}

fn record(id: &str, outcome: ActionOutcome, rollback: RollbackClass) -> JournalRecord {
    JournalRecord {
        action: PlannedMutation::new(
            id,
            ExecutionLane::Root,
            format!("private action summary for {id}"),
            rollback,
            Preconditions::new([PRIVATE_PRECONDITION]),
        ),
        outcome,
        message: Some(PRIVATE_JOURNAL_MESSAGE.to_owned()),
    }
}

fn report(
    disposition: HostPreparationExecutionDisposition,
    records: Vec<JournalRecord>,
) -> HostPreparationExecutionReport {
    HostPreparationExecutionReport {
        schema_version: HOST_PREPARATION_EXECUTION_SCHEMA_VERSION,
        source: source("example/project"),
        phase_id: "host-preparation-root-phase".to_owned(),
        disposition,
        journal: ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records,
            stopped_after: None,
        },
        continuation_barriers: Vec::new(),
        deferred_actions: Vec::new(),
    }
}

fn context() -> HostPreparationReceiptContext {
    HostPreparationReceiptContext {
        execution_id: JournalId::parse("host-prepare-0123456789abcdef").expect("journal ID"),
        source_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
            .expect("source digest"),
        started_at: ReceiptTimestamp::parse("2026-07-26T18:00:00.000Z").expect("start timestamp"),
        terminal_at: ReceiptTimestamp::parse("2026-07-26T18:00:03.000Z")
            .expect("terminal timestamp"),
    }
}

#[test]
fn completed_report_maps_to_one_content_minimised_receipt() {
    let report = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record(
            "ensure-system-user",
            ActionOutcome::Completed,
            RollbackClass::Irreversible,
        )],
    );
    let receipt = map_host_preparation_execution_receipt(&report, context()).expect("receipt");
    let encoded = encode_execution_receipt(&receipt).expect("encoded receipt");
    let value = serde_json::from_str::<Value>(&encoded).expect("receipt JSON");

    assert_eq!(
        receipt.disposition(),
        ExecutionReceiptDisposition::Completed
    );
    assert_eq!(receipt.summary().completed(), 1);
    assert_eq!(value["operation"]["family"], "host_preparation");
    assert_eq!(value["operation"]["repository"], "example/project");
    assert_eq!(
        value["operation"]["source_digest"],
        format!("sha256:{}", "ab".repeat(32))
    );
    assert_eq!(
        value["operation"]["phase_id"],
        "host-preparation-root-phase"
    );
    for private in [
        PRIVATE_EXECUTABLE_PATH,
        PRIVATE_PRECONDITION,
        PRIVATE_JOURNAL_MESSAGE,
        "private action summary",
    ] {
        assert!(!encoded.contains(private), "receipt disclosed {private}");
    }
}

#[test]
fn failure_mapping_uses_generic_codes_and_terminal_public_outcomes() {
    let report = report(
        HostPreparationExecutionDisposition::ActionFailed,
        vec![
            record(
                "rolled-back",
                ActionOutcome::RolledBack,
                RollbackClass::Reversible,
            ),
            record(
                "compensated",
                ActionOutcome::Compensated,
                RollbackClass::Compensating,
            ),
            record("failed", ActionOutcome::Failed, RollbackClass::Irreversible),
            record(
                "not-run",
                ActionOutcome::Pending,
                RollbackClass::Irreversible,
            ),
            record(
                "rollback-failed",
                ActionOutcome::RollbackFailed,
                RollbackClass::Reversible,
            ),
        ],
    );
    let receipt = map_host_preparation_execution_receipt(&report, context()).expect("receipt");

    assert_eq!(
        receipt.disposition(),
        ExecutionReceiptDisposition::ActionFailed
    );
    assert_eq!(
        receipt.actions()[0].outcome(),
        ExecutionReceiptActionOutcome::RolledBack
    );
    assert_eq!(
        receipt.actions()[1].outcome(),
        ExecutionReceiptActionOutcome::Compensated
    );
    assert_eq!(
        receipt.actions()[2].failure_code(),
        Some("action-execution-failed")
    );
    assert_eq!(
        receipt.actions()[3].outcome(),
        ExecutionReceiptActionOutcome::NotRun
    );
    assert_eq!(
        receipt.actions()[4].failure_code(),
        Some("action-rollback-failed")
    );
    assert_eq!(receipt.summary().failed(), 1);
    assert_eq!(receipt.summary().rollback_failed(), 1);
    assert_eq!(receipt.summary().not_run(), 1);
}

#[test]
fn fresh_observation_mapping_retains_only_barrier_and_deferred_identities() {
    let mut report = report(
        HostPreparationExecutionDisposition::FreshObservationRequired,
        vec![record(
            "ensure-subordinate-uids",
            ActionOutcome::Completed,
            RollbackClass::Irreversible,
        )],
    );
    report.continuation_barriers = vec![FreshObservationBarrier {
        id: "reobserve-subordinate-ids-and-runner-runtime".to_owned(),
        after_action_ids: vec!["ensure-subordinate-uids".to_owned()],
        requirements: Vec::new(),
        summary: PRIVATE_BARRIER_SUMMARY.to_owned(),
    }];
    report.deferred_actions = vec![DeferredHostPreparationAction {
        id: "migrate-runner-podman-after-subordinate-id-change".to_owned(),
        lane: ExecutionLane::RunnerUser,
        command_kind: LaneCommandKind::RunnerPodmanMigrate,
        summary: PRIVATE_DEFERRED_SUMMARY.to_owned(),
        depends_on: vec!["ensure-subordinate-uids".to_owned()],
        reason: DeferredActionReason::FreshObservationRequired,
    }];

    let receipt = map_host_preparation_execution_receipt(&report, context()).expect("receipt");
    let encoded = encode_execution_receipt(&receipt).expect("encoded receipt");

    assert_eq!(
        receipt.disposition(),
        ExecutionReceiptDisposition::FreshObservationRequired
    );
    assert_eq!(
        receipt.continuation().barriers().collect::<Vec<_>>(),
        ["reobserve-subordinate-ids-and-runner-runtime"]
    );
    assert_eq!(
        receipt
            .continuation()
            .deferred_actions()
            .collect::<Vec<_>>(),
        ["migrate-runner-podman-after-subordinate-id-change"]
    );
    assert!(!encoded.contains(PRIVATE_BARRIER_SUMMARY));
    assert!(!encoded.contains(PRIVATE_DEFERRED_SUMMARY));
}

#[test]
fn non_terminal_journal_records_fail_closed() {
    for outcome in [ActionOutcome::Executing, ActionOutcome::RollbackInProgress] {
        let report = report(
            HostPreparationExecutionDisposition::ActionFailed,
            vec![record("in-flight", outcome, RollbackClass::Reversible)],
        );
        let error = map_host_preparation_execution_receipt(&report, context())
            .expect_err("non-terminal journal");
        assert_eq!(
            error.kind(),
            HostPreparationReceiptMappingErrorKind::NonTerminalJournal
        );
        assert_eq!(
            error.message(),
            "host-preparation journal contains a non-terminal action"
        );
    }
}

#[test]
fn unsupported_schema_and_invalid_repository_errors_are_bounded() {
    let mut unsupported_execution = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record(
            "one",
            ActionOutcome::Completed,
            RollbackClass::Irreversible,
        )],
    );
    unsupported_execution.schema_version = 99;
    assert_eq!(
        map_host_preparation_execution_receipt(&unsupported_execution, context())
            .expect_err("execution schema")
            .kind(),
        HostPreparationReceiptMappingErrorKind::UnsupportedExecutionSchema
    );

    let mut unsupported_journal = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record(
            "one",
            ActionOutcome::Completed,
            RollbackClass::Irreversible,
        )],
    );
    unsupported_journal.journal.schema_version = 99;
    assert_eq!(
        map_host_preparation_execution_receipt(&unsupported_journal, context())
            .expect_err("journal schema")
            .kind(),
        HostPreparationReceiptMappingErrorKind::UnsupportedJournalSchema
    );

    let mut private_repository = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record(
            "one",
            ActionOutcome::Completed,
            RollbackClass::Irreversible,
        )],
    );
    private_repository.source = source("/private/repository/path");
    let error = map_host_preparation_execution_receipt(&private_repository, context())
        .expect_err("repository identity");
    assert_eq!(
        error.kind(),
        HostPreparationReceiptMappingErrorKind::InvalidRepositoryIdentity
    );
    assert!(!error.to_string().contains("/private/repository/path"));
}

#[test]
fn inconsistent_report_semantics_become_one_generic_receipt_error() {
    let completed_with_failure = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record(
            "failed",
            ActionOutcome::Failed,
            RollbackClass::Irreversible,
        )],
    );
    let error = map_host_preparation_execution_receipt(&completed_with_failure, context())
        .expect_err("inconsistent completed report");
    assert_eq!(
        error.kind(),
        HostPreparationReceiptMappingErrorKind::InvalidReceipt
    );
    assert_eq!(
        error.message(),
        "host-preparation execution cannot produce a valid external receipt"
    );
    assert!(!error.to_string().contains(PRIVATE_JOURNAL_MESSAGE));
}
