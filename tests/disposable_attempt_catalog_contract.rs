use glaeda::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptCatalogCodecError,
    DisposableAttemptCatalogCodecErrorKind, DisposableAttemptCatalogDocument,
    DisposableAttemptCatalogError, DisposableAttemptCatalogErrorKind,
    DisposableAttemptCatalogRevision, DisposableAttemptCatalogStore,
    DisposableAttemptCatalogWriteDisposition, DisposableAttemptCatalogWriteReceipt,
    DisposableAttemptReservation, MAX_ACTIVE_DISPOSABLE_ATTEMPTS,
    MemoryDisposableAttemptCatalogStore, decode_disposable_attempt_catalog,
    encode_disposable_attempt_catalog,
};
use glaeda::disposable_attempt_state::{
    DisposableAttemptRevision, DisposableAttemptState, decode_disposable_attempt_state,
    encode_disposable_attempt_state,
};
use glaeda::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
    decode_disposable_prepared_template, encode_disposable_prepared_template,
};
use glaeda::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableWorkerResources,
};
use glaeda::execution_admission::EpochMillis;
use glaeda::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference, ScaleSetRunnerRequestId,
};

type Catalog = DisposableAttemptCatalog<FixtureStore>;

#[derive(Debug, Default)]
struct FixtureStore {
    document: Option<DisposableAttemptCatalogDocument>,
}

impl FixtureStore {
    fn with_document(document: DisposableAttemptCatalogDocument) -> Self {
        Self {
            document: Some(document),
        }
    }
}

impl DisposableAttemptCatalogStore for FixtureStore {
    fn load(
        &self,
    ) -> Result<Option<DisposableAttemptCatalogDocument>, DisposableAttemptCatalogError> {
        Ok(self.document.clone())
    }

    fn create(
        &mut self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        assert!(self.document.is_none());
        self.document = Some(document.clone());
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Created,
            document.revision(),
            None,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: DisposableAttemptCatalogRevision,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        assert_eq!(
            self.document.as_ref().map(|current| current.revision()),
            Some(expected_revision)
        );
        self.document = Some(document.clone());
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Replaced,
            document.revision(),
            None,
        ))
    }
}

fn template_digest() -> DisposablePreparedTemplateIdentity {
    current_disposable_prepared_template()
        .unwrap()
        .identity()
        .unwrap()
}

fn other_template_digest() -> DisposablePreparedTemplateIdentity {
    changed_template_identity()
}

fn try_bind_vm_fixture(
    document: &DisposableAttemptCatalogDocument,
    index: usize,
    identity_index: usize,
) -> Result<DisposableAttemptCatalogDocument, DisposableAttemptCatalogCodecError> {
    let attempt_id = DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap();
    let current_attempt = document.find_active(&attempt_id).unwrap().attempt();
    assert_eq!(
        current_attempt.phase(),
        DisposableAttemptPhase::CloneStarted
    );
    assert!(current_attempt.vm_identity().is_none());

    let current_attempt_json =
        String::from_utf8(encode_disposable_attempt_state(current_attempt).unwrap()).unwrap();
    let mut next_attempt_json = current_attempt_json.clone();
    next_attempt_json = next_attempt_json.replacen(
        &format!("\"revision\":{}", current_attempt.revision().get()),
        &format!("\"revision\":{}", current_attempt.revision().get() + 1),
        1,
    );
    next_attempt_json = next_attempt_json.replacen(
        &format!("\"vm_id\":\"vm-{index}\",\"runner_name\""),
        &format!(
            "\"vm_id\":\"vm-{index}\",\"vm_identity_digest\":\"sha256:{identity_index:064x}\",\"runner_name\""
        ),
        1,
    );
    let bound_attempt = decode_disposable_attempt_state(next_attempt_json.as_bytes()).unwrap();
    let current_attempt_value: serde_json::Value =
        serde_json::from_str(&current_attempt_json).unwrap();
    let current_catalog_attempt_json = serde_json::to_string(&current_attempt_value).unwrap();
    let bound_attempt_value: serde_json::Value =
        serde_json::from_slice(&encode_disposable_attempt_state(&bound_attempt).unwrap()).unwrap();
    let bound_catalog_attempt_json = serde_json::to_string(&bound_attempt_value).unwrap();

    let mut catalog_json =
        String::from_utf8(encode_disposable_attempt_catalog(document).unwrap()).unwrap();
    catalog_json = catalog_json.replacen(
        &format!("\"revision\":{}", document.revision().get()),
        &format!("\"revision\":{}", document.revision().get() + 1),
        1,
    );
    catalog_json = catalog_json.replacen(
        &current_catalog_attempt_json,
        &bound_catalog_attempt_json,
        1,
    );
    decode_disposable_attempt_catalog(catalog_json.as_bytes())
}

