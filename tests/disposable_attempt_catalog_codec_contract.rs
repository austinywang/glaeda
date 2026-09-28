use glaeda::disposable_attempt_catalog::{
    DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION, DisposableAttemptCatalog,
    DisposableAttemptCatalogAction, DisposableAttemptCatalogCodecErrorKind,
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogError,
    DisposableAttemptCatalogRevision, DisposableAttemptCatalogStore,
    DisposableAttemptCatalogWriteDisposition, DisposableAttemptCatalogWriteReceipt,
    DisposableAttemptReservation, MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
    MAX_DISPOSABLE_ATTEMPT_TOMBSTONES, MemoryDisposableAttemptCatalogStore,
    decode_disposable_attempt_catalog, encode_disposable_attempt_catalog,
};
use glaeda::disposable_attempt_state::{
    DisposableAttemptState, decode_disposable_attempt_state, encode_disposable_attempt_state,
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

fn attempt(index: usize) -> DisposableAttemptState {
    DisposableAttemptState::reserved(
        DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap(),
        CapacityClaimId::parse(&format!("claim-{index}")).unwrap(),
        DisposableVmId::parse(&format!("vm-{index}")).unwrap(),
        ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
        ScaleSetRunnerRequestId::new(1_000 + u64::try_from(index).unwrap()).unwrap(),
        EpochMillis::new(200_000 + u64::try_from(index).unwrap()).unwrap(),
    )
}

fn template_digest() -> DisposablePreparedTemplateIdentity {
    current_disposable_prepared_template()
        .unwrap()
        .identity()
        .unwrap()
}

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

fn bind_vm_fixture(
    document: &DisposableAttemptCatalogDocument,
    index: usize,
) -> DisposableAttemptCatalogDocument {
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
            "\"vm_id\":\"vm-{index}\",\"vm_identity_digest\":\"sha256:{index:064x}\",\"runner_name\""
        ),
        1,
    );
    let bound_attempt = decode_disposable_attempt_state(next_attempt_json.as_bytes()).unwrap();
    assert!(bound_attempt.vm_identity().is_some());
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
    decode_disposable_attempt_catalog(catalog_json.as_bytes()).unwrap()
}

