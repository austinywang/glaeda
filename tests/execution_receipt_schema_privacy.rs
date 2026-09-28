use glaeda::artifact::{RepositoryRef, Sha256Digest};
use glaeda::execution_receipt::{
    ExecutionReceipt, ExecutionReceiptAction, ExecutionReceiptActionOutcome,
    ExecutionReceiptContinuation, ExecutionReceiptDisposition, ExecutionReceiptOperation,
    ReceiptTimestamp, decode_execution_receipt, encode_execution_receipt,
};
use glaeda::journal::{ExecutionLane, RollbackClass};
use glaeda::state::JournalId;
use serde_json::Value;

fn receipt() -> ExecutionReceipt {
    ExecutionReceipt::new_host_preparation(
        JournalId::parse("host-prepare-0123456789abcdef").expect("journal"),
        RepositoryRef::parse("example/project").expect("repository"),
        Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("digest"),
        "host-preparation-root-phase",
        ReceiptTimestamp::parse("2026-07-26T18:00:00.000Z").expect("started"),
        ReceiptTimestamp::parse("2026-07-26T18:00:02.000Z").expect("terminal"),
        ExecutionReceiptDisposition::Completed,
        vec![
            ExecutionReceiptAction::new(
                "ensure-system-user",
                ExecutionLane::Root,
                RollbackClass::Irreversible,
                ExecutionReceiptActionOutcome::Completed,
                None,
            )
            .expect("action"),
        ],
        ExecutionReceiptContinuation::new(false, [] as [&str; 0], [] as [&str; 0])
            .expect("continuation"),
    )
    .expect("receipt")
}

#[test]
fn typed_operation_exposes_the_reviewed_public_binding() {
    let receipt = receipt();
    match receipt.operation() {
        ExecutionReceiptOperation::HostPreparation {
            schema_version,
            repository,
            source_digest,
            phase_id,
        } => {
            assert_eq!(*schema_version, 1);
            assert_eq!(repository.as_str(), "example/project");
            assert_eq!(
                source_digest.as_str(),
                format!("sha256:{}", "ab".repeat(32))
            );
            assert_eq!(
                serde_json::to_value(phase_id).expect("phase JSON"),
                Value::String("host-preparation-root-phase".to_owned())
            );
        }
    }
}

#[test]
fn untrusted_schema_errors_do_not_echo_attacker_controlled_field_names() {
    let encoded = encode_execution_receipt(&receipt()).expect("encoded");
    let mut value = serde_json::from_str::<Value>(&encoded).expect("JSON");
    let sentinel = "PRIVATE_UNKNOWN_FIELD_SENTINEL";

    value["operation"][sentinel] = Value::String("PRIVATE_VALUE_SENTINEL".to_owned());
    let error = decode_execution_receipt(&value.to_string()).expect_err("unknown operation field");
    let message = error.to_string();
    assert_eq!(
        message,
        "execution receipt validation failed\n- execution receipt JSON or schema is invalid\n"
    );
    assert!(!message.contains(sentinel));
    assert!(!message.contains("PRIVATE_VALUE_SENTINEL"));
}

#[test]
fn producer_versions_require_an_alphanumeric_leading_character() {
    let encoded = encode_execution_receipt(&receipt()).expect("encoded");
    let mut value = serde_json::from_str::<Value>(&encoded).expect("JSON");
    value["producer"]["version"] = Value::String("...".to_owned());

    let error = decode_execution_receipt(&value.to_string()).expect_err("punctuation version");
    assert!(
        error
            .to_string()
            .contains("producer version must be a bounded ASCII version token")
    );
}
