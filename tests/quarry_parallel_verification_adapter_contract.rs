use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionRequestId, ExecutionResourceLimits, ReservationGeneration, ReservationId,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerQueueGeneration,
    PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_repository_result::{
    PersonalWorkerJobAttemptGeneration, PersonalWorkerJobAttemptId,
    PersonalWorkerRepositoryAttemptInput, PersonalWorkerRepositoryResultErrorKind,
    PersonalWorkerRepositoryTerminalClass, RepositoryCleanupDisposition,
    RepositoryConcurrencyGrant, RepositoryProcessTerminalClass, RepositoryReceiptAssessment,
    RepositoryReceiptChannelId, RepositoryReceiptContract, RepositorySignalClass,
    RepositoryStopEvidence, RepositoryStopReason, RepositoryVerifierProducerId,
    bind_personal_worker_repository_attempt,
};
use glaeda::personal_worker_store::PersonalWorkerStoreRevision;
use glaeda::quarry_parallel_verification_adapter::{
    QUARRY_PARALLEL_VERIFICATION_PRODUCER_ID, QuarryParallelVerificationAdapterErrorKind,
    QuarryParallelVerificationCapture, QuarryParallelVerificationOuterObservation,
    correlate_quarry_parallel_verification,
};
use glaeda::verification_profile::{
    CacheId, RepositoryCommandId, RepositoryCommandIdentity, VerificationProfileId,
};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

const FIXTURE: &[u8] = include_bytes!("fixtures/quarry_parallel_verification_receipt_v2.json");
const GIB: u64 = 1_024 * 1_024 * 1_024;
const RECEIPT_ID_PREFIX: &str = "quarry-parallel-verification-receipt-v2:sha256:";

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn at(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn repository() -> RepositoryRef {
    RepositoryRef::parse("example/project").expect("repository")
}

fn attempt_id() -> PersonalWorkerJobAttemptId {
    PersonalWorkerJobAttemptId::parse(&format!("pw-job-attempt-v1-{}", "a".repeat(64)))
        .expect("attempt")
}

fn channel_id() -> RepositoryReceiptChannelId {
    RepositoryReceiptChannelId::parse(&format!("repository-receipt-channel-v1-{}", "b".repeat(64)))
        .expect("channel")
}

fn binding_input() -> PersonalWorkerRepositoryAttemptInput {
    PersonalWorkerRepositoryAttemptInput {
        request_id: ExecutionRequestId::parse("request-1").expect("request"),
        attempt_id: attempt_id(),
        attempt_generation: PersonalWorkerJobAttemptGeneration::new(7).expect("attempt generation"),
        predecessor_store_revision: PersonalWorkerStoreRevision::new(10).expect("store revision"),
        predecessor_queue_generation: PersonalWorkerQueueGeneration::new(11)
            .expect("queue generation"),
        source: PersonalWorkerSourceIdentity::new(
            repository(),
            CommitId::parse(&"1".repeat(40)).expect("commit"),
            GitTreeId::parse(&"2".repeat(40)).expect("tree"),
        ),
        verification_profile_id: VerificationProfileId::parse("parallel-full-v2").expect("profile"),
        command: RepositoryCommandIdentity::new(
            repository(),
            RepositoryCommandId::parse("parallel-full").expect("command"),
            digest('6'),
        ),
        toolchain_envelope_digest: digest('3'),
        requested_limits: ExecutionResourceLimits::new(8_000, 10 * GIB, 1_024)
            .expect("requested limits"),
        applied_limits: ExecutionResourceLimits::new(7_000, 8 * GIB, 768).expect("applied limits"),
        repository_concurrency: RepositoryConcurrencyGrant::new(2).expect("concurrency"),
        reservation_id: ReservationId::parse("reservation-1").expect("reservation"),
        reservation_generation: ReservationGeneration::new(3).expect("reservation generation"),
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("repository-build").expect("cache id"),
            repository: repository(),
            namespace_digest: digest('4'),
        },
        cache_access: PersonalWorkerCacheAccessMode::Exclusive,
        cache_lease_acquired_at: at(100),
        bound_at: at(110),
        not_after: at(10_000),
        receipt_contract: RepositoryReceiptContract::new(
            RepositoryVerifierProducerId::parse(QUARRY_PARALLEL_VERIFICATION_PRODUCER_ID)
                .expect("producer"),
            2,
            channel_id(),
            65_536,
        )
        .expect("receipt contract"),
    }
}

