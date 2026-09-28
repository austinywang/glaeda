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
    PersonalWorkerOuterTerminalEvidence, PersonalWorkerRepositoryAttemptInput,
    PersonalWorkerRepositoryResultErrorKind, PersonalWorkerRepositoryTerminalClass,
    RepositoryAggregateReceipt, RepositoryCleanupDisposition,
    RepositoryCompletionReplayDisposition, RepositoryConcurrencyGrant,
    RepositoryProcessTerminalClass, RepositoryReceiptAssessment, RepositoryReceiptChannelId,
    RepositoryReceiptChannelObservation, RepositoryReceiptContract, RepositoryReceiptTerminalClass,
    RepositoryResourceExhaustionClass, RepositoryResourceExhaustionEvidence, RepositorySignalClass,
    RepositoryStopEvidence, RepositoryStopReason, RepositoryVerifierProducerId,
    RepositoryWorkDetail, RepositoryWorkUnitId, RepositoryWorkUnitOutcome,
    RepositoryWorkUnitRecord, bind_personal_worker_repository_attempt,
    classify_repository_completion_replay, correlate_personal_worker_repository_result,
};
use glaeda::personal_worker_store::PersonalWorkerStoreRevision;
use glaeda::verification_profile::{
    CacheId, RepositoryCommandId, RepositoryCommandIdentity, VerificationProfileId,
};

const GIB: u64 = 1_024 * 1_024 * 1_024;

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn at(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn repository() -> RepositoryRef {
    RepositoryRef::parse("example/project").expect("repository")
}

fn source() -> PersonalWorkerSourceIdentity {
    PersonalWorkerSourceIdentity::new(
        repository(),
        CommitId::parse(&"1".repeat(40)).expect("commit"),
        GitTreeId::parse(&"2".repeat(40)).expect("tree"),
    )
}

fn profile() -> VerificationProfileId {
    VerificationProfileId::parse("parallel-full-v1").expect("profile")
}

fn command() -> RepositoryCommandIdentity {
    RepositoryCommandIdentity::new(
        repository(),
        RepositoryCommandId::parse("parallel-full").expect("command"),
        digest('3'),
    )
}

fn requested_limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(8_000, 10 * GIB, 1_024).expect("requested limits")
}

fn applied_limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(7_000, 8 * GIB, 768).expect("applied limits")
}

fn attempt_id() -> PersonalWorkerJobAttemptId {
    PersonalWorkerJobAttemptId::parse(&format!("pw-job-attempt-v1-{}", "a".repeat(64)))
        .expect("attempt")
}

fn channel_id() -> RepositoryReceiptChannelId {
    RepositoryReceiptChannelId::parse(&format!("repository-receipt-channel-v1-{}", "b".repeat(64)))
        .expect("channel")
}

fn cache_namespace() -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("repository-build").expect("cache id"),
        repository: repository(),
        namespace_digest: digest('4'),
    }
}

fn receipt_contract() -> RepositoryReceiptContract {
    RepositoryReceiptContract::new(
        RepositoryVerifierProducerId::parse("parallel-full-producer").expect("producer"),
        1,
        channel_id(),
        65_536,
    )
    .expect("receipt contract")
}

fn binding_input() -> PersonalWorkerRepositoryAttemptInput {
    PersonalWorkerRepositoryAttemptInput {
        request_id: ExecutionRequestId::parse("request-1").expect("request"),
        attempt_id: attempt_id(),
        attempt_generation: PersonalWorkerJobAttemptGeneration::new(7).expect("attempt generation"),
        predecessor_store_revision: PersonalWorkerStoreRevision::new(10).expect("store revision"),
        predecessor_queue_generation: PersonalWorkerQueueGeneration::new(11)
            .expect("queue generation"),
        source: source(),
        verification_profile_id: profile(),
        command: command(),
        toolchain_envelope_digest: digest('5'),
        requested_limits: requested_limits(),
        applied_limits: applied_limits(),
        repository_concurrency: RepositoryConcurrencyGrant::new(4).expect("concurrency"),
        reservation_id: ReservationId::parse("reservation-1").expect("reservation"),
        reservation_generation: ReservationGeneration::new(3).expect("reservation generation"),
        cache_namespace: cache_namespace(),
        cache_access: PersonalWorkerCacheAccessMode::Exclusive,
        cache_lease_acquired_at: at(100),
        bound_at: at(110),
        not_after: at(10_000),
        receipt_contract: receipt_contract(),
    }
}

