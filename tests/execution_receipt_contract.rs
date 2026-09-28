use glaeda::artifact::{RepositoryRef, Sha256Digest};
use glaeda::execution_receipt::{
    EXECUTION_RECEIPT_SCHEMA_VERSION, ExecutionReceipt, ExecutionReceiptAction,
    ExecutionReceiptActionOutcome, ExecutionReceiptContinuation, ExecutionReceiptDisposition,
    MAX_EXECUTION_RECEIPT_ACTIONS, ReceiptTimestamp, decode_execution_receipt,
    encode_execution_receipt,
};
use glaeda::journal::{ExecutionLane, RollbackClass};
use glaeda::state::JournalId;
use serde_json::Value;

fn timestamp(value: &str) -> ReceiptTimestamp {
    ReceiptTimestamp::parse(value).expect("timestamp")
}

fn action(
    id: &str,
    outcome: ExecutionReceiptActionOutcome,
    failure: Option<&str>,
) -> ExecutionReceiptAction {
    ExecutionReceiptAction::new(
        id,
        ExecutionLane::Root,
        RollbackClass::Irreversible,
        outcome,
        failure,
    )
    .expect("action")
}

fn no_continuation() -> ExecutionReceiptContinuation {
    ExecutionReceiptContinuation::new(false, [] as [&str; 0], [] as [&str; 0])
        .expect("continuation")
}

fn build_receipt(
    disposition: ExecutionReceiptDisposition,
    actions: Vec<ExecutionReceiptAction>,
    continuation: ExecutionReceiptContinuation,
) -> Result<ExecutionReceipt, glaeda::execution_receipt::ExecutionReceiptError> {
    ExecutionReceipt::new_host_preparation(
        JournalId::parse("host-prepare-0123456789abcdef").expect("journal"),
        RepositoryRef::parse("example/project").expect("repository"),
        Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("digest"),
        "host-preparation-root-phase",
        timestamp("2026-07-26T18:00:00.000Z"),
        timestamp("2026-07-26T18:00:02.125Z"),
        disposition,
        actions,
        continuation,
    )
}

#[test]
fn completed_receipt_round_trips_with_derived_summary_and_fixed_coverage() {
    let receipt = build_receipt(
        ExecutionReceiptDisposition::Completed,
        vec![
            action(
                "ensure-system-group",
                ExecutionReceiptActionOutcome::Completed,
                None,
            ),
            action(
                "ensure-system-user",
                ExecutionReceiptActionOutcome::Completed,
                None,
            ),
        ],
        no_continuation(),
    )
    .expect("receipt");
    let encoded = encode_execution_receipt(&receipt).expect("encoded");
    let decoded = decode_execution_receipt(&encoded).expect("decoded");

    assert_eq!(decoded, receipt);
    assert_eq!(decoded.schema_version(), EXECUTION_RECEIPT_SCHEMA_VERSION);
    assert_eq!(decoded.summary().total(), 2);
    assert_eq!(decoded.summary().completed(), 2);
    assert!(decoded.coverage().redacted());
    assert!(!decoded.coverage().truncated());
    assert!(encoded.ends_with('\n'));
    assert_eq!(
        encoded,
        encode_execution_receipt(&decoded).expect("stable encoding")
    );
}

#[test]
fn fresh_observation_receipt_retains_bounded_continuation_identity() {
    let receipt = build_receipt(
        ExecutionReceiptDisposition::FreshObservationRequired,
        vec![action(
            "ensure-subordinate-uids",
            ExecutionReceiptActionOutcome::Completed,
            None,
        )],
        ExecutionReceiptContinuation::new(
            true,
            ["reobserve-subordinate-ids-and-runner-runtime"],
            ["migrate-runner-podman-after-subordinate-id-change"],
        )
        .expect("continuation"),
    )
    .expect("receipt");

    assert!(receipt.continuation().fresh_observation_required());
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
}