fn observation(
    capture: QuarryParallelVerificationCapture<'_>,
) -> QuarryParallelVerificationOuterObservation<'_> {
    QuarryParallelVerificationOuterObservation {
        request_id: ExecutionRequestId::parse("request-1").expect("request"),
        attempt_id: attempt_id(),
        attempt_generation: PersonalWorkerJobAttemptGeneration::new(7).expect("attempt generation"),
        reservation_id: ReservationId::parse("reservation-1").expect("reservation"),
        reservation_generation: ReservationGeneration::new(3).expect("reservation generation"),
        started_at: at(115),
        terminal_at: at(2_100),
        process_terminal: RepositoryProcessTerminalClass::ExitedFailure,
        stop: None,
        resource_exhaustion: None,
        outer_cleanup: RepositoryCleanupDisposition::Complete,
        channel_id: channel_id(),
        capture,
        aggregate_started_at: at(120),
        aggregate_terminal_at: at(2_000),
        maximum_parallelism_observed: 2,
    }
}

fn correlate(
    input: PersonalWorkerRepositoryAttemptInput,
    observation: QuarryParallelVerificationOuterObservation<'_>,
) -> Result<
    glaeda::personal_worker_repository_result::PersonalWorkerRepositoryCompletionInput,
    glaeda::quarry_parallel_verification_adapter::QuarryParallelVerificationAdapterError,
> {
    correlate_quarry_parallel_verification(
        bind_personal_worker_repository_attempt(input).expect("binding"),
        observation,
    )
}

fn fixture_value() -> Value {
    serde_json::from_slice(&FIXTURE[..FIXTURE.len() - 1]).expect("fixture JSON")
}

fn canonical(mut value: Value) -> Vec<u8> {
    refresh_receipt_id(&mut value);
    let mut bytes = serde_json::to_vec(&value).expect("canonical JSON");
    bytes.push(b'\n');
    bytes
}

fn refresh_receipt_id(value: &mut Value) {
    let object = value.as_object().expect("receipt object");
    let mut core = Map::new();
    for field in [
        "plan",
        "result",
        "outcomes",
        "cleanup",
        "evidence_scope",
        "verified_head",
        "hosted_ci_evidence",
        "merge_authority",
    ] {
        core.insert(field.to_owned(), object[field].clone());
    }
    let encoded = serde_json::to_vec(&Value::Object(core)).expect("core JSON");
    value["receipt_id"] = json!(format!("{RECEIPT_ID_PREFIX}{:x}", Sha256::digest(encoded)));
}

fn passing_fixture() -> Vec<u8> {
    let mut value = fixture_value();
    value["outcomes"][0]["state"] = json!("passed");
    value["outcomes"][0]["exit_code"] = json!(0);
    value["outcomes"][1] = json!({
        "collection_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "exit_code": 0,
        "name": "regime",
        "output_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "state": "passed",
        "wall_millis": 100
    });
    value["result"]["class"] = json!("passed");
    value["result"]["termination_reason"] = json!("completed");
    value["evidence_scope"] = json!("exact_head");
    value["verified_head"] = json!("1".repeat(40));
    canonical(value)
}

#[test]
fn one_valid_quarry_receipt_correlates_to_one_outer_attempt() {
    let completion = correlate(
        binding_input(),
        observation(QuarryParallelVerificationCapture::Bytes(FIXTURE)),
    )
    .expect("correlated failure");

    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::RepositoryVerificationFailed
    );
    assert_eq!(completion.binding().request_id().as_str(), "request-1");
    let RepositoryReceiptAssessment::Present { receipt } = completion.repository_receipt() else {
        panic!("valid Quarry receipt must be present")
    };
    assert_eq!(receipt.work_detail.work_unit_count(), 2);
    assert_eq!(receipt.maximum_parallelism_observed, 2);
    let expected_digest = format!("sha256:{:x}", Sha256::digest(FIXTURE));
    assert_eq!(receipt.receipt_digest.as_str(), expected_digest);
}

