use glaeda::disposable_attempt_state::{
    DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION, DisposableAttemptState,
    decode_disposable_attempt_state, encode_disposable_attempt_state,
};
use glaeda::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
};
use glaeda::execution_admission::EpochMillis;
use glaeda::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference, ScaleSetRunnerRequestId,
};

fn reserved() -> DisposableAttemptState {
    DisposableAttemptState::reserved(
        DisposableAttemptId::parse("attempt-1").unwrap(),
        CapacityClaimId::parse("claim-1").unwrap(),
        DisposableVmId::parse("vm-attempt-1").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-1").unwrap(),
        ScaleSetRunnerRequestId::new(41).unwrap(),
        EpochMillis::new(50_000).unwrap(),
    )
}

fn bind_vm_fixture(state: DisposableAttemptState) -> DisposableAttemptState {
    assert_eq!(state.phase(), DisposableAttemptPhase::CloneStarted);
    assert!(state.vm_identity().is_none());
    let revision = state.revision().get();
    let mut encoded = String::from_utf8(encode_disposable_attempt_state(&state).unwrap()).unwrap();
    encoded = encoded.replacen(
        &format!("\"revision\":{revision}"),
        &format!("\"revision\":{}", revision + 1),
        1,
    );
    encoded = encoded.replacen(
        &format!("\"vm_id\":\"{}\"", state.vm_id().as_str()),
        &format!(
            "\"vm_id\":\"{}\",\"vm_identity_digest\":\"sha256:{}\"",
            state.vm_id().as_str(),
            "33".repeat(32)
        ),
        1,
    );
    decode_disposable_attempt_state(encoded.as_bytes()).unwrap()
}

trait BindVmFixture {
    fn bind_vm_fixture(self) -> Self;
}

impl BindVmFixture for DisposableAttemptState {
    fn bind_vm_fixture(self) -> Self {
        bind_vm_fixture(self)
    }
}

#[test]
fn unprovisioned_completion_skips_cleanup_only_before_clone_start() {
    let complete = reserved().complete_unprovisioned().unwrap();

    assert_eq!(complete.phase(), DisposableAttemptPhase::Complete);
    assert_eq!(complete.revision().get(), 2);
    assert!(complete.runner_id().is_none());
    assert!(complete.github_job_id().is_none());
    assert!(complete.result().is_none());
    assert_eq!(
        reserved()
            .authorize_clone()
            .unwrap()
            .record_clone_started()
            .unwrap()
            .complete_unprovisioned()
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
}

#[test]
fn bound_vm_identity_survives_codec_and_has_no_public_first_binding_transition() {
    let started = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap();
    let bound = bind_vm_fixture(started);
    assert_eq!(bound.phase(), DisposableAttemptPhase::CloneStarted);
    assert_eq!(bound.revision().get(), 4);
    assert!(bound.vm_identity().is_some());

    let encoded = encode_disposable_attempt_state(&bound).unwrap();
    assert_eq!(decode_disposable_attempt_state(&encoded).unwrap(), bound);
}

fn runner(id: u64) -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(id).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-1").unwrap(),
    )
}

fn job(value: &str) -> ScaleSetJobId {
    ScaleSetJobId::parse(value).unwrap()
}

#[test]
fn registration_and_listener_readiness_are_distinct_durable_checkpoints() {
    let registering = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap();
    let jit_started = registering.record_jit_generation_started().unwrap();
    let registered = jit_started.record_registration(&runner(41)).unwrap();

    assert_eq!(registered.phase(), DisposableAttemptPhase::Registering);
    assert_eq!(registered.runner_id().unwrap().get(), 41);
    assert!(registered.github_job_id().is_none());

    let runner_started = registered.record_runner_start_started().unwrap();
    let ready = runner_started.record_runner_ready(&runner(41)).unwrap();
    assert_eq!(ready.phase(), DisposableAttemptPhase::Waiting);
    assert_eq!(ready.runner_id().unwrap().get(), 41);
    assert!(ready.revision().get() > registered.revision().get());
}