fn binding() -> glaeda::personal_worker_repository_result::PersonalWorkerRepositoryAttemptBinding {
    bind_personal_worker_repository_attempt(binding_input()).expect("binding")
}

fn work_units(count: usize, outcome: RepositoryWorkUnitOutcome) -> Vec<RepositoryWorkUnitRecord> {
    (0..count)
        .map(|index| RepositoryWorkUnitRecord {
            id: RepositoryWorkUnitId::parse(&format!("shard-{index:02}")).expect("work unit"),
            command_digest: digest('6'),
            wall_millis: 80 + u64::try_from(index).expect("small index"),
            outcome,
            output_digest: digest('7'),
        })
        .collect()
}

fn receipt() -> RepositoryAggregateReceipt {
    RepositoryAggregateReceipt {
        request_id: ExecutionRequestId::parse("request-1").expect("request"),
        attempt_id: attempt_id(),
        attempt_generation: PersonalWorkerJobAttemptGeneration::new(7).expect("attempt generation"),
        predecessor_store_revision: PersonalWorkerStoreRevision::new(10).expect("store revision"),
        predecessor_queue_generation: PersonalWorkerQueueGeneration::new(11)
            .expect("queue generation"),
        source: source(),
        verification_profile_id: profile(),
        command: command(),
        toolchain_envelope_digest: digest('5'),
        requested_limits: requested_limits(),
        applied_limits: applied_limits(),
        repository_concurrency: RepositoryConcurrencyGrant::new(4).expect("concurrency"),
        reservation_id: ReservationId::parse("reservation-1").expect("reservation"),
        reservation_generation: ReservationGeneration::new(3).expect("reservation generation"),
        cache_namespace: cache_namespace(),
        cache_access: PersonalWorkerCacheAccessMode::Exclusive,
        cache_lease_acquired_at: at(100),
        not_after: at(10_000),
        producer_id: RepositoryVerifierProducerId::parse("parallel-full-producer")
            .expect("producer"),
        producer_schema_version: 1,
        receipt_digest: digest('8'),
        aggregate_started_at: at(120),
        aggregate_terminal_at: at(500),
        terminal_class: RepositoryReceiptTerminalClass::Passed,
        maximum_parallelism_observed: 4,
        work_detail: RepositoryWorkDetail::WorkUnits {
            work_units: work_units(15, RepositoryWorkUnitOutcome::Passed),
        },
        stop: None,
        resource_exhaustion: None,
        repository_cleanup: RepositoryCleanupDisposition::Complete,
    }
}

fn outer() -> PersonalWorkerOuterTerminalEvidence {
    PersonalWorkerOuterTerminalEvidence {
        request_id: ExecutionRequestId::parse("request-1").expect("request"),
        attempt_id: attempt_id(),
        attempt_generation: PersonalWorkerJobAttemptGeneration::new(7).expect("attempt generation"),
        reservation_id: ReservationId::parse("reservation-1").expect("reservation"),
        reservation_generation: ReservationGeneration::new(3).expect("reservation generation"),
        started_at: at(115),
        terminal_at: at(510),
        process_terminal: RepositoryProcessTerminalClass::ExitedSuccess,
        stop: None,
        resource_exhaustion: None,
        outer_cleanup: RepositoryCleanupDisposition::Complete,
        receipt_channel: RepositoryReceiptChannelObservation::Captured {
            channel_id: channel_id(),
            bytes_written: 4_096,
            digest: digest('8'),
        },
    }
}

fn correlate_receipt(
    receipt: RepositoryAggregateReceipt,
) -> Result<
    glaeda::personal_worker_repository_result::PersonalWorkerRepositoryCompletionInput,
    glaeda::personal_worker_repository_result::PersonalWorkerRepositoryResultError,
> {
    correlate_personal_worker_repository_result(
        binding(),
        outer(),
        RepositoryReceiptAssessment::present(receipt),
    )
}

fn assert_receipt_error(
    receipt: RepositoryAggregateReceipt,
    expected: PersonalWorkerRepositoryResultErrorKind,
) {
    assert_eq!(
        correlate_receipt(receipt).expect_err("must reject").kind(),
        expected
    );
}

