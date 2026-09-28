#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
    DisposableAttemptReservation, decode_disposable_attempt_catalog,
    encode_disposable_attempt_catalog,
};
use glaeda::disposable_attempt_state::{
    DisposableAttemptState, decode_disposable_attempt_state, encode_disposable_attempt_state,
};
use glaeda::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
};
use glaeda::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableVmIdentity, DisposableVmObservation, DisposableWorkerAction,
    DisposableWorkerReconcileInput, DisposableWorkerResources, ScaleSetRunnerObservation,
    reconcile_attempt,
};
use glaeda::execution_admission::EpochMillis;
use glaeda::github_scale_set_protocol::{
    ScaleSetJobEvent, ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference, ScaleSetRunnerRequestId,
};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-durable-reconcile-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("private state root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn epoch(value: u64) -> EpochMillis {
    EpochMillis::new(value).unwrap()
}

fn template_digest() -> DisposablePreparedTemplateIdentity {
    current_disposable_prepared_template()
        .unwrap()
        .identity()
        .unwrap()
}

fn attempt_id() -> DisposableAttemptId {
    DisposableAttemptId::parse("attempt-restart-1").unwrap()
}

fn vm_identity() -> &'static DisposableVmIdentity {
    static BOUND: LazyLock<DisposableAttemptState> = LazyLock::new(|| {
        bind_attempt_fixture(
            DisposableAttemptState::reserved(
                attempt_id(),
                CapacityClaimId::parse("claim-restart-1").unwrap(),
                DisposableVmId::parse("vm-restart-1").unwrap(),
                ScaleSetRunnerName::parse("smol-attempt-restart-1").unwrap(),
                ScaleSetRunnerRequestId::new(41).unwrap(),
                epoch(100_000),
            )
            .authorize_clone()
            .unwrap()
            .record_clone_started()
            .unwrap(),
        )
    });
    BOUND.vm_identity().unwrap()
}

fn bind_attempt_fixture(state: DisposableAttemptState) -> DisposableAttemptState {
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
            "22".repeat(32)
        ),
        1,
    );
    decode_disposable_attempt_state(encoded.as_bytes()).unwrap()
}

fn bind_current_catalog_fixture(root: &Path) {
    let path = root
        .join("personal-worker")
        .join("disposable-attempt-catalog.json");
    let current_bytes = fs::read(&path).unwrap();
    let current = decode_disposable_attempt_catalog(&current_bytes).unwrap();
    let attempt = current.find_active(&attempt_id()).unwrap().attempt();
    let bound_attempt = bind_attempt_fixture(attempt.clone());
    let current_attempt_value: serde_json::Value =
        serde_json::from_slice(&encode_disposable_attempt_state(attempt).unwrap()).unwrap();
    let current_attempt_json = serde_json::to_string(&current_attempt_value).unwrap();
    let bound_attempt_value: serde_json::Value =
        serde_json::from_slice(&encode_disposable_attempt_state(&bound_attempt).unwrap()).unwrap();
    let bound_attempt_json = serde_json::to_string(&bound_attempt_value).unwrap();
    let mut encoded = String::from_utf8(current_bytes).unwrap();
    encoded = encoded.replacen(
        &format!("\"revision\":{}", current.revision().get()),
        &format!("\"revision\":{}", current.revision().get() + 1),
        1,
    );
    encoded = encoded.replacen(&current_attempt_json, &bound_attempt_json, 1);
    let bound = decode_disposable_attempt_catalog(encoded.as_bytes()).unwrap();
    fs::write(path, encode_disposable_attempt_catalog(&bound).unwrap()).unwrap();
}

fn runner() -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(71).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-restart-1").unwrap(),
    )
}

fn initialize(root: &Path) -> DisposableAttemptCatalogDocument {
    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root).unwrap();
    let mut catalog = DisposableAttemptCatalog::new(store);
    let (empty, _) = catalog.initialize().unwrap();
    let state = DisposableAttemptState::reserved(
        attempt_id(),
        CapacityClaimId::parse("claim-restart-1").unwrap(),
        DisposableVmId::parse("vm-restart-1").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-restart-1").unwrap(),
        ScaleSetRunnerRequestId::new(41).unwrap(),
        epoch(100_000),
    );
    catalog
        .reserve(
            empty.revision(),
            DisposableAttemptReservation::new(
                state,
                DisposableWorkerResources::new(2_000, 4_000, 8_000).unwrap(),
                template_digest(),
            )
            .unwrap(),
        )
        .unwrap()
        .0
}