fn bind_vm_fixture(
    catalog: &mut Catalog,
    document: &DisposableAttemptCatalogDocument,
    index: usize,
    identity_index: usize,
) -> DisposableAttemptCatalogDocument {
    let bound = try_bind_vm_fixture(document, index, identity_index).unwrap();
    *catalog = Catalog::new(FixtureStore::with_document(bound.clone()));
    bound
}

fn changed_template_identity() -> DisposablePreparedTemplateIdentity {
    let current = current_disposable_prepared_template().unwrap();
    let bytes = encode_disposable_prepared_template(&current).unwrap();
    let revision = current.provisioning_recipe_revision();
    let changed = String::from_utf8(bytes).unwrap().replacen(
        &format!("\"recipe_revision\": {revision}"),
        &format!("\"recipe_revision\": {}", revision + 1),
        1,
    );
    decode_disposable_prepared_template(changed.as_bytes())
        .unwrap()
        .identity()
        .unwrap()
}

fn attempt(index: usize) -> DisposableAttemptState {
    DisposableAttemptState::reserved(
        DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap(),
        CapacityClaimId::parse(&format!("claim-{index}")).unwrap(),
        DisposableVmId::parse(&format!("vm-{index}")).unwrap(),
        ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
        ScaleSetRunnerRequestId::new(1_000 + u64::try_from(index).unwrap()).unwrap(),
        EpochMillis::new(100_000 + u64::try_from(index).unwrap()).unwrap(),
    )
}

fn reservation(index: usize) -> DisposableAttemptReservation {
    reservation_with_digest(index, template_digest())
}

fn reservation_with_digest(
    index: usize,
    prepared_template_digest: DisposablePreparedTemplateIdentity,
) -> DisposableAttemptReservation {
    DisposableAttemptReservation::new(
        attempt(index),
        DisposableWorkerResources::new(1_000, 2 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        prepared_template_digest,
    )
    .unwrap()
}

fn runner(index: usize, id: u64) -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(id).unwrap(),
        ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
    )
}

fn initialized() -> (Catalog, DisposableAttemptCatalogDocument) {
    let mut catalog = DisposableAttemptCatalog::new(FixtureStore::default());
    let (document, receipt) = catalog.initialize().unwrap();
    assert_eq!(
        receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Created
    );
    (catalog, document)
}

fn transition(
    catalog: &mut Catalog,
    document: &DisposableAttemptCatalogDocument,
    index: usize,
    action: DisposableAttemptCatalogAction,
) -> DisposableAttemptCatalogDocument {
    let attempt_id = DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap();
    let attempt_revision = document
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    catalog
        .transition(document.revision(), &attempt_id, attempt_revision, action)
        .unwrap()
        .0
}