#[test]
fn one_parallel_repository_verification_is_one_passing_attempt() {
    let completion = correlate_receipt(receipt()).expect("correlated completion");

    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::Passed
    );
    assert_eq!(completion.binding().request_id().as_str(), "request-1");
    assert_eq!(completion.binding().attempt_generation().get(), 7);
    assert_eq!(completion.binding().repository_concurrency().get(), 4);

    let document = serde_json::to_value(&completion).expect("public completion JSON");
    assert_eq!(document["binding"]["request_id"], "request-1");
    assert_eq!(
        document["repository_receipt"]["receipt"]["work_detail"]["work_units"]
            .as_array()
            .expect("work units")
            .len(),
        15
    );
    assert!(!document.to_string().contains("/home/"));
}

#[test]
fn total_work_units_may_exceed_concurrency_but_observed_parallelism_may_not() {
    correlate_receipt(receipt()).expect("15 work units scheduled within four workers");

    let mut too_parallel = receipt();
    too_parallel.maximum_parallelism_observed = 5;
    assert_receipt_error(
        too_parallel,
        PersonalWorkerRepositoryResultErrorKind::ConcurrencyExceeded,
    );

    let mut too_many = receipt();
    too_many.work_detail = RepositoryWorkDetail::DetailDigest {
        work_unit_count: 513,
        detail_digest: digest('9'),
    };
    assert_receipt_error(
        too_many,
        PersonalWorkerRepositoryResultErrorKind::ConcurrencyExceeded,
    );
}

#[test]
fn binding_rejects_source_cache_resource_and_timeline_drift() {
    let mut input = binding_input();
    input.command = RepositoryCommandIdentity::new(
        RepositoryRef::parse("other/project").expect("other repository"),
        RepositoryCommandId::parse("parallel-full").expect("command"),
        digest('3'),
    );
    assert_eq!(
        bind_personal_worker_repository_attempt(input)
            .expect_err("source mismatch")
            .kind(),
        PersonalWorkerRepositoryResultErrorKind::SourceMismatch
    );

    let mut input = binding_input();
    input.cache_namespace = PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("repository-build").expect("cache id"),
        repository: RepositoryRef::parse("other/project").expect("other repository"),
        namespace_digest: digest('4'),
    };
    assert_eq!(
        bind_personal_worker_repository_attempt(input)
            .expect_err("cache mismatch")
            .kind(),
        PersonalWorkerRepositoryResultErrorKind::CacheMismatch
    );

    let mut input = binding_input();
    input.applied_limits = ExecutionResourceLimits::new(8_001, 8 * GIB, 768).expect("limits");
    assert_eq!(
        bind_personal_worker_repository_attempt(input)
            .expect_err("resource mismatch")
            .kind(),
        PersonalWorkerRepositoryResultErrorKind::ResourceMismatch
    );

    let mut input = binding_input();
    input.bound_at = at(10_000);
    assert_eq!(
        bind_personal_worker_repository_attempt(input)
            .expect_err("timeline")
            .kind(),
        PersonalWorkerRepositoryResultErrorKind::InvalidTimeline
    );
}

#[test]
fn receipt_must_match_every_outer_binding_dimension() {
    let mut changed = receipt();
    changed.attempt_generation = PersonalWorkerJobAttemptGeneration::new(8).expect("generation");
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::AttemptMismatch,
    );

    let mut changed = receipt();
    changed.source.tree = GitTreeId::parse(&"f".repeat(40)).expect("tree");
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::SourceMismatch,
    );

    let mut changed = receipt();
    changed.verification_profile_id =
        VerificationProfileId::parse("other-profile").expect("profile");
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::VerificationMismatch,
    );

    let mut changed = receipt();
    changed.toolchain_envelope_digest = digest('a');
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::VerificationMismatch,
    );

    let mut changed = receipt();
    changed.applied_limits = ExecutionResourceLimits::new(6_000, 8 * GIB, 768).expect("limits");
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::ResourceMismatch,
    );

    let mut changed = receipt();
    changed.reservation_generation = ReservationGeneration::new(4).expect("generation");
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::ReservationMismatch,
    );

    let mut changed = receipt();
    changed.cache_namespace = PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("repository-build").expect("cache id"),
        repository: repository(),
        namespace_digest: digest('a'),
    };
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::CacheMismatch,
    );

    let mut changed = receipt();
    changed.not_after = at(9_999);
    assert_receipt_error(
        changed,
        PersonalWorkerRepositoryResultErrorKind::DeadlineMismatch,
    );
}