fn other_template_digest() -> DisposablePreparedTemplateIdentity {
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

fn reservation(index: usize) -> DisposableAttemptReservation {
    reservation_with_digest(index, template_digest())
}

fn reservation_with_digest(
    index: usize,
    prepared_template_digest: DisposablePreparedTemplateIdentity,
) -> DisposableAttemptReservation {
    DisposableAttemptReservation::new(
        attempt(index),
        DisposableWorkerResources::new(1_500, 4 * 1024 * 1024, 16 * 1024 * 1024).unwrap(),
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

fn transition<S: DisposableAttemptCatalogStore>(
    catalog: &mut DisposableAttemptCatalog<S>,
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

fn populated_catalog() -> DisposableAttemptCatalogDocument {
    let mut catalog = DisposableAttemptCatalog::new(FixtureStore::default());
    let (empty, _) = catalog.initialize().unwrap();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();

    let provisioning = transition(
        &mut catalog,
        &two,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let started = transition(
        &mut catalog,
        &provisioning,
        2,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let registering = bind_vm_fixture(&started, 2);
    catalog = DisposableAttemptCatalog::new(FixtureStore::with_document(registering.clone()));
    let registering = transition(
        &mut catalog,
        &registering,
        2,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let jit_started = transition(
        &mut catalog,
        &registering,
        2,
        DisposableAttemptCatalogAction::RecordJitGenerationStarted,
    );
    let registered = transition(
        &mut catalog,
        &jit_started,
        2,
        DisposableAttemptCatalogAction::RecordRegistration(runner(2, 22)),
    );

    let both_authorized = transition(
        &mut catalog,
        &registered,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let both_started = transition(
        &mut catalog,
        &both_authorized,
        1,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    );
    let both_started = bind_vm_fixture(&both_started, 1);
    catalog = DisposableAttemptCatalog::new(FixtureStore::with_document(both_started.clone()));

    let registering = transition(
        &mut catalog,
        &both_started,
        1,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let jit_started = transition(
        &mut catalog,
        &registering,
        1,
        DisposableAttemptCatalogAction::RecordJitGenerationStarted,
    );
    let registered = transition(
        &mut catalog,
        &jit_started,
        1,
        DisposableAttemptCatalogAction::RecordRegistration(runner(1, 11)),
    );
    let runner_started = transition(
        &mut catalog,
        &registered,
        1,
        DisposableAttemptCatalogAction::RecordRunnerStartStarted,
    );

    let terminal = transition(
        &mut catalog,
        &runner_started,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: Some(runner(1, 11)),
            job_id: ScaleSetJobId::parse("completed-job-1").unwrap(),
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
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = complete
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    catalog
        .retire_complete(complete.revision(), &attempt_id, attempt_revision)
        .unwrap()
        .0
}

#[test]
fn canonical_codec_round_trips_progressed_active_and_tombstone_state() {
    let document = populated_catalog();
    let encoded = encode_disposable_attempt_catalog(&document).unwrap();
    let decoded = decode_disposable_attempt_catalog(&encoded).unwrap();

    assert_eq!(decoded, document);
    assert_eq!(decoded.active().len(), 1);
    assert_eq!(decoded.tombstones().len(), 1);
    assert_eq!(
        decoded.active()[0].prepared_template_identity(),
        &template_digest()
    );
    assert_eq!(
        decoded.active()[0].attempt().phase(),
        DisposableAttemptPhase::Registering
    );
    assert_eq!(decoded.active()[0].attempt().runner_id().unwrap().get(), 22);
    assert_eq!(
        decoded.tombstones()[0].phase(),
        DisposableAttemptPhase::Complete
    );
}

#[test]
fn prepared_template_digest_is_part_of_the_durable_reservation_identity() {
    let mut catalog = DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (empty, _) = catalog.initialize().unwrap();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let encoded = encode_disposable_attempt_catalog(&reserved).unwrap();
    let mut other_catalog =
        DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (other_empty, _) = other_catalog.initialize().unwrap();
    let (other_reserved, _) = other_catalog
        .reserve(
            other_empty.revision(),
            reservation_with_digest(1, other_template_digest()),
        )
        .unwrap();
    let changed = encode_disposable_attempt_catalog(&other_reserved).unwrap();
    let decoded = decode_disposable_attempt_catalog(&changed).unwrap();

    assert_ne!(changed, encoded);
    assert_eq!(
        decoded.active()[0].prepared_template_identity(),
        &other_template_digest()
    );
}

#[test]
fn noncanonical_json_is_rejected_after_full_validation() {
    let encoded = encode_disposable_attempt_catalog(&populated_catalog()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    let pretty = serde_json::to_vec_pretty(&value).unwrap();

    let error = decode_disposable_attempt_catalog(&pretty).unwrap_err();
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogCodecErrorKind::NonCanonical
    );
}

#[test]
fn catalog_revision_must_match_the_represented_history() {
    let empty =
        encode_disposable_attempt_catalog(&DisposableAttemptCatalogDocument::empty()).unwrap();
    let mut empty_value: serde_json::Value = serde_json::from_slice(&empty).unwrap();
    empty_value["revision"] = serde_json::json!(2);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&empty_value).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let mut catalog = DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (empty, _) = catalog.initialize().unwrap();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let encoded = encode_disposable_attempt_catalog(&reserved).unwrap();
    let mut reserved_value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    reserved_value["revision"] = serde_json::json!(1);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&reserved_value).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let mut impossible_reservation: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    impossible_reservation["revision"] = serde_json::json!(3);
    impossible_reservation["active"][0]["attempt"]["revision"] = serde_json::json!(2);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&impossible_reservation).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let mut preauthorization_terminal: serde_json::Value =
        serde_json::from_slice(&encoded).unwrap();
    preauthorization_terminal["revision"] = serde_json::json!(3);
    preauthorization_terminal["active"][0]["attempt"]["revision"] = serde_json::json!(2);
    preauthorization_terminal["active"][0]["attempt"]["runner_id"] = serde_json::json!(11);
    preauthorization_terminal["active"][0]["attempt"]["phase"] = serde_json::json!("terminal");
    preauthorization_terminal["active"][0]["attempt"]["github_job_id"] =
        serde_json::json!("preauthorization-terminal");
    preauthorization_terminal["active"][0]["attempt"]["result"] = serde_json::json!("canceled");
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&preauthorization_terminal).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );
}

#[test]
fn saturated_tombstone_history_retains_a_safe_revision_lower_bound() {
    let mut catalog = DisposableAttemptCatalog::new(FixtureStore::default());
    let (mut document, _) = catalog.initialize().unwrap();
    for index in 1..=MAX_DISPOSABLE_ATTEMPT_TOMBSTONES + 1 {
        document = catalog
            .reserve(document.revision(), reservation(index))
            .unwrap()
            .0;
        document = transition(
            &mut catalog,
            &document,
            index,
            DisposableAttemptCatalogAction::AuthorizeClone,
        );
        document = transition(
            &mut catalog,
            &document,
            index,
            DisposableAttemptCatalogAction::RecordCloneStarted,
        );
        document = bind_vm_fixture(&document, index);
        catalog = DisposableAttemptCatalog::new(FixtureStore::with_document(document.clone()));
        document = transition(
            &mut catalog,
            &document,
            index,
            DisposableAttemptCatalogAction::BeginRegistration,
        );
        let job_id = ScaleSetJobId::parse(&format!("completed-job-{index}")).unwrap();
        document = transition(
            &mut catalog,
            &document,
            index,
            DisposableAttemptCatalogAction::RecordAssigned(job_id.clone()),
        );
        document = transition(
            &mut catalog,
            &document,
            index,
            DisposableAttemptCatalogAction::RecordTerminal {
                runner: None,
                job_id,
                result: ScaleSetJobResult::parse("canceled").unwrap(),
            },
        );
        for phase in [
            None,
            Some(DisposableAttemptPhase::Deregistering),
            Some(DisposableAttemptPhase::Releasing),
            Some(DisposableAttemptPhase::Complete),
        ] {
            document = transition(
                &mut catalog,
                &document,
                index,
                phase.map_or(
                    DisposableAttemptCatalogAction::BeginCleanup,
                    DisposableAttemptCatalogAction::AdvanceCleanup,
                ),
            );
        }
        let attempt_id = DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap();
        let attempt_revision = document
            .find_active(&attempt_id)
            .unwrap()
            .attempt()
            .revision();
        document = catalog
            .retire_complete(document.revision(), &attempt_id, attempt_revision)
            .unwrap()
            .0;
    }

    assert_eq!(
        document.tombstones().len(),
        MAX_DISPOSABLE_ATTEMPT_TOMBSTONES
    );
    let encoded = encode_disposable_attempt_catalog(&document).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&encoded).unwrap(),
        document
    );
}

#[test]
fn top_level_future_schema_and_unknown_fields_fail_closed() {
    let encoded = encode_disposable_attempt_catalog(&populated_catalog()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

    let mut legacy_attempt = value.clone();
    legacy_attempt["active"][0]["attempt"]["schema_version"] = serde_json::json!(1);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&legacy_attempt).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    let mut legacy_catalog = value.clone();
    legacy_catalog["schema_version"] = serde_json::json!(1);
    legacy_catalog["active"][0]
        .as_object_mut()
        .unwrap()
        .remove("prepared_template_digest");
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&legacy_catalog).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    let mut previous_catalog = value.clone();
    previous_catalog["schema_version"] = serde_json::json!(2);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&previous_catalog).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    previous_catalog["schema_version"] = serde_json::json!(3);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&previous_catalog).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    previous_catalog["schema_version"] = serde_json::json!(5);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&previous_catalog).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    previous_catalog["schema_version"] = serde_json::json!(6);
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&previous_catalog).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    value["schema_version"] = serde_json::json!(DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION + 1);
    let future = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&future)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    value["schema_version"] = serde_json::json!(DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION);
    value["unexpected"] = serde_json::json!(true);
    let unknown = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&unknown)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::InvalidDocument
    );
}

