#![cfg(target_os = "linux")]

use glaeda::debian_package_plan::{DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION, PackagePlanDisposition};
use glaeda::execution_receipt::{
    ExecutionReceiptOperation, ReceiptTimestamp, encode_execution_receipt,
};
use glaeda::host_preparation_execution::{
    HOST_PREPARATION_EXECUTION_SCHEMA_VERSION, HostPreparationExecutionDisposition,
    HostPreparationExecutionReport,
};
use glaeda::host_preparation_plan::{
    HostReadinessSourceIdentity, SourceExecutableIdentity, SourceRootlessPodmanIdentity,
    SourceRunnerAccountIdentity,
};
use glaeda::host_preparation_receipt_binding::{
    HostPreparationReceiptBinding, HostPreparationReceiptBindingErrorKind,
    digest_host_preparation_source,
};
use glaeda::host_readiness::HostObservationState;
use glaeda::journal::{
    ActionOutcome, ExecutionJournal, ExecutionLane, JOURNAL_SCHEMA_VERSION, JournalRecord,
    PlannedMutation, Preconditions, RollbackClass,
};
use glaeda::rootless_podman_preflight::RootlessPodmanPreflightState;
use glaeda::state::JournalId;

const PRIVATE_PATH: &str = "/private/reviewed/usr/bin/git";
const CHANGED_PRIVATE_PATH: &str = "/different/private/usr/bin/git";
const PRIVATE_EVIDENCE: &str = "PRIVATE_PRECONDITION_EVIDENCE";
const PRIVATE_MESSAGE: &str = "PRIVATE_JOURNAL_MESSAGE";
const PHASE_ID: &str = "host-preparation-root-phase";