#[test]
fn valid_completed_receipt_can_pass_only_with_matching_outer_success() {
    let bytes = passing_fixture();
    let mut outer = observation(QuarryParallelVerificationCapture::Bytes(&bytes));
    outer.process_terminal = RepositoryProcessTerminalClass::ExitedSuccess;

    let completion = correlate(binding_input(), outer).expect("correlated success");

    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::Passed
    );
}

#[test]
fn missing_malformed_and_overflow_are_terminal_without_retaining_bytes() {
    let missing = correlate(
        binding_input(),
        observation(QuarryParallelVerificationCapture::Missing),
    )
    .expect("missing receipt completion");
    assert_eq!(
        missing.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ReceiptMissing
    );

    let private_sentinel = b"/private/path token=do-not-retain\n";
    let malformed = correlate(
        binding_input(),
        observation(QuarryParallelVerificationCapture::Bytes(private_sentinel)),
    )
    .expect("malformed receipt completion");
    assert_eq!(
        malformed.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ReceiptMalformed
    );
    let public = serde_json::to_string(&malformed).expect("public completion");
    assert!(!public.contains("private"));
    assert!(!public.contains("do-not-retain"));

    let overflow = correlate(
        binding_input(),
        observation(QuarryParallelVerificationCapture::Overflow {
            bytes_observed_at_least: 65_537,
        }),
    )
    .expect("overflow completion");
    assert_eq!(
        overflow.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ReceiptMalformed
    );

    let oversized = vec![b'x'; 65_537];
    let oversized = correlate(
        binding_input(),
        observation(QuarryParallelVerificationCapture::Bytes(&oversized)),
    )
    .expect("oversized byte capture completion");
    assert_eq!(
        oversized.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ReceiptMalformed
    );
}

#[test]
fn source_toolchain_concurrency_and_channel_drift_fail_closed() {
    let mut source_drift = binding_input();
    source_drift.source.commit = CommitId::parse(&"9".repeat(40)).expect("commit");
    assert_eq!(
        correlate(
            source_drift,
            observation(QuarryParallelVerificationCapture::Bytes(FIXTURE)),
        )
        .expect_err("source drift")
        .kind(),
        QuarryParallelVerificationAdapterErrorKind::BindingMismatch
    );

    let mut toolchain_drift = binding_input();
    toolchain_drift.toolchain_envelope_digest = digest('9');
    assert_eq!(
        correlate(
            toolchain_drift,
            observation(QuarryParallelVerificationCapture::Bytes(FIXTURE)),
        )
        .expect_err("toolchain drift")
        .kind(),
        QuarryParallelVerificationAdapterErrorKind::BindingMismatch
    );

    let mut concurrency_drift = binding_input();
    concurrency_drift.repository_concurrency =
        RepositoryConcurrencyGrant::new(3).expect("concurrency");
    assert_eq!(
        correlate(
            concurrency_drift,
            observation(QuarryParallelVerificationCapture::Bytes(FIXTURE)),
        )
        .expect_err("concurrency drift")
        .kind(),
        QuarryParallelVerificationAdapterErrorKind::BindingMismatch
    );

    let mut channel_drift = observation(QuarryParallelVerificationCapture::Bytes(FIXTURE));
    channel_drift.channel_id = RepositoryReceiptChannelId::parse(&format!(
        "repository-receipt-channel-v1-{}",
        "c".repeat(64)
    ))
    .expect("channel");
    assert_eq!(
        correlate(binding_input(), channel_drift)
            .expect_err("channel drift")
            .kind(),
        QuarryParallelVerificationAdapterErrorKind::Correlation(
            PersonalWorkerRepositoryResultErrorKind::ReceiptContractMismatch
        )
    );
}