#[test]
fn embedded_attempt_unknown_fields_and_invalid_resources_fail_closed() {
    let encoded = encode_disposable_attempt_catalog(&populated_catalog()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

    value["active"][0]["attempt"]["unexpected"] = serde_json::json!(true);
    let invalid_attempt = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&invalid_attempt)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["active"][0]["resources"]["cpu_millis"] = serde_json::json!(0);
    let invalid_resources = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&invalid_resources)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["active"][0]["prepared_template_digest"] = serde_json::json!("sha256:moving");
    assert_eq!(
        decode_disposable_attempt_catalog(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );
}

#[test]
fn global_identity_collisions_are_revalidated_after_decode() {
    let mut catalog = DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (empty, _) = catalog.initialize().unwrap();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();
    let encoded = encode_disposable_attempt_catalog(&two).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

    let first_name = value["active"][0]["attempt"]["runner_name"].clone();
    value["active"][1]["attempt"]["runner_name"] = first_name;
    let collision = serde_json::to_vec(&value).unwrap();

    assert_eq!(
        decode_disposable_attempt_catalog(&collision)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    let first_request = value["active"][0]["attempt"]["runner_request_id"].clone();
    value["active"][1]["attempt"]["runner_request_id"] = first_request;
    let collision = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&collision)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let encoded = encode_disposable_attempt_catalog(&populated_catalog()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    let completed_request = value["tombstones"][0]["runner_request_id"].clone();
    value["active"][0]["attempt"]["runner_request_id"] = completed_request;
    let collision = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&collision)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );
}

#[test]
fn oversized_document_is_rejected_before_json_parse() {
    let oversized = vec![b' '; MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES + 1];
    assert_eq!(
        decode_disposable_attempt_catalog(&oversized)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::DocumentTooLarge
    );
}