#[test]
fn failed_receipt_requires_typed_codes_and_derives_all_counts() {
    let receipt = build_receipt(
        ExecutionReceiptDisposition::ActionFailed,
        vec![
            action("one", ExecutionReceiptActionOutcome::RolledBack, None),
            action("two", ExecutionReceiptActionOutcome::Compensated, None),
            action(
                "three",
                ExecutionReceiptActionOutcome::Failed,
                Some("lane-process-failed"),
            ),
            action("four", ExecutionReceiptActionOutcome::NotRun, None),
            action("five", ExecutionReceiptActionOutcome::Skipped, None),
            action(
                "six",
                ExecutionReceiptActionOutcome::RollbackFailed,
                Some("rollback-failed"),
            ),
        ],
        ExecutionReceiptContinuation::new(false, [] as [&str; 0], ["reobserve-host-state"])
            .expect("continuation"),
    )
    .expect("receipt");

    assert_eq!(receipt.summary().failed(), 1);
    assert_eq!(receipt.summary().not_run(), 1);
    assert_eq!(receipt.summary().skipped(), 1);
    assert_eq!(receipt.summary().rolled_back(), 1);
    assert_eq!(receipt.summary().compensated(), 1);
    assert_eq!(receipt.summary().rollback_failed(), 1);
    assert_eq!(
        receipt.actions()[2].failure_code(),
        Some("lane-process-failed")
    );
}

#[test]
fn private_or_free_form_values_cannot_enter_receipt_fields() {
    for value in [
        "/private/host/path",
        "UPPERCASE",
        "contains space",
        "secret=token",
        "../escape",
    ] {
        assert!(
            ExecutionReceiptAction::new(
                value,
                ExecutionLane::Root,
                RollbackClass::Irreversible,
                ExecutionReceiptActionOutcome::Completed,
                None,
            )
            .is_err(),
            "accepted {value}"
        );
    }
    assert!(
        ExecutionReceiptAction::new(
            "failed-action",
            ExecutionLane::Root,
            RollbackClass::Irreversible,
            ExecutionReceiptActionOutcome::Failed,
            None,
        )
        .is_err()
    );
    assert!(
        ExecutionReceiptAction::new(
            "completed-action",
            ExecutionLane::Root,
            RollbackClass::Irreversible,
            ExecutionReceiptActionOutcome::Completed,
            Some("unexpected-code"),
        )
        .is_err()
    );
}

#[test]
fn timestamps_reject_invalid_calendar_values_offsets_precision_and_order() {
    for value in [
        "2026-02-29T00:00:00.000Z",
        "2024-02-30T00:00:00.000Z",
        "2026-01-01T24:00:00.000Z",
        "2026-01-01T00:60:00.000Z",
        "2026-01-01T00:00:60.000Z",
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00.000+00:00",
    ] {
        assert!(ReceiptTimestamp::parse(value).is_err(), "accepted {value}");
    }
    assert!(ReceiptTimestamp::parse("2024-02-29T00:00:00.000Z").is_ok());
    assert!(
        ExecutionReceipt::new_host_preparation(
            JournalId::parse("host-prepare-0123456789abcdef").expect("journal"),
            RepositoryRef::parse("example/project").expect("repository"),
            Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("digest"),
            "host-preparation-root-phase",
            timestamp("2026-07-26T18:00:03.000Z"),
            timestamp("2026-07-26T18:00:02.000Z"),
            ExecutionReceiptDisposition::Completed,
            vec![action(
                "one",
                ExecutionReceiptActionOutcome::Completed,
                None,
            )],
            no_continuation(),
        )
        .is_err()
    );
}