fn checkpoint_runner_start(
    catalog: &mut Catalog,
    document: &DisposableAttemptCatalogDocument,
    index: usize,
    runner_id: u64,
) -> DisposableAttemptCatalogDocument {
    let registering = transition(
        catalog,
        document,
        index,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let jit_started = transition(
        catalog,
        &registering,
        index,
        DisposableAttemptCatalogAction::RecordJitGenerationStarted,
    );
    let registered = transition(
        catalog,
        &jit_started,
        index,
        DisposableAttemptCatalogAction::RecordRegistration(runner(index, runner_id)),
    );
    transition(
        catalog,
        &registered,
        index,
        DisposableAttemptCatalogAction::RecordRunnerStartStarted,
    )
}

#[test]
fn initialization_and_exact_duplicate_reservation_are_idempotent() {
    let (mut catalog, empty) = initialized();
    let (same_empty, init_receipt) = catalog.initialize().unwrap();
    assert_eq!(same_empty, empty);
    assert_eq!(
        init_receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );

    let first = reservation(1);
    let (reserved, reserve_receipt) = catalog.reserve(empty.revision(), first.clone()).unwrap();
    assert_eq!(reserved.active().len(), 1);
    assert_eq!(
        reserve_receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Replaced
    );
    assert_eq!(reserve_receipt.attempt_revision.unwrap().get(), 1);

    let (duplicate, duplicate_receipt) = catalog.reserve(reserved.revision(), first).unwrap();
    assert_eq!(duplicate, reserved);
    assert_eq!(
        duplicate_receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );
    assert_eq!(duplicate_receipt.catalog_revision, reserved.revision());

    let changed_template = DisposableAttemptReservation::new(
        attempt(1),
        DisposableWorkerResources::new(1_000, 2 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        other_template_digest(),
    )
    .unwrap();
    assert_eq!(
        catalog
            .reserve(reserved.revision(), changed_template)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogErrorKind::AlreadyExists
    );
}

#[test]
fn raw_store_refuses_unrelated_revision_successors() {
    let (mut first_catalog, first_empty) = initialized();
    let (first_reserved, _) = first_catalog
        .reserve(first_empty.revision(), reservation(1))
        .unwrap();

    let (mut second_catalog, second_empty) = initialized();
    let (second_reserved, _) = second_catalog
        .reserve(second_empty.revision(), reservation(2))
        .unwrap();
    let unrelated = transition(
        &mut second_catalog,
        &second_reserved,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    assert_eq!(
        unrelated.revision().get(),
        first_reserved.revision().get() + 1
    );

    let mut store = MemoryDisposableAttemptCatalogStore::default();
    assert_eq!(
        store
            .create(&first_reserved)
            .expect_err("raw create must accept only the exact empty catalog")
            .kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );
    store.create(&first_empty).unwrap();
    store
        .replace_if_revision(first_empty.revision(), &first_reserved)
        .unwrap();
    let error = store
        .replace_if_revision(first_reserved.revision(), &unrelated)
        .expect_err("revision alone must not authorize an unrelated catalog");
    assert_eq!(error.kind(), DisposableAttemptCatalogErrorKind::Conflict);
    assert_eq!(store.load().unwrap(), Some(first_reserved.clone()));

    let (mut rebound_catalog, rebound_empty) = initialized();
    let (rebound_reserved, _) = rebound_catalog
        .reserve(
            rebound_empty.revision(),
            reservation_with_digest(1, other_template_digest()),
        )
        .unwrap();
    let rebound = transition(
        &mut rebound_catalog,
        &rebound_reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    assert_eq!(
        store
            .replace_if_revision(first_reserved.revision(), &rebound)
            .expect_err("phase advance cannot rebind the prepared-template generation")
            .kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );
    assert_eq!(store.load().unwrap(), Some(first_reserved));
}

#[test]
fn raw_store_refuses_first_vm_identity_binding() {
    let (mut current_catalog, empty) = initialized();
    let (reserved, _) = current_catalog
        .reserve(empty.revision(), reservation(1))
        .unwrap();
    let authorized = transition(
        &mut current_catalog,
        &reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let started = transition(
        &mut current_catalog,
        &authorized,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    assert_eq!(
        current_catalog
            .transition(
                started.revision(),
                &attempt_id,
                started
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::BeginCleanup,
            )
            .expect_err("unbound clone outcome cannot acquire cleanup authority")
            .kind(),
        DisposableAttemptCatalogErrorKind::InvalidAction
    );
    let bound = bind_vm_fixture(&mut current_catalog, &started, 1, 1);

    let mut store = MemoryDisposableAttemptCatalogStore::default();
    store.create(&empty).unwrap();
    for document in [&reserved, &authorized, &started] {
        let current = store.load().unwrap().unwrap();
        store
            .replace_if_revision(current.revision(), document)
            .unwrap();
    }
    assert_eq!(
        store
            .replace_if_revision(started.revision(), &bound)
            .expect_err("raw store replacement cannot manufacture clone provenance")
            .kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );
    assert_eq!(store.load().unwrap(), Some(started));
}

#[test]
fn one_observed_vm_identity_cannot_be_owned_by_two_attempts() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();
    let one_authorized = transition(
        &mut catalog,
        &two,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let one_started = transition(
        &mut catalog,
        &one_authorized,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let one_bound = bind_vm_fixture(&mut catalog, &one_started, 1, 7);
    let two_authorized = transition(
        &mut catalog,
        &one_bound,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let two_started = transition(
        &mut catalog,
        &two_authorized,
        2,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    assert_eq!(
        try_bind_vm_fixture(&two_started, 2, 7)
            .expect_err("one exact VM object cannot satisfy two durable attempts")
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );
}

#[test]
fn reserved_attempt_cannot_enter_external_cleanup_through_the_public_catalog() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let durable = reserved.find_active(&attempt_id).unwrap();

    let error = catalog
        .transition(
            reserved.revision(),
            durable.attempt().attempt_id(),
            durable.attempt().revision(),
            DisposableAttemptCatalogAction::BeginCleanup,
        )
        .expect_err("reserved state has no VM cleanup authority");
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::InvalidAction
    );
    assert_eq!(catalog.load().unwrap(), reserved);

    let error = catalog
        .transition(
            reserved.revision(),
            &attempt_id,
            durable.attempt().revision(),
            DisposableAttemptCatalogAction::RecordTerminal {
                runner: Some(runner(1, 11)),
                job_id: ScaleSetJobId::parse("preauthorization-terminal").unwrap(),
                result: ScaleSetJobResult::parse("canceled").unwrap(),
            },
        )
        .expect_err("job completion cannot manufacture pre-clone cleanup authority");
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::InvalidAction
    );
    assert_eq!(catalog.load().unwrap(), reserved);
}

#[test]
fn durable_active_attempts_are_the_host_resource_ledger() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();

    let usage = serde_json::to_value(two.host_usage().unwrap()).unwrap();
    assert_eq!(usage["workers"], 2);
    assert_eq!(usage["resources"]["cpu_millis"], 2_000);
    assert_eq!(usage["resources"]["memory_bytes"], 4 * 1024 * 1024);
    assert_eq!(usage["resources"]["disk_bytes"], 16 * 1024 * 1024);
}

#[test]
fn catalog_and_attempt_revisions_reject_stale_mutations() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = DisposableAttemptRevision::new(1).unwrap();

    let (provisioning, _) = catalog
        .transition(
            reserved.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::AuthorizeClone,
        )
        .unwrap();

    let stale_catalog = catalog
        .transition(
            reserved.revision(),
            &attempt_id,
            DisposableAttemptRevision::new(2).unwrap(),
            DisposableAttemptCatalogAction::BeginRegistration,
        )
        .unwrap_err();
    assert_eq!(
        stale_catalog.kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );

    let stale_attempt = catalog
        .transition(
            provisioning.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::BeginRegistration,
        )
        .unwrap_err();
    assert_eq!(
        stale_attempt.kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );
}

#[test]
fn global_ownership_identities_are_unique_across_attempts() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();

    let duplicate_name = DisposableAttemptState::reserved(
        DisposableAttemptId::parse("attempt-2").unwrap(),
        CapacityClaimId::parse("claim-2").unwrap(),
        DisposableVmId::parse("vm-2").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-1").unwrap(),
        ScaleSetRunnerRequestId::new(1_002).unwrap(),
        EpochMillis::new(100_002).unwrap(),
    );
    let conflicting = DisposableAttemptReservation::new(
        duplicate_name,
        DisposableWorkerResources::new(1_000, 2 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        template_digest(),
    )
    .unwrap();

    let error = catalog.reserve(one.revision(), conflicting).unwrap_err();
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::CorruptState
    );
    assert_eq!(catalog.load().unwrap().active().len(), 1);

    let duplicate_request = DisposableAttemptState::reserved(
        DisposableAttemptId::parse("attempt-3").unwrap(),
        CapacityClaimId::parse("claim-3").unwrap(),
        DisposableVmId::parse("vm-3").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-3").unwrap(),
        one.active()[0].attempt().runner_request_id(),
        EpochMillis::new(100_003).unwrap(),
    );
    let conflicting = DisposableAttemptReservation::new(
        duplicate_request,
        DisposableWorkerResources::new(1_000, 2 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        template_digest(),
    )
    .unwrap();
    let error = catalog.reserve(one.revision(), conflicting).unwrap_err();
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::CorruptState
    );
    assert_eq!(catalog.load().unwrap().active().len(), 1);
}

#[test]
fn duplicate_terminal_observation_is_satisfied_without_catalog_churn() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let authorized = transition(
        &mut catalog,
        &reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let started = transition(
        &mut catalog,
        &authorized,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let started = bind_vm_fixture(&mut catalog, &started, 1, 1);
    let started = checkpoint_runner_start(&mut catalog, &started, 1, 11);
    let job = ScaleSetJobId::parse("opaque-job-1").unwrap();
    let result = ScaleSetJobResult::parse("future-service-result").unwrap();

    let terminal = transition(
        &mut catalog,
        &started,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: Some(runner(1, 11)),
            job_id: job.clone(),
            result: result.clone(),
        },
    );
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = terminal
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();

    let (same, receipt) = catalog
        .transition(
            terminal.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::RecordTerminal {
                runner: Some(runner(1, 11)),
                job_id: job,
                result,
            },
        )
        .unwrap();
    assert_eq!(same, terminal);
    assert_eq!(
        receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );
    assert_eq!(receipt.catalog_revision, terminal.revision());
    assert_eq!(receipt.attempt_revision, Some(attempt_revision));
}

#[test]
fn identity_drift_remains_distinct_from_an_illegal_phase_action() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let provisioning = transition(
        &mut catalog,
        &reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let started = transition(
        &mut catalog,
        &provisioning,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let started = bind_vm_fixture(&mut catalog, &started, 1, 1);
    let registering = transition(
        &mut catalog,
        &started,
        1,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let registering = transition(
        &mut catalog,
        &registering,
        1,
        DisposableAttemptCatalogAction::RecordJitGenerationStarted,
    );

    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = registering
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    let wrong_runner = ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(9).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-other").unwrap(),
    );
    let drift = catalog
        .transition(
            registering.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::RecordRegistration(wrong_runner),
        )
        .unwrap_err();
    assert_eq!(
        drift.kind(),
        DisposableAttemptCatalogErrorKind::IdentityDrift
    );
}

#[test]
fn completed_attempt_releases_usage_then_moves_to_bounded_replay_history() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let authorized = transition(
        &mut catalog,
        &reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let started = transition(
        &mut catalog,
        &authorized,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let started = bind_vm_fixture(&mut catalog, &started, 1, 1);
    let started = checkpoint_runner_start(&mut catalog, &started, 1, 11);
    let terminal = transition(
        &mut catalog,
        &started,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: Some(runner(1, 11)),
            job_id: ScaleSetJobId::parse("job-complete-1").unwrap(),
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        },
    );
    let destroying = transition(
        &mut catalog,
        &terminal,
        1,
        DisposableAttemptCatalogAction::BeginCleanup,
    );
    let deregistering = transition(
        &mut catalog,
        &destroying,
        1,
        DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Deregistering),
    );
    let releasing = transition(
        &mut catalog,
        &deregistering,
        1,
        DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Releasing),
    );
    let complete = transition(
        &mut catalog,
        &releasing,
        1,
        DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Complete),
    );

    let usage = serde_json::to_value(complete.host_usage().unwrap()).unwrap();
    assert_eq!(usage["workers"], 0);
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = complete
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    let (retired, _) = catalog
        .retire_complete(complete.revision(), &attempt_id, attempt_revision)
        .unwrap();

    assert!(retired.find_active(&attempt_id).is_none());
    assert_eq!(
        retired.find_tombstone(&attempt_id).unwrap().phase(),
        DisposableAttemptPhase::Complete
    );
    assert_eq!(retired.tombstones().len(), 1);

    let reuse = catalog
        .reserve(retired.revision(), reservation(1))
        .unwrap_err();
    assert_eq!(
        reuse.kind(),
        DisposableAttemptCatalogErrorKind::AlreadyExists
    );
}

#[test]
fn active_limit_refuses_new_work_but_keeps_exact_duplicate_reservation_idempotent() {
    let (mut catalog, mut document) = initialized();
    for index in 0..MAX_ACTIVE_DISPOSABLE_ATTEMPTS {
        (document, _) = catalog
            .reserve(document.revision(), reservation(index))
            .unwrap();
    }

    let first = reservation(0);
    let (same, receipt) = catalog.reserve(document.revision(), first).unwrap();
    assert_eq!(same, document);
    assert_eq!(
        receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );

    let overflow = catalog
        .reserve(
            document.revision(),
            reservation(MAX_ACTIVE_DISPOSABLE_ATTEMPTS),
        )
        .unwrap_err();
    assert_eq!(
        overflow.kind(),
        DisposableAttemptCatalogErrorKind::LimitExceeded
    );
}

#[test]
fn exact_runner_ids_cannot_be_reused_across_concurrent_attempts() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();

    let one_provisioning = transition(
        &mut catalog,
        &two,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let one_started = transition(
        &mut catalog,
        &one_provisioning,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let one_started = bind_vm_fixture(&mut catalog, &one_started, 1, 1);
    let one_registering = transition(
        &mut catalog,
        &one_started,
        1,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let one_registering = transition(
        &mut catalog,
        &one_registering,
        1,
        DisposableAttemptCatalogAction::RecordJitGenerationStarted,
    );
    let one_registered = transition(
        &mut catalog,
        &one_registering,
        1,
        DisposableAttemptCatalogAction::RecordRegistration(runner(1, 77)),
    );

    let two_provisioning = transition(
        &mut catalog,
        &one_registered,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let two_started = transition(
        &mut catalog,
        &two_provisioning,
        2,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let two_started = bind_vm_fixture(&mut catalog, &two_started, 2, 2);
    let two_registering = transition(
        &mut catalog,
        &two_started,
        2,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let two_registering = transition(
        &mut catalog,
        &two_registering,
        2,
        DisposableAttemptCatalogAction::RecordJitGenerationStarted,
    );
    let attempt_id = DisposableAttemptId::parse("attempt-2").unwrap();
    let duplicate_runner_id = catalog
        .transition(
            two_registering.revision(),
            &attempt_id,
            two_registering
                .find_active(&attempt_id)
                .unwrap()
                .attempt()
                .revision(),
            DisposableAttemptCatalogAction::RecordRegistration(runner(2, 77)),
        )
        .unwrap_err();
    assert_eq!(
        duplicate_runner_id.kind(),
        DisposableAttemptCatalogErrorKind::CorruptState
    );
}

#[test]
fn exact_job_ids_cannot_be_reused_across_concurrent_attempts() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();
    let shared_job = ScaleSetJobId::parse("shared-job-id").unwrap();

    let one_authorized = transition(
        &mut catalog,
        &two,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let both_authorized = transition(
        &mut catalog,
        &one_authorized,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let one_started = transition(
        &mut catalog,
        &both_authorized,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let one_started = bind_vm_fixture(&mut catalog, &one_started, 1, 1);
    let one_registering = transition(
        &mut catalog,
        &one_started,
        1,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let one_assigned = transition(
        &mut catalog,
        &one_registering,
        1,
        DisposableAttemptCatalogAction::RecordAssigned(shared_job.clone()),
    );
    let one_terminal = transition(
        &mut catalog,
        &one_assigned,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: None,
            job_id: shared_job.clone(),
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        },
    );
    let two_started = transition(
        &mut catalog,
        &one_terminal,
        2,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let two_started = bind_vm_fixture(&mut catalog, &two_started, 2, 2);
    let two_registering = transition(
        &mut catalog,
        &two_started,
        2,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let attempt_id = DisposableAttemptId::parse("attempt-2").unwrap();
    let duplicate_job_id = catalog
        .transition(
            two_registering.revision(),
            &attempt_id,
            two_registering
                .find_active(&attempt_id)
                .unwrap()
                .attempt()
                .revision(),
            DisposableAttemptCatalogAction::RecordAssigned(shared_job),
        )
        .unwrap_err();
    assert_eq!(
        duplicate_job_id.kind(),
        DisposableAttemptCatalogErrorKind::CorruptState
    );
}