fn source(path: &str) -> HostReadinessSourceIdentity {
    HostReadinessSourceIdentity {
        kind: "host_readiness".to_owned(),
        schema_version: 1,
        repository: "example/project".to_owned(),
        executables: vec![SourceExecutableIdentity {
            name: "git".to_owned(),
            path: path.to_owned(),
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

fn report(source: HostReadinessSourceIdentity, phase_id: &str) -> HostPreparationExecutionReport {
    HostPreparationExecutionReport {
        schema_version: HOST_PREPARATION_EXECUTION_SCHEMA_VERSION,
        source,
        phase_id: phase_id.to_owned(),
        disposition: HostPreparationExecutionDisposition::Completed,
        journal: ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records: vec![JournalRecord {
                action: PlannedMutation::new(
                    "ensure-host",
                    ExecutionLane::Root,
                    "private action summary",
                    RollbackClass::Reversible,
                    Preconditions::new([PRIVATE_EVIDENCE]),
                ),
                outcome: ActionOutcome::Completed,
                message: Some(PRIVATE_MESSAGE.to_owned()),
            }],
            stopped_after: None,
        },
        continuation_barriers: Vec::new(),
        deferred_actions: Vec::new(),
    }
}

fn started_at() -> ReceiptTimestamp {
    ReceiptTimestamp::parse("2026-07-26T20:00:00.000Z").expect("start timestamp")
}

fn terminal_at() -> ReceiptTimestamp {
    ReceiptTimestamp::parse("2026-07-26T20:00:02.000Z").expect("terminal timestamp")
}

fn execution_id() -> JournalId {
    JournalId::parse("host-prepare-0123456789abcdef").expect("execution ID")
}

#[test]
fn begin_binds_exact_source_phase_execution_and_start_before_terminal_mapping() {
    let reviewed_source = source(PRIVATE_PATH);
    let expected_digest = digest_host_preparation_source(&reviewed_source).expect("source digest");
    let binding = HostPreparationReceiptBinding::begin(
        execution_id(),
        &reviewed_source,
        PHASE_ID,
        started_at(),
    )
    .expect("begin receipt binding");

    assert_eq!(binding.execution_id(), &execution_id());
    assert_eq!(binding.source_digest(), &expected_digest);
    assert_eq!(binding.phase_id(), PHASE_ID);
    assert_eq!(binding.started_at(), &started_at());
    let debug = format!("{binding:?}");
    assert!(!debug.contains(PRIVATE_PATH));

    let receipt = binding
        .finish(&report(reviewed_source, PHASE_ID), terminal_at())
        .expect("finish receipt binding");
    assert_eq!(receipt.execution_id(), &execution_id());
    assert_eq!(receipt.started_at(), &started_at());
    assert_eq!(receipt.terminal_at(), &terminal_at());
    let ExecutionReceiptOperation::HostPreparation { source_digest, .. } = receipt.operation();
    assert_eq!(source_digest, &expected_digest);

    let encoded = encode_execution_receipt(&receipt).expect("encode receipt");
    for private in [
        PRIVATE_PATH,
        PRIVATE_EVIDENCE,
        PRIVATE_MESSAGE,
        "private action summary",
    ] {
        assert!(!encoded.contains(private), "receipt disclosed {private}");
    }
}

#[test]
fn source_digest_is_deterministic_domain_separated_and_sensitive_to_private_source_changes() {
    let first = digest_host_preparation_source(&source(PRIVATE_PATH)).expect("first digest");
    let replay = digest_host_preparation_source(&source(PRIVATE_PATH)).expect("replay digest");
    let changed =
        digest_host_preparation_source(&source(CHANGED_PRIVATE_PATH)).expect("changed digest");

    assert_eq!(first, replay);
    assert_ne!(first, changed);
    assert_eq!(
        first.as_str(),
        "sha256:6578b2a8eaedf9f7894da5eba4072cc137ac7148b8d27c51db1098b9ac087c36"
    );
    assert!(first.as_str().starts_with("sha256:"));
    assert_eq!(first.as_str().len(), 71);
}

#[test]
fn terminal_source_mismatch_fails_without_disclosing_private_source_values() {
    let binding = HostPreparationReceiptBinding::begin(
        execution_id(),
        &source(PRIVATE_PATH),
        PHASE_ID,
        started_at(),
    )
    .expect("begin binding");
    let error = binding
        .finish(
            &report(source(CHANGED_PRIVATE_PATH), PHASE_ID),
            terminal_at(),
        )
        .expect_err("changed source must fail");

    assert_eq!(
        error.kind(),
        HostPreparationReceiptBindingErrorKind::SourceMismatch
    );
    assert!(!error.message().contains(PRIVATE_PATH));
    assert!(!error.message().contains(CHANGED_PRIVATE_PATH));
}

#[test]
fn terminal_phase_mismatch_and_backwards_time_fail_closed() {
    let binding = HostPreparationReceiptBinding::begin(
        execution_id(),
        &source(PRIVATE_PATH),
        PHASE_ID,
        started_at(),
    )
    .expect("begin phase binding");
    let error = binding
        .finish(
            &report(source(PRIVATE_PATH), "different-phase"),
            terminal_at(),
        )
        .expect_err("changed phase must fail");
    assert_eq!(
        error.kind(),
        HostPreparationReceiptBindingErrorKind::PhaseMismatch
    );

    let binding = HostPreparationReceiptBinding::begin(
        execution_id(),
        &source(PRIVATE_PATH),
        PHASE_ID,
        started_at(),
    )
    .expect("begin time binding");
    let before = ReceiptTimestamp::parse("2026-07-26T19:59:59.999Z").expect("earlier time");
    let error = binding
        .finish(&report(source(PRIVATE_PATH), PHASE_ID), before)
        .expect_err("backwards terminal time must fail");
    assert_eq!(
        error.kind(),
        HostPreparationReceiptBindingErrorKind::InvalidTerminalTime
    );
}

#[test]
fn invalid_phase_identity_is_rejected_before_source_binding() {
    let error = HostPreparationReceiptBinding::begin(
        execution_id(),
        &source(PRIVATE_PATH),
        "PRIVATE PHASE WITH SPACES",
        started_at(),
    )
    .expect_err("invalid phase identity");
    assert_eq!(
        error.kind(),
        HostPreparationReceiptBindingErrorKind::InvalidPhaseIdentity
    );
    assert!(!error.message().contains("PRIVATE PHASE WITH SPACES"));
}
