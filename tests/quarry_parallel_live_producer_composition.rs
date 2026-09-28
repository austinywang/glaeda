//! Live-producer composition: one real Big Red `parallel-full` capture
//! correlated against its real outer execution facts.
//!
//! The sibling `quarry_parallel_verification_adapter_contract` suite proves the
//! adapter against a hand-rolled fixture. This suite proves the same
//! composition against authentic producer bytes and authentic outer
//! observations, closing #1011 items 2-4 physically:
//!
//! * fixture `quarry_parallel_live_receipt_468ccc44.json`: exact stdout bytes
//!   of `python scripts/run_local_tests.py parallel-full --workers 4
//!   --receipt-stdout` at Quarry commit
//!   `468ccc442dd6be737d1cb6deeafc36d29e02a581`, produced on Big Red
//!   (regenerate with that command; the test pins the toolchain and source
//!   the bytes must carry, so producer drift fails closed);
//! * binding: the exact admitted source, the independently observed
//!   toolchain (`capture_verifier_toolchain`, never the receipt), the
//!   applied 400%/8GiB/512 grant with concurrency 4, and the fixed
//!   producer/schema/byte contract;
//! * outer observation: the real managed-run terminal facts (exit 0,
//!   start/end millis, complete task cleanup, 8,049 captured bytes).
//!
//! Attempt, reservation, and store identity stay experiment-scoped: this
//! proves live-bytes composition, not queue authority. The full worker
//! loop (reservation issuance through the reconciler, B07 publication)
//! remains future work and must not be inferred from these ids.

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
    PersonalWorkerRepositoryAttemptInput, PersonalWorkerRepositoryCompletionInput,
    PersonalWorkerRepositoryTerminalClass, RepositoryCleanupDisposition,
    RepositoryConcurrencyGrant, RepositoryProcessTerminalClass, RepositoryReceiptAssessment,
    RepositoryReceiptChannelId, RepositoryReceiptContract, RepositoryVerifierProducerId,
    bind_personal_worker_repository_attempt,
};
use glaeda::personal_worker_store::PersonalWorkerStoreRevision;
use glaeda::quarry_parallel_verification_adapter::{
    QUARRY_PARALLEL_VERIFICATION_PRODUCER_ID, QuarryParallelVerificationCapture,
    QuarryParallelVerificationOuterObservation, correlate_quarry_parallel_verification,
};
use glaeda::verification_profile::{
    CacheId, RepositoryCommandId, RepositoryCommandIdentity, VerificationProfileId,
};

/// Exact stdout bytes of the live Big Red producer run (8,049 bytes).
const LIVE_RECEIPT: &[u8] = include_bytes!("fixtures/quarry_parallel_live_receipt_468ccc44.json");

const GIB: u64 = 1_024 * 1_024 * 1_024;

const LIVE_COMMIT: &str = "468ccc442dd6be737d1cb6deeafc36d29e02a581";
const LIVE_TREE: &str = "9316a33c1fc1c599158ee113840539a981056a9a";
/// Independently observed via `capture_verifier_toolchain` on the Big Red
/// resident venv (identical under TZ=UTC and host CST); never read from the
/// receipt under test.
const LIVE_TOOLCHAIN_HEX: &str = "42ef015b6cf09754c683b0bfc1add9d5c178f307681a708d057794aea9dc161b";
/// sha256 of the fixed producer argv:
/// `python scripts/run_local_tests.py parallel-full --workers 4 --receipt-stdout`.
const LIVE_COMMAND_DIGEST: &str =
    "7452e294d1780a1cd61e4bb9354529b43b2862efda009f76734148aa31c254e6";
/// Resident closure digest, reused as the experiment cache namespace.
const LIVE_CLOSURE_DIGEST: &str =
    "6dd40c9d3808769ed141ff9be4f373ffe5ea796633e10e75b9ba139fe02c90e0";

/// Real managed-run outer facts (Big Red, 2026-09-11): exit 0, complete
/// task cleanup, 8,049 captured bytes on the bounded channel.
const STARTED_AT_MILLIS: u64 = 1_789_112_392_369;
const TERMINAL_AT_MILLIS: u64 = 1_789_112_480_895;

fn digest(hex: &str) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{hex}")).expect("digest")
}