fn tick(
    root: &Path,
    vm: DisposableVmObservation,
    runner_observation: ScaleSetRunnerObservation,
    job_event: Option<ScaleSetJobEvent>,
    capacity_reserved: bool,
) -> (DisposableAttemptCatalogDocument, DisposableWorkerAction) {
    tick_with_cancellation(
        root,
        vm,
        runner_observation,
        job_event,
        capacity_reserved,
        false,
    )
}

fn tick_with_cancellation(
    root: &Path,
    vm: DisposableVmObservation,
    runner_observation: ScaleSetRunnerObservation,
    job_event: Option<ScaleSetJobEvent>,
    capacity_reserved: bool,
    cancellation_requested: bool,
) -> (DisposableAttemptCatalogDocument, DisposableWorkerAction) {
    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root).unwrap();
    let mut catalog = DisposableAttemptCatalog::new(store);
    let current = catalog.load().unwrap();
    let reservation = current.find_active(&attempt_id()).unwrap();
    let state = reservation.attempt().clone();
    let action = reconcile_attempt(DisposableWorkerReconcileInput {
        now: epoch(1_000),
        attempt: &state,
        vm,
        vm_identity: matches!(
            vm,
            DisposableVmObservation::Stopped | DisposableVmObservation::Ready
        )
        .then(vm_identity),
        runner: runner_observation,
        job_event,
        capacity_reserved,
        cancellation_requested,
    })
    .unwrap();
    let next = match &action {
        DisposableWorkerAction::Persist { transition } => {
            catalog
                .transition(
                    current.revision(),
                    state.attempt_id(),
                    state.revision(),
                    transition.clone(),
                )
                .unwrap()
                .0
        }
        DisposableWorkerAction::CheckpointAndCloneVm => {
            catalog
                .transition(
                    current.revision(),
                    state.attempt_id(),
                    state.revision(),
                    DisposableAttemptCatalogAction::RecordCloneStarted,
                )
                .unwrap()
                .0
        }
        DisposableWorkerAction::CheckpointAndGenerateJit => {
            catalog
                .transition(
                    current.revision(),
                    state.attempt_id(),
                    state.revision(),
                    DisposableAttemptCatalogAction::RecordJitGenerationStarted,
                )
                .unwrap()
                .0
        }
        _ => current,
    };
    drop(catalog);

    let reopened = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root).unwrap();
    let reopened = DisposableAttemptCatalog::new(reopened).load().unwrap();
    assert_eq!(
        reopened, next,
        "every tick must survive a controller restart"
    );
    (reopened, action)
}

#[test]
fn every_durable_checkpoint_reopens_without_duplicate_capacity_or_cleanup_loss() {
    let root = TempRoot::new();
    let initial = initialize(root.path());
    assert_eq!(initial.host_usage().unwrap().workers(), 1);

    let (state, action) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert!(matches!(action, DisposableWorkerAction::Persist { .. }));
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::CloneAuthorized
    );

    let (state, action) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(action, DisposableWorkerAction::CheckpointAndCloneVm);
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::CloneStarted
    );
    bind_current_catalog_fixture(root.path());
    let state = DisposableAttemptCatalog::new(
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap(),
    )
    .load()
    .unwrap();
    assert!(
        state
            .find_active(&attempt_id())
            .unwrap()
            .attempt()
            .vm_identity()
            .is_some()
    );

    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Stopped,
            ScaleSetRunnerObservation::Absent,
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::DiscardIncompleteVm);
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Registering
    );
    let (jit_state, action) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(action, DisposableWorkerAction::CheckpointAndGenerateJit);
    let attempt = jit_state.find_active(&attempt_id()).unwrap().attempt();
    assert!(attempt.jit_generation_started());
    assert!(!attempt.runner_start_started());
    let (_, action) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(
        action,
        DisposableWorkerAction::Blocked {
            code: "jit_generation_outcome_unknown"
        }
    );

    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
    let mut catalog = DisposableAttemptCatalog::new(store);
    let current = catalog.load().unwrap();
    let attempt = current.find_active(&attempt_id()).unwrap().attempt();
    let registered = catalog
        .transition(
            current.revision(),
            attempt.attempt_id(),
            attempt.revision(),
            DisposableAttemptCatalogAction::RecordRegistration(runner()),
        )
        .unwrap()
        .0;
    let attempt = registered.find_active(&attempt_id()).unwrap().attempt();
    let runner_started = catalog
        .transition(
            registered.revision(),
            attempt.attempt_id(),
            attempt.revision(),
            DisposableAttemptCatalogAction::RecordRunnerStartStarted,
        )
        .unwrap()
        .0;
    let attempt = runner_started.find_active(&attempt_id()).unwrap().attempt();
    assert!(attempt.jit_generation_started());
    assert!(attempt.runner_start_started());
    drop(catalog);

    let exact_runner = runner();
    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Waiting
    );

    let exact_job = ScaleSetJobId::parse("opaque-job/restart-1").unwrap();
    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
        Some(ScaleSetJobEvent::Started {
            runner: exact_runner.clone(),
            job_id: exact_job.clone(),
        }),
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Running
    );

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner,
        },
        Some(ScaleSetJobEvent::Completed {
            runner: Some(runner()),
            job_id: exact_job,
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        }),
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Terminal
    );

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Destroying
    );
    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Ready,
            ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::DestroyVm);
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Deregistering
    );
    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Absent,
            ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
            None,
            true,
        );
        assert_eq!(
            action,
            DisposableWorkerAction::DeleteRunner { runner: runner() }
        );
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Releasing
    );
    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Absent,
            ScaleSetRunnerObservation::Absent,
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::ReleaseCapacity);
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Complete
    );

    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
    let catalog = DisposableAttemptCatalog::new(store).load().unwrap();
    assert_eq!(catalog.host_usage().unwrap().workers(), 0);
}