#[test]
fn incomplete_cleanup_prevents_semantic_success() {
    let mut repository_incomplete = receipt();
    repository_incomplete.repository_cleanup = RepositoryCleanupDisposition::Incomplete;
    assert_eq!(
        correlate_receipt(repository_incomplete)
            .expect("typed cleanup failure")
            .terminal_class(),
        PersonalWorkerRepositoryTerminalClass::CleanupIncomplete
    );

    let mut outer_incomplete = outer();
    outer_incomplete.outer_cleanup = RepositoryCleanupDisposition::Incomplete;
    let completion = correlate_personal_worker_repository_result(
        binding(),
        outer_incomplete,
        RepositoryReceiptAssessment::present(receipt()),
    )
    .expect("typed outer cleanup failure");
    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::CleanupIncomplete
    );

    let mut contradictory_outer = outer();
    contradictory_outer.process_terminal = RepositoryProcessTerminalClass::ExitedFailure;
    contradictory_outer.outer_cleanup = RepositoryCleanupDisposition::Incomplete;
    assert_eq!(
        correlate_personal_worker_repository_result(
            binding(),
            contradictory_outer,
            RepositoryReceiptAssessment::present(receipt()),
        )
        .expect_err("cleanup debt cannot excuse terminal drift")
        .kind(),
        PersonalWorkerRepositoryResultErrorKind::TerminalMismatch
    );
}

#[test]
fn missing_malformed_overflow_and_runner_loss_are_durable_terminal_classes() {
    let mut missing_outer = outer();
    missing_outer.process_terminal = RepositoryProcessTerminalClass::ExitedFailure;
    missing_outer.receipt_channel = RepositoryReceiptChannelObservation::Empty {
        channel_id: channel_id(),
    };
    let missing = correlate_personal_worker_repository_result(
        binding(),
        missing_outer,
        RepositoryReceiptAssessment::Missing,
    )
    .expect("missing terminal");
    assert_eq!(
        missing.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ReceiptMissing
    );

    let mut malformed_outer = outer();
    malformed_outer.process_terminal = RepositoryProcessTerminalClass::ExitedFailure;
    malformed_outer.receipt_channel = RepositoryReceiptChannelObservation::Malformed {
        channel_id: channel_id(),
        bytes_written: 512,
        observed_digest: Some(digest('9')),
    };
    let malformed = correlate_personal_worker_repository_result(
        binding(),
        malformed_outer,
        RepositoryReceiptAssessment::Malformed {
            observed_digest: Some(digest('9')),
        },
    )
    .expect("malformed terminal");
    assert_eq!(
        malformed.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ReceiptMalformed
    );

    let mut overflow_outer = outer();
    overflow_outer.process_terminal = RepositoryProcessTerminalClass::ExitedFailure;
    overflow_outer.receipt_channel = RepositoryReceiptChannelObservation::Overflow {
        channel_id: channel_id(),
        bytes_observed_at_least: 65_537,
    };
    let overflow = correlate_personal_worker_repository_result(
        binding(),
        overflow_outer,
        RepositoryReceiptAssessment::Malformed {
            observed_digest: None,
        },
    )
    .expect("overflow terminal");
    assert_eq!(
        overflow.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ReceiptMalformed
    );

    let mut lost_outer = outer();
    lost_outer.process_terminal = RepositoryProcessTerminalClass::RunnerLost;
    lost_outer.receipt_channel = RepositoryReceiptChannelObservation::Empty {
        channel_id: channel_id(),
    };
    let lost = correlate_personal_worker_repository_result(
        binding(),
        lost_outer,
        RepositoryReceiptAssessment::Missing,
    )
    .expect("runner-lost terminal");
    assert_eq!(
        lost.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::RunnerLost
    );
}