fn at(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn repository() -> RepositoryRef {
    RepositoryRef::parse("teamleaderleo/quarry").expect("repository")
}

fn attempt_id() -> PersonalWorkerJobAttemptId {
    // Opaque form is fixed: prefix plus 64 lowercase hex. Experiment scope
    // is carried by the request and reservation ids, not here.
    PersonalWorkerJobAttemptId::parse(&format!("pw-job-attempt-v1-{}", "d".repeat(64)))
        .expect("attempt")
}

fn channel_id() -> RepositoryReceiptChannelId {
    RepositoryReceiptChannelId::parse(&format!("repository-receipt-channel-v1-{}", "e".repeat(64)))
        .expect("channel")
}

fn binding_input() -> PersonalWorkerRepositoryAttemptInput {
    PersonalWorkerRepositoryAttemptInput {
        request_id: ExecutionRequestId::parse("big-red-live-quarry-parallel-1").expect("request"),
        attempt_id: attempt_id(),
        attempt_generation: PersonalWorkerJobAttemptGeneration::new(1).expect("attempt generation"),
        predecessor_store_revision: PersonalWorkerStoreRevision::new(1).expect("store revision"),
        predecessor_queue_generation: PersonalWorkerQueueGeneration::new(1)
            .expect("queue generation"),
        source: PersonalWorkerSourceIdentity::new(
            repository(),
            CommitId::parse(LIVE_COMMIT).expect("commit"),
            GitTreeId::parse(LIVE_TREE).expect("tree"),
        ),
        verification_profile_id: VerificationProfileId::parse("quarry.parallel-full-v2")
            .expect("profile"),
        command: RepositoryCommandIdentity::new(
            repository(),
            RepositoryCommandId::parse("parallel-full").expect("command"),
            digest(LIVE_COMMAND_DIGEST),
        ),
        toolchain_envelope_digest: digest(LIVE_TOOLCHAIN_HEX),
        requested_limits: ExecutionResourceLimits::new(16_000, 30 * GIB, 1_024)
            .expect("requested limits"),
        applied_limits: ExecutionResourceLimits::new(4_000, 8 * GIB, 512).expect("applied limits"),
        repository_concurrency: RepositoryConcurrencyGrant::new(4).expect("concurrency"),
        reservation_id: ReservationId::parse("big-red-live-reservation-1").expect("reservation"),
        reservation_generation: ReservationGeneration::new(1).expect("reservation generation"),
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("quarry-parallel").expect("cache id"),
            repository: repository(),
            namespace_digest: digest(LIVE_CLOSURE_DIGEST),
        },
        cache_access: PersonalWorkerCacheAccessMode::Read,
        cache_lease_acquired_at: at(STARTED_AT_MILLIS - 1_000),
        bound_at: at(STARTED_AT_MILLIS),
        // The 900-second applied grant deadline, in epoch millis.
        not_after: at(STARTED_AT_MILLIS + 900_000),
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

fn observation() -> QuarryParallelVerificationOuterObservation<'static> {
    QuarryParallelVerificationOuterObservation {
        request_id: ExecutionRequestId::parse("big-red-live-quarry-parallel-1").expect("request"),
        attempt_id: attempt_id(),
        attempt_generation: PersonalWorkerJobAttemptGeneration::new(1).expect("attempt generation"),
        reservation_id: ReservationId::parse("big-red-live-reservation-1").expect("reservation"),
        reservation_generation: ReservationGeneration::new(1).expect("reservation generation"),
        started_at: at(STARTED_AT_MILLIS),
        terminal_at: at(TERMINAL_AT_MILLIS),
        process_terminal: RepositoryProcessTerminalClass::ExitedSuccess,
        stop: None,
        resource_exhaustion: None,
        outer_cleanup: RepositoryCleanupDisposition::Complete,
        channel_id: channel_id(),
        capture: QuarryParallelVerificationCapture::Bytes(LIVE_RECEIPT),
        aggregate_started_at: at(STARTED_AT_MILLIS),
        // The kernel requires the aggregate window inside the outer process
        // window; the real 88,526 ms span covers the receipt's 84,007 ms.
        aggregate_terminal_at: at(TERMINAL_AT_MILLIS),
        maximum_parallelism_observed: 4,
    }
}

#[test]
fn live_big_red_capture_correlates_to_passed_completion() {
    let binding =
        bind_personal_worker_repository_attempt(binding_input()).expect("live binding binds");
    let completion: PersonalWorkerRepositoryCompletionInput =
        correlate_quarry_parallel_verification(binding, observation()).expect("live correlates");
    assert_eq!(
        completion.terminal_class(),
        PersonalWorkerRepositoryTerminalClass::Passed,
        "live Big Red parallel-full must complete passed"
    );
    let RepositoryReceiptAssessment::Present { receipt } = completion.repository_receipt() else {
        panic!("live Big Red parallel-full must present its aggregate receipt");
    };
    assert_eq!(
        receipt.receipt_digest.as_str(),
        &format!("sha256:{}", {
            use sha2::{Digest as _, Sha256};
            format!("{:x}", Sha256::digest(LIVE_RECEIPT))
        }),
        "completion must bind the exact live captured bytes"
    );
}

#[test]
fn live_binding_refuses_toolchain_drift() {
    // The toolchain half of the binding is observed independently of the
    // receipt. A producer that drifts (new pytest, new closure) must fail
    // closed even when its bytes are otherwise well-formed.
    let mut input = binding_input();
    input.toolchain_envelope_digest = digest(&"0".repeat(64));
    let binding =
        bind_personal_worker_repository_attempt(input).expect("drifted binding still binds");
    let error = correlate_quarry_parallel_verification(binding, observation())
        .expect_err("drifted toolchain must refuse");
    assert_eq!(
        error.kind(),
        glaeda::quarry_parallel_verification_adapter::QuarryParallelVerificationAdapterErrorKind::BindingMismatch,
        "toolchain drift must surface as a binding mismatch"
    );
}