#[test]
fn jit_generation_and_runner_start_are_distinct_no_replay_checkpoints() {
    let registering = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap();
    assert!(!registering.jit_generation_started());
    assert!(!registering.runner_start_started());

    let jit_started = registering.record_jit_generation_started().unwrap();
    assert!(jit_started.jit_generation_started());
    assert!(!jit_started.runner_start_started());
    assert_eq!(
        jit_started.record_jit_generation_started().unwrap(),
        jit_started
    );
    assert_eq!(
        jit_started
            .record_runner_start_started()
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
    assert_eq!(
        jit_started
            .record_runner_ready(&runner(41))
            .unwrap_err()
            .code(),
        "invalid_transition"
    );

    let registered = jit_started.record_registration(&runner(41)).unwrap();
    let runner_started = registered.record_runner_start_started().unwrap();
    assert!(runner_started.jit_generation_started());
    assert!(runner_started.runner_start_started());
    assert_eq!(
        runner_started.record_runner_start_started().unwrap(),
        runner_started
    );

    let pre_jit_cleanup = registering
        .begin_cleanup()
        .unwrap()
        .advance_cleanup(DisposableAttemptPhase::Deregistering)
        .unwrap();
    assert_eq!(
        pre_jit_cleanup
            .record_registration(&runner(41))
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
    assert_eq!(
        pre_jit_cleanup
            .record_terminal(
                Some(&runner(41)),
                job("canceled-before-jit"),
                ScaleSetJobResult::parse("canceled").unwrap(),
            )
            .unwrap_err()
            .code(),
        "invalid_document"
    );
}

#[test]
fn job_assignment_does_not_bind_a_runner_before_job_started() {
    let assigned = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap()
        .record_assigned(job("job_opaque-7"))
        .unwrap();

    assert_eq!(assigned.phase(), DisposableAttemptPhase::Assigned);
    assert!(assigned.runner_id().is_none());
    assert_eq!(assigned.github_job_id().unwrap().as_str(), "job_opaque-7");

    let running = assigned
        .record_jit_generation_started()
        .unwrap()
        .record_registration(&runner(9))
        .unwrap()
        .record_runner_start_started()
        .unwrap()
        .record_running(&runner(9), job("job_opaque-7"))
        .unwrap();
    assert_eq!(running.phase(), DisposableAttemptPhase::Running);
    assert_eq!(running.runner_id().unwrap().get(), 9);
    assert_eq!(running.github_job_id().unwrap().as_str(), "job_opaque-7");
}

#[test]
fn runnerless_completion_requires_an_exact_prebound_job() {
    assert_eq!(
        reserved()
            .record_terminal(
                None,
                job("cancel-before-runner"),
                ScaleSetJobResult::parse("canceled").unwrap(),
            )
            .unwrap_err()
            .code(),
        "identity_drift"
    );

    let terminal = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap()
        .record_assigned(job("cancel-before-runner"))
        .unwrap()
        .record_terminal(
            None,
            job("cancel-before-runner"),
            ScaleSetJobResult::parse("canceled").unwrap(),
        )
        .unwrap();

    assert_eq!(terminal.phase(), DisposableAttemptPhase::Terminal);
    assert!(terminal.runner_id().is_none());
    assert_eq!(
        terminal.github_job_id().unwrap().as_str(),
        "cancel-before-runner"
    );
    assert_eq!(terminal.result().unwrap().as_str(), "canceled");
}

#[test]
fn preauthorization_terminal_evidence_cannot_create_cleanup_authority() {
    assert_eq!(
        reserved()
            .record_terminal(
                Some(&runner(9)),
                job("preauthorization-terminal"),
                ScaleSetJobResult::parse("canceled").unwrap(),
            )
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
}

#[test]
fn unknown_completion_result_still_reaches_cleanup() {
    let terminal = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap()
        .record_jit_generation_started()
        .unwrap()
        .record_registration(&runner(41))
        .unwrap()
        .record_runner_start_started()
        .unwrap()
        .record_assigned(job("job-future-result"))
        .unwrap()
        .record_terminal(
            None,
            job("job-future-result"),
            ScaleSetJobResult::parse("future-service-result").unwrap(),
        )
        .unwrap();
    let destroying = terminal.begin_cleanup().unwrap();
    let deregistering = destroying
        .advance_cleanup(DisposableAttemptPhase::Deregistering)
        .unwrap();
    let releasing = deregistering
        .advance_cleanup(DisposableAttemptPhase::Releasing)
        .unwrap();
    let complete = releasing
        .advance_cleanup(DisposableAttemptPhase::Complete)
        .unwrap();

    assert_eq!(complete.phase(), DisposableAttemptPhase::Complete);
    assert_eq!(complete.result().unwrap().as_str(), "future-service-result");
}

#[test]
fn late_job_evidence_binds_without_reversing_cleanup_and_conflicts_fail_closed() {
    let exact_runner = runner(41);
    let destroying = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap()
        .record_jit_generation_started()
        .unwrap()
        .record_registration(&exact_runner)
        .unwrap()
        .record_runner_start_started()
        .unwrap()
        .begin_cleanup()
        .unwrap()
        .record_running(&exact_runner, job("late-job"))
        .unwrap();
    assert_eq!(destroying.phase(), DisposableAttemptPhase::Destroying);
    assert_eq!(destroying.runner_id().unwrap().get(), 41);
    assert_eq!(destroying.github_job_id().unwrap().as_str(), "late-job");

    let terminal = destroying
        .record_terminal(
            Some(&exact_runner),
            job("late-job"),
            ScaleSetJobResult::parse("succeeded").unwrap(),
        )
        .unwrap();
    assert_eq!(terminal.phase(), DisposableAttemptPhase::Destroying);
    assert_eq!(terminal.result().unwrap().as_str(), "succeeded");
    assert_eq!(
        terminal
            .record_terminal(
                Some(&exact_runner),
                job("late-job"),
                ScaleSetJobResult::parse("failed").unwrap(),
            )
            .unwrap_err()
            .code(),
        "identity_drift"
    );
}

#[test]
fn exact_runner_and_job_identity_drift_fails_closed() {
    let registered = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap()
        .record_jit_generation_started()
        .unwrap()
        .record_registration(&runner(7))
        .unwrap();

    assert_eq!(
        registered
            .record_runner_ready(&runner(8))
            .unwrap_err()
            .code(),
        "identity_drift"
    );

    let running = registered
        .record_runner_start_started()
        .unwrap()
        .record_runner_ready(&runner(7))
        .unwrap()
        .record_assigned(job("job-a"))
        .unwrap()
        .record_running(&runner(7), job("job-a"))
        .unwrap();
    assert_eq!(
        running
            .record_terminal(
                Some(&runner(7)),
                job("job-b"),
                ScaleSetJobResult::parse("succeeded").unwrap(),
            )
            .unwrap_err()
            .code(),
        "identity_drift"
    );
}

#[test]
fn canonical_codec_round_trips_exact_state_and_revision() {
    let state = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap()
        .record_jit_generation_started()
        .unwrap()
        .record_registration(&runner(77))
        .unwrap();
    let encoded = encode_disposable_attempt_state(&state).unwrap();
    let decoded = decode_disposable_attempt_state(&encoded).unwrap();

    assert_eq!(decoded, state);
    assert_eq!(
        decoded.schema_version(),
        DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION
    );
    assert_eq!(decoded.revision(), state.revision());
    assert_eq!(decoded.runner_name().as_str(), "smol-attempt-1");
    assert_eq!(decoded.runner_request_id().get(), 41);
    assert_eq!(decoded.runner_id().unwrap().get(), 77);
}

#[test]
fn codec_rejects_future_versions_unknown_fields_and_inconsistent_phase_evidence() {
    let base = encode_disposable_attempt_state(&reserved()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&base).unwrap();

    value["schema_version"] = serde_json::json!(DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION + 1);
    let future = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_state(&future).unwrap_err().code(),
        "version_incompatible"
    );

    value["schema_version"] = serde_json::json!(DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION);
    value["unexpected"] = serde_json::json!(true);
    let unknown = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_state(&unknown)
            .unwrap_err()
            .code(),
        "invalid_document"
    );

    value.as_object_mut().unwrap().remove("unexpected");
    value["schema_version"] = serde_json::json!(4);
    value.as_object_mut().unwrap().remove("runner_request_id");
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .code(),
        "version_incompatible"
    );
    value["schema_version"] = serde_json::json!(5);
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .code(),
        "version_incompatible"
    );

    let mut inconsistent: serde_json::Value = serde_json::from_slice(&base).unwrap();
    inconsistent["phase"] = serde_json::json!("running");
    inconsistent["runner_id"] = serde_json::json!(12);
    let inconsistent = serde_json::to_vec(&inconsistent).unwrap();
    assert_eq!(
        decode_disposable_attempt_state(&inconsistent)
            .unwrap_err()
            .code(),
        "invalid_document"
    );

    let mut impossible_started: serde_json::Value = serde_json::from_slice(&base).unwrap();
    impossible_started["revision"] = serde_json::json!(4);
    impossible_started["phase"] = serde_json::json!("clone_started");
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&impossible_started).unwrap())
            .unwrap_err()
            .code(),
        "invalid_document"
    );

    let registering = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture()
        .begin_registration()
        .unwrap();
    let mut forged: serde_json::Value =
        serde_json::from_slice(&encode_disposable_attempt_state(&registering).unwrap()).unwrap();
    forged["runner_start_started"] = serde_json::json!(true);
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&forged).unwrap())
            .unwrap_err()
            .code(),
        "invalid_document"
    );

    let mut preclone: serde_json::Value = serde_json::from_slice(&base).unwrap();
    preclone["jit_generation_started"] = serde_json::json!(true);
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&preclone).unwrap())
            .unwrap_err()
            .code(),
        "invalid_document"
    );

    let unbound_started = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap();
    let mut unbound_cleanup: serde_json::Value =
        serde_json::from_slice(&encode_disposable_attempt_state(&unbound_started).unwrap())
            .unwrap();
    unbound_cleanup["revision"] = serde_json::json!(4);
    unbound_cleanup["phase"] = serde_json::json!("destroying");
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&unbound_cleanup).unwrap())
            .unwrap_err()
            .code(),
        "invalid_document"
    );
    unbound_cleanup["revision"] = serde_json::json!(5);
    unbound_cleanup["phase"] = serde_json::json!("complete");
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&unbound_cleanup).unwrap())
            .unwrap_err()
            .code(),
        "invalid_document"
    );

    let bound = reserved()
        .authorize_clone()
        .unwrap()
        .record_clone_started()
        .unwrap()
        .bind_vm_fixture();
    let mut impossible_registration: serde_json::Value =
        serde_json::from_slice(&encode_disposable_attempt_state(&bound).unwrap()).unwrap();
    impossible_registration["phase"] = serde_json::json!("registering");
    assert_eq!(
        decode_disposable_attempt_state(&serde_json::to_vec(&impossible_registration).unwrap())
            .unwrap_err()
            .code(),
        "invalid_document"
    );
}