#[test]
fn absent_receipt_does_not_erase_trustworthy_outer_stop_or_exhaustion_evidence() {
    for (process_terminal, stop, resource_exhaustion, expected) in [
        (
            RepositoryProcessTerminalClass::Timeout,
            Some(RepositoryStopEvidence {
                reason: RepositoryStopReason::DeadlineExceeded,
                signal: Some(RepositorySignalClass::Terminate),
            }),
            None,
            PersonalWorkerRepositoryTerminalClass::Timeout,
        ),
        (
            RepositoryProcessTerminalClass::Cancelled,
            Some(RepositoryStopEvidence {
                reason: RepositoryStopReason::OperatorRequested,
                signal: Some(RepositorySignalClass::Interrupt),
            }),
            None,
            PersonalWorkerRepositoryTerminalClass::Cancelled,
        ),
        (
            RepositoryProcessTerminalClass::ResourceExhausted,
            None,
            Some(RepositoryResourceExhaustionEvidence {
                class: RepositoryResourceExhaustionClass::Memory,
                observation_digest: digest('a'),
            }),
            PersonalWorkerRepositoryTerminalClass::ResourceExhausted,
        ),
    ] {
        let mut absent_outer = outer();
        absent_outer.process_terminal = process_terminal;
        absent_outer.stop = stop;
        absent_outer.resource_exhaustion = resource_exhaustion;
        absent_outer.receipt_channel = RepositoryReceiptChannelObservation::Empty {
            channel_id: channel_id(),
        };
        let completion = correlate_personal_worker_repository_result(
            binding(),
            absent_outer,
            RepositoryReceiptAssessment::Missing,
        )
        .expect("typed outer terminal survives absent receipt");
        assert_eq!(completion.terminal_class(), expected);
        assert!(matches!(
            completion.repository_receipt(),
            RepositoryReceiptAssessment::Missing
        ));
    }
}

#[test]
fn timeout_and_cancellation_require_matching_outer_stop_evidence() {
    let deadline_stop = RepositoryStopEvidence {
        reason: RepositoryStopReason::DeadlineExceeded,
        signal: Some(RepositorySignalClass::Terminate),
    };
    let mut timeout_receipt = receipt();
    timeout_receipt.terminal_class = RepositoryReceiptTerminalClass::Timeout;
    timeout_receipt.stop = Some(deadline_stop);
    timeout_receipt.work_detail = RepositoryWorkDetail::WorkUnits {
        work_units: work_units(15, RepositoryWorkUnitOutcome::TimedOut),
    };
    let mut timeout_outer = outer();
    timeout_outer.process_terminal = RepositoryProcessTerminalClass::Timeout;
    timeout_outer.stop = Some(deadline_stop);
    assert_eq!(
        correlate_personal_worker_repository_result(
            binding(),
            timeout_outer,
            RepositoryReceiptAssessment::present(timeout_receipt),
        )
        .expect("timeout")
        .terminal_class(),
        PersonalWorkerRepositoryTerminalClass::Timeout
    );

    let cancel_stop = RepositoryStopEvidence {
        reason: RepositoryStopReason::OperatorRequested,
        signal: Some(RepositorySignalClass::Interrupt),
    };
    let mut cancelled_receipt = receipt();
    cancelled_receipt.terminal_class = RepositoryReceiptTerminalClass::Cancelled;
    cancelled_receipt.stop = Some(cancel_stop);
    cancelled_receipt.work_detail = RepositoryWorkDetail::WorkUnits {
        work_units: work_units(15, RepositoryWorkUnitOutcome::Cancelled),
    };
    let mut cancelled_outer = outer();
    cancelled_outer.process_terminal = RepositoryProcessTerminalClass::Cancelled;
    cancelled_outer.stop = Some(cancel_stop);
    assert_eq!(
        correlate_personal_worker_repository_result(
            binding(),
            cancelled_outer,
            RepositoryReceiptAssessment::present(cancelled_receipt),
        )
        .expect("cancelled")
        .terminal_class(),
        PersonalWorkerRepositoryTerminalClass::Cancelled
    );

    let mut mismatch_receipt = receipt();
    mismatch_receipt.terminal_class = RepositoryReceiptTerminalClass::Timeout;
    mismatch_receipt.stop = Some(deadline_stop);
    let mut mismatch_outer = outer();
    mismatch_outer.process_terminal = RepositoryProcessTerminalClass::Timeout;
    mismatch_outer.stop = Some(RepositoryStopEvidence {
        reason: RepositoryStopReason::DeadlineExceeded,
        signal: Some(RepositorySignalClass::Kill),
    });
    assert_eq!(
        correlate_personal_worker_repository_result(
            binding(),
            mismatch_outer,
            RepositoryReceiptAssessment::present(mismatch_receipt),
        )
        .expect_err("stop evidence changed")
        .kind(),
        PersonalWorkerRepositoryResultErrorKind::CancellationMismatch
    );
}