#[test]
fn disposition_and_continuation_must_match_terminal_actions() {
    assert!(ExecutionReceiptContinuation::new(true, [] as [&str; 0], [] as [&str; 0]).is_err());
    assert!(ExecutionReceiptContinuation::new(false, ["barrier"], [] as [&str; 0]).is_err());
    assert!(
        build_receipt(
            ExecutionReceiptDisposition::Completed,
            vec![action(
                "failed",
                ExecutionReceiptActionOutcome::Failed,
                Some("failed"),
            )],
            no_continuation(),
        )
        .is_err()
    );
    assert!(
        build_receipt(
            ExecutionReceiptDisposition::FreshObservationRequired,
            vec![action(
                "not-complete",
                ExecutionReceiptActionOutcome::NotRun,
                None,
            )],
            ExecutionReceiptContinuation::new(true, ["barrier"], [] as [&str; 0])
                .expect("continuation"),
        )
        .is_err()
    );
    assert!(
        build_receipt(
            ExecutionReceiptDisposition::ActionFailed,
            vec![action(
                "complete",
                ExecutionReceiptActionOutcome::Completed,
                None,
            )],
            no_continuation(),
        )
        .is_err()
    );
}

#[test]
fn duplicate_and_excessive_action_or_continuation_sets_fail_closed() {
    let duplicate = vec![
        action("same", ExecutionReceiptActionOutcome::Completed, None),
        action("same", ExecutionReceiptActionOutcome::Completed, None),
    ];
    assert!(
        build_receipt(
            ExecutionReceiptDisposition::Completed,
            duplicate,
            no_continuation(),
        )
        .is_err()
    );

    let excessive = (0..=MAX_EXECUTION_RECEIPT_ACTIONS)
        .map(|index| {
            action(
                &format!("action-{index}"),
                ExecutionReceiptActionOutcome::Completed,
                None,
            )
        })
        .collect::<Vec<_>>();
    assert!(
        build_receipt(
            ExecutionReceiptDisposition::Completed,
            excessive,
            no_continuation(),
        )
        .is_err()
    );
    assert!(
        ExecutionReceiptContinuation::new(true, ["duplicate", "duplicate"], [] as [&str; 0])
            .is_err()
    );
}

#[test]
fn decoder_rejects_unknown_fields_versions_and_forged_derived_sections() {
    let receipt = build_receipt(
        ExecutionReceiptDisposition::Completed,
        vec![action(
            "one",
            ExecutionReceiptActionOutcome::Completed,
            None,
        )],
        no_continuation(),
    )
    .expect("receipt");
    let encoded = encode_execution_receipt(&receipt).expect("encoded");
    let mut value = serde_json::from_str::<Value>(&encoded).expect("value");

    value["unknown"] = Value::Bool(true);
    assert!(decode_execution_receipt(&value.to_string()).is_err());
    value.as_object_mut().expect("object").remove("unknown");

    value["schema_version"] = Value::from(99);
    assert!(decode_execution_receipt(&value.to_string()).is_err());
    value["schema_version"] = Value::from(EXECUTION_RECEIPT_SCHEMA_VERSION);

    value["summary"]["completed"] = Value::from(0);
    assert!(decode_execution_receipt(&value.to_string()).is_err());
    value["summary"]["completed"] = Value::from(1);

    value["coverage"]["redacted"] = Value::Bool(false);
    assert!(decode_execution_receipt(&value.to_string()).is_err());
}

#[test]
fn encoded_receipt_contains_only_bounded_public_evidence() {
    let receipt = build_receipt(
        ExecutionReceiptDisposition::ActionFailed,
        vec![action(
            "run-reviewed-command",
            ExecutionReceiptActionOutcome::Failed,
            Some("lane-process-failed"),
        )],
        ExecutionReceiptContinuation::new(false, [] as [&str; 0], ["reobserve-host-state"])
            .expect("continuation"),
    )
    .expect("receipt");
    let encoded = encode_execution_receipt(&receipt).expect("encoded");

    assert!(encoded.contains("command-values"));
    assert!(encoded.contains("process-output"));
    for private in [
        "/var/lib/smolrunner/private",
        "/usr/sbin/runuser --user project-runner",
        "PRIVATE_STDERR_SENTINEL",
        "HOME=/var/lib/project-runner",
        "github_pat_private",
        "precondition evidence from /etc/subuid",
        "journal message with operator prose",
    ] {
        assert!(!encoded.contains(private));
    }
}