#[test]
fn producer_contract_drift_fails_even_without_receipt_bytes() {
    let mut producer_drift = binding_input();
    producer_drift.receipt_contract = RepositoryReceiptContract::new(
        RepositoryVerifierProducerId::parse("not-quarry-v2").expect("producer"),
        2,
        channel_id(),
        65_536,
    )
    .expect("receipt contract");
    assert_eq!(
        correlate(
            producer_drift,
            observation(QuarryParallelVerificationCapture::Missing),
        )
        .expect_err("producer drift")
        .kind(),
        QuarryParallelVerificationAdapterErrorKind::BindingMismatch
    );
}

#[test]
fn cancellation_before_any_shard_starts_preserves_zero_observed_parallelism() {
    let mut value = fixture_value();
    for outcome in value["outcomes"].as_array_mut().expect("outcomes") {
        outcome["state"] = json!("not_started");
        outcome["wall_millis"] = Value::Null;
        outcome["exit_code"] = Value::Null;
        outcome["output_sha256"] = Value::Null;
        outcome["collection_sha256"] = Value::Null;
    }
    value["result"]["termination_reason"] = json!("cancelled");
    let bytes = canonical(value);
    let mut outer = observation(QuarryParallelVerificationCapture::Bytes(&bytes));
    outer.process_terminal = RepositoryProcessTerminalClass::Cancelled;
    outer.maximum_parallelism_observed = 0;
    outer.stop = Some(RepositoryStopEvidence {
        reason: RepositoryStopReason::OperatorRequested,
        signal: Some(RepositorySignalClass::Interrupt),
    });

    let completion = correlate(binding_input(), outer).expect("cancelled before work");

    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::Cancelled
    );
    let RepositoryReceiptAssessment::Present { receipt } = completion.repository_receipt() else {
        panic!("valid cancellation receipt must be present")
    };
    assert_eq!(receipt.work_detail.work_unit_count(), 0);
    assert_eq!(receipt.maximum_parallelism_observed, 0);
}

#[test]
fn timeline_parallelism_and_invalid_overflow_evidence_fail_closed() {
    let mut short = observation(QuarryParallelVerificationCapture::Bytes(FIXTURE));
    short.aggregate_terminal_at = at(1_000);
    assert_eq!(
        correlate(binding_input(), short)
            .expect_err("short aggregate")
            .kind(),
        QuarryParallelVerificationAdapterErrorKind::InvalidTimeline
    );

    let mut excessive = observation(QuarryParallelVerificationCapture::Bytes(FIXTURE));
    excessive.maximum_parallelism_observed = 3;
    assert_eq!(
        correlate(binding_input(), excessive)
            .expect_err("parallelism")
            .kind(),
        QuarryParallelVerificationAdapterErrorKind::InvalidTimeline
    );

    assert_eq!(
        correlate(
            binding_input(),
            observation(QuarryParallelVerificationCapture::Overflow {
                bytes_observed_at_least: 65_536,
            }),
        )
        .expect_err("invalid overflow")
        .kind(),
        QuarryParallelVerificationAdapterErrorKind::InvalidCapture
    );
}

#[test]
fn cleanup_and_outer_cancellation_cannot_be_mislabeled_passing() {
    let mut cleanup_value = fixture_value();
    cleanup_value["result"]["termination_reason"] = json!("cleanup_failure");
    cleanup_value["cleanup"]["status"] = json!("failed");
    cleanup_value["cleanup"]["failure_codes"] = json!(["temporary_root"]);
    let cleanup_bytes = canonical(cleanup_value);
    let cleanup = correlate(
        binding_input(),
        observation(QuarryParallelVerificationCapture::Bytes(&cleanup_bytes)),
    )
    .expect("cleanup completion");
    assert_eq!(
        cleanup.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::CleanupIncomplete
    );

    let mut internal = fixture_value();
    internal["result"]["termination_reason"] = json!("internal_failure");
    let internal_bytes = canonical(internal);
    let mut cancelled = observation(QuarryParallelVerificationCapture::Bytes(&internal_bytes));
    cancelled.process_terminal = RepositoryProcessTerminalClass::Cancelled;
    cancelled.stop = Some(RepositoryStopEvidence {
        reason: RepositoryStopReason::OperatorRequested,
        signal: Some(RepositorySignalClass::Interrupt),
    });
    let completion = correlate(binding_input(), cancelled).expect("cancelled completion");
    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::Cancelled
    );
}