#[test]
fn resource_exhaustion_requires_matching_reviewed_evidence() {
    let evidence = RepositoryResourceExhaustionEvidence {
        class: RepositoryResourceExhaustionClass::Memory,
        observation_digest: digest('a'),
    };
    let mut exhausted_receipt = receipt();
    exhausted_receipt.terminal_class = RepositoryReceiptTerminalClass::ResourceExhausted;
    exhausted_receipt.resource_exhaustion = Some(evidence.clone());
    exhausted_receipt.work_detail = RepositoryWorkDetail::WorkUnits {
        work_units: work_units(15, RepositoryWorkUnitOutcome::ResourceExhausted),
    };
    let mut exhausted_outer = outer();
    exhausted_outer.process_terminal = RepositoryProcessTerminalClass::ResourceExhausted;
    exhausted_outer.resource_exhaustion = Some(evidence);
    let completion = correlate_personal_worker_repository_result(
        binding(),
        exhausted_outer,
        RepositoryReceiptAssessment::present(exhausted_receipt),
    )
    .expect("corroborated exhaustion");
    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::ResourceExhausted
    );

    let mut changed_receipt = receipt();
    changed_receipt.terminal_class = RepositoryReceiptTerminalClass::ResourceExhausted;
    changed_receipt.resource_exhaustion = Some(RepositoryResourceExhaustionEvidence {
        class: RepositoryResourceExhaustionClass::Memory,
        observation_digest: digest('b'),
    });
    let mut changed_outer = outer();
    changed_outer.process_terminal = RepositoryProcessTerminalClass::ResourceExhausted;
    changed_outer.resource_exhaustion = Some(RepositoryResourceExhaustionEvidence {
        class: RepositoryResourceExhaustionClass::Memory,
        observation_digest: digest('a'),
    });
    assert_eq!(
        correlate_personal_worker_repository_result(
            binding(),
            changed_outer,
            RepositoryReceiptAssessment::present(changed_receipt),
        )
        .expect_err("uncorroborated exhaustion")
        .kind(),
        PersonalWorkerRepositoryResultErrorKind::ResourceMismatch
    );
}

#[test]
fn work_detail_is_bounded_unique_and_inside_the_aggregate_timeline() {
    let mut duplicate = receipt();
    let RepositoryWorkDetail::WorkUnits { work_units } = &mut duplicate.work_detail else {
        unreachable!("fixture retains work units")
    };
    work_units[1].id = work_units[0].id.clone();
    assert_receipt_error(
        duplicate,
        PersonalWorkerRepositoryResultErrorKind::InvalidWorkDetail,
    );

    let mut outside = receipt();
    let RepositoryWorkDetail::WorkUnits { work_units } = &mut outside.work_detail else {
        unreachable!("fixture retains work units")
    };
    work_units[0].wall_millis = 381;
    assert_receipt_error(
        outside,
        PersonalWorkerRepositoryResultErrorKind::InvalidWorkDetail,
    );

    let mut false_success = receipt();
    let RepositoryWorkDetail::WorkUnits { work_units } = &mut false_success.work_detail else {
        unreachable!("fixture retains work units")
    };
    work_units[0].outcome = RepositoryWorkUnitOutcome::Failed;
    assert_receipt_error(
        false_success,
        PersonalWorkerRepositoryResultErrorKind::TerminalMismatch,
    );

    let mut digest_only = receipt();
    digest_only.work_detail = RepositoryWorkDetail::DetailDigest {
        work_unit_count: 15,
        detail_digest: digest('9'),
    };
    correlate_receipt(digest_only).expect("bounded repository-owned detail digest");
}