#[test]
fn lost_reserved_capacity_completes_without_acquiring_vm_cleanup_authority() {
    let root = TempRoot::new();
    initialize(root.path());

    let (catalog, action) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert!(matches!(
        action,
        DisposableWorkerAction::Persist {
            transition: glaeda::disposable_attempt_catalog::DisposableAttemptCatalogAction::CompleteUnprovisioned
        }
    ));
    let attempt = catalog.find_active(&attempt_id()).unwrap().attempt();
    assert_eq!(attempt.phase(), DisposableAttemptPhase::Complete);
    assert_eq!(attempt.revision().get(), 2);
    assert!(attempt.runner_id().is_none());
    assert!(attempt.github_job_id().is_none());
    assert_eq!(catalog.host_usage().unwrap().workers(), 0);

    let (_, replay) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(
        replay,
        DisposableWorkerAction::Blocked {
            code: "completed_attempt_retains_external_state"
        }
    );

    let (_, absent_replay) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(absent_replay, DisposableWorkerAction::NoOp);
}

#[test]
fn reserved_cancellation_survives_restart_before_capacity_release() {
    let root = TempRoot::new();
    initialize(root.path());

    let (catalog, checkpoint) = tick_with_cancellation(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
        true,
    );
    assert!(matches!(
        checkpoint,
        DisposableWorkerAction::Persist {
            transition: glaeda::disposable_attempt_catalog::DisposableAttemptCatalogAction::BeginUnprovisionedRelease
        }
    ));
    assert_eq!(
        catalog
            .find_active(&attempt_id())
            .unwrap()
            .attempt()
            .phase(),
        DisposableAttemptPhase::UnprovisionedReleasing
    );

    // The transient cancellation input is deliberately absent after reopening.
    let (_, release) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(release, DisposableWorkerAction::ReleaseCapacity);

    let (complete, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(
        complete
            .find_active(&attempt_id())
            .unwrap()
            .attempt()
            .phase(),
        DisposableAttemptPhase::Complete
    );
    assert_eq!(complete.host_usage().unwrap().workers(), 0);
}

#[test]
fn stale_registration_is_bound_before_delete_and_recovery_never_uses_name_alone() {
    let root = TempRoot::new();
    initialize(root.path());

    tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    bind_current_catalog_fixture(root.path());
    tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );

    let (catalog, action) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    assert!(matches!(
        action,
        DisposableWorkerAction::Persist {
            transition:
                glaeda::disposable_attempt_catalog::DisposableAttemptCatalogAction::RecordRegistration(_)
        }
    ));
    assert_eq!(
        catalog
            .find_active(&attempt_id())
            .unwrap()
            .attempt()
            .runner_id()
            .unwrap()
            .get(),
        71
    );

    tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );

    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Absent,
            ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
            None,
            true,
        );
        assert_eq!(
            action,
            DisposableWorkerAction::DeleteRunner { runner: runner() }
        );
    }

    let (catalog, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    let state = catalog.find_active(&attempt_id()).unwrap().attempt();
    assert_eq!(state.phase(), DisposableAttemptPhase::Releasing);

    let replacement = ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(72).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-restart-1").unwrap(),
    );
    assert_eq!(
        reconcile_attempt(DisposableWorkerReconcileInput {
            now: epoch(1_000),
            attempt: state,
            vm: DisposableVmObservation::Absent,
            vm_identity: None,
            runner: ScaleSetRunnerObservation::RegistrationOnly {
                runner: replacement,
            },
            job_event: None,
            capacity_reserved: true,
            cancellation_requested: false,
        })
        .unwrap_err()
        .code(),
        "runner_identity_drift"
    );
}