#[test]
fn all_repository_failure_classes_remain_one_outer_attempt() {
    for (repository_terminal, process_terminal, expected) in [
        (
            RepositoryReceiptTerminalClass::VerificationFailed,
            RepositoryProcessTerminalClass::ExitedFailure,
            PersonalWorkerRepositoryTerminalClass::RepositoryVerificationFailed,
        ),
        (
            RepositoryReceiptTerminalClass::CompileSetupFailed,
            RepositoryProcessTerminalClass::ExitedFailure,
            PersonalWorkerRepositoryTerminalClass::CompileSetupFailed,
        ),
        (
            RepositoryReceiptTerminalClass::DiagnosticInconclusive,
            RepositoryProcessTerminalClass::DiagnosticInconclusive,
            PersonalWorkerRepositoryTerminalClass::DiagnosticInconclusive,
        ),
    ] {
        let mut failed_receipt = receipt();
        failed_receipt.terminal_class = repository_terminal;
        failed_receipt.work_detail = RepositoryWorkDetail::WorkUnits {
            work_units: work_units(15, RepositoryWorkUnitOutcome::Inconclusive),
        };
        let mut failed_outer = outer();
        failed_outer.process_terminal = process_terminal;
        let completion = correlate_personal_worker_repository_result(
            binding(),
            failed_outer,
            RepositoryReceiptAssessment::present(failed_receipt),
        )
        .expect("typed failure");
        assert_eq!(completion.terminal_class(), expected);
        assert_eq!(completion.binding().request_id().as_str(), "request-1");
        assert_eq!(completion.binding().attempt_generation().get(), 7);
    }
}

#[test]
fn completion_replay_is_exact_idempotent_stale_or_conflicting() {
    let completion = correlate_receipt(receipt()).expect("completion");
    assert_eq!(
        classify_repository_completion_replay(
            PersonalWorkerStoreRevision::new(10).expect("revision"),
            PersonalWorkerQueueGeneration::new(11).expect("generation"),
            None,
            &completion,
        )
        .expect("new completion"),
        RepositoryCompletionReplayDisposition::New
    );
    assert_eq!(
        classify_repository_completion_replay(
            PersonalWorkerStoreRevision::new(99).expect("advanced revision"),
            PersonalWorkerQueueGeneration::new(99).expect("advanced generation"),
            Some(&completion),
            &completion,
        )
        .expect("response-loss retry"),
        RepositoryCompletionReplayDisposition::ExactReplay
    );
    assert_eq!(
        classify_repository_completion_replay(
            PersonalWorkerStoreRevision::new(9).expect("stale revision"),
            PersonalWorkerQueueGeneration::new(11).expect("generation"),
            None,
            &completion,
        )
        .expect_err("stale predecessor")
        .kind(),
        PersonalWorkerRepositoryResultErrorKind::StalePredecessor
    );

    let mut changed_outer = outer();
    changed_outer.outer_cleanup = RepositoryCleanupDisposition::Incomplete;
    let changed = correlate_personal_worker_repository_result(
        binding(),
        changed_outer,
        RepositoryReceiptAssessment::present(receipt()),
    )
    .expect("changed completion");
    assert_eq!(
        classify_repository_completion_replay(
            PersonalWorkerStoreRevision::new(10).expect("revision"),
            PersonalWorkerQueueGeneration::new(11).expect("generation"),
            Some(&completion),
            &changed,
        )
        .expect_err("changed replay")
        .kind(),
        PersonalWorkerRepositoryResultErrorKind::ChangedReplay
    );
}

#[test]
fn identifiers_are_bounded_and_opaque() {
    assert!(RepositoryVerifierProducerId::parse("../producer").is_err());
    assert!(RepositoryWorkUnitId::parse("work/unit").is_err());
    assert!(PersonalWorkerJobAttemptId::parse("attempt-1").is_err());
    assert!(
        PersonalWorkerJobAttemptId::parse(&format!("pw-job-attempt-v1-{}", "A".repeat(64)))
            .is_err()
    );
    assert!(RepositoryReceiptChannelId::parse("channel-1").is_err());
    assert_eq!(
        format!("{:?}", attempt_id()),
        "PersonalWorkerJobAttemptId(<opaque>)"
    );
    assert_eq!(
        format!("{:?}", channel_id()),
        "RepositoryReceiptChannelId(<opaque>)"
    );
}

#[test]
fn correlation_module_stays_pure_and_path_free() {
    let source = include_str!("../src/personal_worker_repository_result.rs");
    for forbidden in [
        "std::process",
        "std::fs",
        "std::net",
        "unsafe {",
        "Command::",
        "OpenOptions",
        "/tmp/",
        "sh -c",
    ] {
        assert!(
            !source.contains(forbidden),
            "pure correlation module contains forbidden execution primitive: {forbidden}"
        );
    }
}
