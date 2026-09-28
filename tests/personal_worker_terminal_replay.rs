#![cfg(unix)]

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    DrainAcknowledgement, EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput,
    ExecutionAdmissionRecord, ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId, UnavailableReason,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode,
    PersonalWorkerCacheNamespace, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPriority, PersonalWorkerProfile, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_store::{
    MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES, PersonalWorkerDurableCacheLease, PersonalWorkerStore,
    PersonalWorkerStoreDocument, PersonalWorkerStoreError, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreRecovery, PersonalWorkerStoreRecoveryDisposition,
    PersonalWorkerStoreRevision, PersonalWorkerStoreWriteDisposition,
    PersonalWorkerStoreWriteReceipt, PersonalWorkerTerminalTombstone,
    decode_personal_worker_store_document, encode_personal_worker_store_document,
};
use glaeda::personal_worker_store_transaction::{
    PersonalWorkerStoreMutation, PersonalWorkerStoreMutationDisposition,
    PersonalWorkerStoreMutationErrorKind, apply_personal_worker_store_mutation,
};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE: u64 = 10_000_000;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct MemoryStore {
    current: Option<PersonalWorkerStoreDocument>,
    staged: Option<PersonalWorkerStoreDocument>,
}

impl MemoryStore {
    fn with_current(current: PersonalWorkerStoreDocument) -> Self {
        Self {
            current: Some(current),
            staged: None,
        }
    }
}

impl PersonalWorkerStore for MemoryStore {
    fn load(&self) -> Result<Option<PersonalWorkerStoreDocument>, PersonalWorkerStoreError> {
        Ok(self.current.clone())
    }

    fn create(
        &mut self,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        if self.current.is_some() {
            return Err(PersonalWorkerStoreError::new(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "state already exists",
            ));
        }
        self.current = Some(document.clone());
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Created,
            document.revision(),
            1,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: PersonalWorkerStoreRevision,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        let current = self.current.as_ref().ok_or_else(|| {
            PersonalWorkerStoreError::new(PersonalWorkerStoreErrorKind::Missing, "missing")
        })?;
        if current.revision() != expected_revision {
            return Err(PersonalWorkerStoreError::new(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "stale",
            ));
        }
        document.validate_successor_of(current)?;
        self.current = Some(document.clone());
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Replaced,
            document.revision(),
            1,
        ))
    }

    fn recover(&mut self) -> Result<PersonalWorkerStoreRecovery, PersonalWorkerStoreError> {
        if let Some(staged) = self.staged.take() {
            if let Some(current) = &self.current {
                staged.validate_successor_of(current)?;
            }
            let revision = staged.revision();
            self.current = Some(staged);
            return Ok(PersonalWorkerStoreRecovery::new(
                PersonalWorkerStoreRecoveryDisposition::PublishedStaged,
                Some(revision),
            ));
        }
        Ok(PersonalWorkerStoreRecovery::new(
            PersonalWorkerStoreRecoveryDisposition::Clean,
            self.current
                .as_ref()
                .map(PersonalWorkerStoreDocument::revision),
        ))
    }
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-terminal-replay-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create root");
        fs::set_permissions(&path, Permissions::from_mode(0o750)).expect("set root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn store_directory(&self) -> PathBuf {
        self.0.join("personal-worker")
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(2_000, 2 * GIB, 512).expect("limits")
}

fn request(id: &str, submitted_at: u64) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse(id).expect("request ID"),
            VerificationProfileId::parse("smolrunner.required").expect("verification profile"),
            RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
        ),
        source: PersonalWorkerSourceIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"a".repeat(40)).expect("commit"),
            GitTreeId::parse(&"b".repeat(40)).expect("tree"),
        ),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits(),
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("build-cache").expect("cache ID"),
            repository: RepositoryRef::parse("example/project").expect("cache repository"),
            namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
                .expect("namespace digest"),
        },
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(submitted_at),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn reservation(id: &str, reserved_at: u64) -> ReservationEvidence {
    ReservationEvidence::new(
        ReservationId::parse(&format!("reservation-{id}")).expect("reservation ID"),
        ReservationGeneration::new(1).expect("reservation generation"),
        time(reserved_at),
        time(reserved_at + 100_000),
    )
}

fn admission(
    request: &PersonalWorkerJobRequest,
    reservation: &ReservationEvidence,
    state: ExecutionAdmissionState,
    observed_at: u64,
    acknowledgement: Option<DrainAcknowledgement>,
    unavailable_reason: Option<UnavailableReason>,
) -> ExecutionAdmissionRecord {
    ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state,
        observed_at: time(observed_at),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            reservation.reserved_at,
            ExecutionResourceLimits::new(8_000, 10 * GIB, 8_192).expect("host limits"),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(reservation.clone()),
        acknowledgement,
        fallback_eligibility: request.fallback_eligibility.clone(),
        unavailable_reason,
    })
    .expect("admission")
}

fn lease(
    request: &PersonalWorkerJobRequest,
    reservation: &ReservationEvidence,
) -> PersonalWorkerDurableCacheLease {
    PersonalWorkerDurableCacheLease::new(
        request.identity.request_id.clone(),
        request.cache_namespace.clone(),
        request.cache_access,
        reservation.id.clone(),
        reservation.generation,
        reservation.reserved_at,
    )
}

fn terminal_tombstone(
    id: &str,
    base: u64,
) -> (
    PersonalWorkerTerminalTombstone,
    ExecutionAdmissionRecord,
    ExecutionRequestId,
) {
    let request = request(id, base);
    let reservation = reservation(id, base + 20);
    let cache_lease = lease(&request, &reservation);
    let terminal = admission(
        &request,
        &reservation,
        ExecutionAdmissionState::Unavailable,
        base + 70,
        Some(DrainAcknowledgement::Drain),
        Some(UnavailableReason::Drained),
    );
    let request_id = request.identity.request_id.clone();
    let tombstone = PersonalWorkerTerminalTombstone::new(
        request,
        terminal.clone(),
        Some(time(base + 40)),
        cache_lease,
    )
    .expect("terminal tombstone");
    (tombstone, terminal, request_id)
}

fn active_document(
    id: &str,
    base: u64,
    terminal_tombstones: Vec<PersonalWorkerTerminalTombstone>,
) -> (
    PersonalWorkerStoreDocument,
    PersonalWorkerJobRequest,
    ReservationEvidence,
    ExecutionAdmissionRecord,
) {
    let request = request(id, base);
    let reservation = reservation(id, base + 20);
    let cache_lease = lease(&request, &reservation);
    let draining = admission(
        &request,
        &reservation,
        ExecutionAdmissionState::Draining,
        base + 60,
        Some(DrainAcknowledgement::Drain),
        None,
    );
    let terminal = admission(
        &request,
        &reservation,
        ExecutionAdmissionState::Unavailable,
        base + 70,
        Some(DrainAcknowledgement::Drain),
        Some(UnavailableReason::Drained),
    );
    let queue = PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
        observed_at: time(base + 60),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Work,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(base + 60)),
        queued: vec![],
        active: vec![PersonalWorkerActiveReservation {
            request: request.clone(),
            admission: draining,
            started_at: Some(time(base + 40)),
        }],
        pending_profile_change: None,
    };
    let document = PersonalWorkerStoreDocument::new_with_terminal_tombstones(
        queue,
        vec![cache_lease],
        terminal_tombstones,
    )
    .expect("active document");
    (document, request, reservation, terminal)
}

#[test]
fn exact_terminal_replay_is_duplicate_and_conflicting_evidence_fails_closed() {
    let (document, request, reservation, terminal) = active_document("job-one", BASE, vec![]);
    let mut store = MemoryStore::with_current(document);
    let applied = apply_personal_worker_store_mutation(
        &mut store,
        PersonalWorkerStoreRevision::new(1).expect("revision"),
        PersonalWorkerQueueGeneration::new(1).expect("generation"),
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id.clone(),
            terminal_admission: terminal.clone(),
        },
    )
    .expect("release");
    assert_eq!(
        applied.disposition(),
        PersonalWorkerStoreMutationDisposition::Applied
    );
    let current = store.current.clone().expect("current");
    assert!(current.queue().active.is_empty());
    assert!(current.cache_leases().is_empty());
    assert_eq!(current.terminal_tombstones().len(), 1);

    let duplicate = apply_personal_worker_store_mutation(
        &mut store,
        applied.new_revision(),
        applied.new_queue_generation(),
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id.clone(),
            terminal_admission: terminal,
        },
    )
    .expect("duplicate terminal replay");
    assert_eq!(
        duplicate.disposition(),
        PersonalWorkerStoreMutationDisposition::Duplicate
    );
    assert_eq!(duplicate.old_revision(), duplicate.new_revision());
    assert_eq!(
        duplicate.old_queue_generation(),
        duplicate.new_queue_generation()
    );

    let conflicting = admission(
        &request,
        &reservation,
        ExecutionAdmissionState::Unavailable,
        BASE + 80,
        None,
        Some(UnavailableReason::HostUnavailable),
    );
    let error = apply_personal_worker_store_mutation(
        &mut store,
        duplicate.new_revision(),
        duplicate.new_queue_generation(),
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id,
            terminal_admission: conflicting,
        },
    )
    .expect_err("conflicting terminal replay");
    assert_eq!(error.kind(), PersonalWorkerStoreMutationErrorKind::Conflict);
}

#[test]
fn terminal_tombstone_wire_is_canonical_and_digest_bound() {
    let (tombstone, _, _) = terminal_tombstone("wire-job", BASE);
    let queue = PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
        observed_at: time(BASE + 100),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Interactive,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE + 90)),
        queued: vec![],
        active: vec![],
        pending_profile_change: None,
    };
    let document =
        PersonalWorkerStoreDocument::new_with_terminal_tombstones(queue, vec![], vec![tombstone])
            .expect("terminal document");
    let encoded = encode_personal_worker_store_document(&document).expect("encode");
    assert_eq!(
        decode_personal_worker_store_document(&encoded).expect("decode"),
        document
    );

    let mut altered: serde_json::Value = serde_json::from_slice(&encoded).expect("parse document");
    altered["terminal_tombstones"][0]["evidence_digest"] =
        serde_json::Value::String(format!("sha256:{}", "00".repeat(32)));
    let mut bytes = serde_json::to_vec_pretty(&altered).expect("encode altered");
    bytes.push(b'\n');
    let error = decode_personal_worker_store_document(&bytes)
        .expect_err("digest mismatch must fail closed");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);
}

#[test]
fn bounded_eviction_makes_old_terminal_replay_unprovable() {
    let mut tombstones = Vec::new();
    let mut oldest = None;
    for index in 0..MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES {
        let id = format!("old-job-{index:02}");
        let evidence = terminal_tombstone(&id, BASE + index as u64 * 1_000);
        if index == 0 {
            oldest = Some((evidence.1.clone(), evidence.2.clone()));
        }
        tombstones.push(evidence.0);
    }
    let base = BASE + 100_000;
    let (document, request, _, terminal) = active_document("new-job", base, tombstones);
    let mut store = MemoryStore::with_current(document);
    let applied = apply_personal_worker_store_mutation(
        &mut store,
        PersonalWorkerStoreRevision::new(1).expect("revision"),
        PersonalWorkerQueueGeneration::new(1).expect("generation"),
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id.clone(),
            terminal_admission: terminal.clone(),
        },
    )
    .expect("release with eviction");
    let current = store.current.clone().expect("current");
    assert_eq!(
        current.terminal_tombstones().len(),
        MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES
    );
    let (oldest_terminal, oldest_request_id) = oldest.expect("oldest evidence");
    assert!(
        current
            .terminal_tombstones()
            .iter()
            .all(|entry| { entry.request().identity.request_id != oldest_request_id })
    );

    let error = apply_personal_worker_store_mutation(
        &mut store,
        applied.new_revision(),
        applied.new_queue_generation(),
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: oldest_request_id,
            terminal_admission: oldest_terminal,
        },
    )
    .expect_err("evicted replay is unprovable");
    assert_eq!(error.kind(), PersonalWorkerStoreMutationErrorKind::NotFound);

    let duplicate = apply_personal_worker_store_mutation(
        &mut store,
        applied.new_revision(),
        applied.new_queue_generation(),
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id,
            terminal_admission: terminal,
        },
    )
    .expect("retained replay");
    assert_eq!(
        duplicate.disposition(),
        PersonalWorkerStoreMutationDisposition::Duplicate
    );
}

#[test]
fn unix_staged_recovery_preserves_exact_terminal_replay_authority() {
    let (initial, request, _, terminal) = active_document("unix-job", BASE, vec![]);
    let mut memory = MemoryStore::with_current(initial.clone());
    let applied = apply_personal_worker_store_mutation(
        &mut memory,
        initial.revision(),
        initial.queue().generation,
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id.clone(),
            terminal_admission: terminal.clone(),
        },
    )
    .expect("build staged successor");
    let staged = memory.current.clone().expect("staged document");

    let root = TempRoot::new("staged");
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    store.create(&initial).expect("create initial");
    drop(store);

    let staged_path = root.store_directory().join(".next.json");
    fs::write(
        &staged_path,
        encode_personal_worker_store_document(&staged).expect("encode staged"),
    )
    .expect("write staged");
    fs::set_permissions(&staged_path, Permissions::from_mode(0o600)).expect("set staged mode");

    let (mut recovered, recovery) =
        UnixPersonalWorkerStore::open_or_create(root.path()).expect("recover staged");
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerStoreRecoveryDisposition::PublishedStaged
    );
    assert_eq!(recovered.load().expect("load recovered"), Some(staged));

    let duplicate = apply_personal_worker_store_mutation(
        &mut recovered,
        applied.new_revision(),
        applied.new_queue_generation(),
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id,
            terminal_admission: terminal,
        },
    )
    .expect("replay after recovery");
    assert_eq!(
        duplicate.disposition(),
        PersonalWorkerStoreMutationDisposition::Duplicate
    );
}

#[test]
fn terminal_ledger_successor_cannot_replace_retained_proof() {
    let (first, _, _) = terminal_tombstone("retained-one", BASE);
    let (replacement, _, _) = terminal_tombstone("replacement-one", BASE + 1_000);
    let initial_queue = PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
        observed_at: time(BASE + 100),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Interactive,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE + 90)),
        queued: vec![],
        active: vec![],
        pending_profile_change: None,
    };
    let initial = PersonalWorkerStoreDocument::new_with_terminal_tombstones(
        initial_queue,
        vec![],
        vec![first],
    )
    .expect("initial terminal ledger");
    let next_queue = PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(2).expect("generation"),
        observed_at: time(BASE + 1_100),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Interactive,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE + 1_090)),
        queued: vec![],
        active: vec![],
        pending_profile_change: None,
    };
    let error = initial
        .advance_with_terminal_tombstones(next_queue, vec![], vec![replacement])
        .expect_err("retained proof replacement must fail closed");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
}

#[test]
fn successor_validation_rejects_early_terminal_proof_loss_and_fabrication() {
    let (retained, _, _) = terminal_tombstone("retained-job", BASE);
    let (document, _, _, _) = active_document("live-job", BASE + 100_000, vec![retained.clone()]);

    let mut next_queue = document.queue().clone();
    next_queue.generation = next_queue.generation.next().expect("next generation");
    next_queue.observed_at = time(BASE + 100_061);
    assert_eq!(
        document
            .advance_with_terminal_tombstones(
                next_queue.clone(),
                document.cache_leases().to_vec(),
                vec![],
            )
            .expect_err("retained terminal proof must not disappear early")
            .kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );

    let (fabricated, _, _) = terminal_tombstone("fabricated-job", BASE + 100_000);
    next_queue.observed_at = fabricated.completed_at();
    let fabricated_successor = document
        .advance_with_terminal_tombstones(
            next_queue,
            document.cache_leases().to_vec(),
            vec![retained, fabricated],
        )
        .expect("internally valid fabricated-ledger document");
    assert_eq!(
        fabricated_successor
            .validate_successor_of(&document)
            .expect_err("terminal proof must derive from the previous active reservation")
            .kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );
}

#[test]
fn successor_validation_rejects_active_release_without_terminal_proof() {
    let (document, _, _, terminal) = active_document("missing-proof-job", BASE, vec![]);
    let mut queue = document.queue().clone();
    queue.generation = queue.generation.next().expect("next generation");
    queue.observed_at = terminal.observed_at();
    queue.active.clear();
    let successor = document
        .advance_with_terminal_tombstones(queue, vec![], vec![])
        .expect("internally valid proof-free release document");
    assert_eq!(
        successor
            .validate_successor_of(&document)
            .expect_err("active release must append exact terminal proof")
            .kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );
}

fn terminal_release_parts(
    document: &PersonalWorkerStoreDocument,
    request_id: &ExecutionRequestId,
    terminal: &ExecutionAdmissionRecord,
) -> (
    PersonalWorkerQueueInput,
    Vec<PersonalWorkerDurableCacheLease>,
    Vec<PersonalWorkerTerminalTombstone>,
) {
    let active_index = document
        .queue()
        .active
        .iter()
        .position(|active| &active.request.identity.request_id == request_id)
        .expect("active release target");
    let active = document.queue().active[active_index].clone();
    let transition = active
        .admission
        .plan_transition(terminal.clone())
        .expect("terminal transition");
    let lease_index = document
        .cache_leases()
        .iter()
        .position(|lease| lease.request_id() == request_id)
        .expect("release lease");
    let lease = document.cache_leases()[lease_index].clone();
    let tombstone = PersonalWorkerTerminalTombstone::new(
        active.request,
        transition.resulting_record().clone(),
        active.started_at,
        lease,
    )
    .expect("terminal evidence");

    let mut queue = document.queue().clone();
    queue.active.remove(active_index);
    queue.generation = queue.generation.next().expect("next generation");
    queue.observed_at = tombstone.completed_at();
    let mut leases = document.cache_leases().to_vec();
    leases.remove(lease_index);
    let mut tombstones = document.terminal_tombstones().to_vec();
    tombstones.push(tombstone);
    if tombstones.len() > MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES {
        tombstones.remove(0);
    }
    (queue, leases, tombstones)
}

fn assert_terminal_successor_rejected(
    previous: &PersonalWorkerStoreDocument,
    queue: PersonalWorkerQueueInput,
    leases: Vec<PersonalWorkerDurableCacheLease>,
    tombstones: Vec<PersonalWorkerTerminalTombstone>,
    message: &str,
) {
    let successor = previous
        .advance_with_terminal_tombstones(queue, leases, tombstones)
        .expect("mutated terminal document remains internally valid");
    assert_eq!(
        successor
            .validate_successor_of(previous)
            .expect_err(message)
            .kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );
}

fn multi_active_document(
    ids: &[&str],
    base: u64,
) -> (
    PersonalWorkerStoreDocument,
    Vec<ExecutionAdmissionRecord>,
    Vec<ExecutionRequestId>,
) {
    let mut active = Vec::new();
    let mut leases = Vec::new();
    let mut terminals = Vec::new();
    let mut request_ids = Vec::new();
    let common_observed_at = base + ids.len() as u64 * 1_000 + 60;
    for (index, id) in ids.iter().enumerate() {
        let offset = index as u64 * 1_000;
        let mut request = request(id, base + offset);
        request.cache_access = PersonalWorkerCacheAccessMode::Read;
        let reservation = reservation(id, base + offset + 20);
        let cache_lease = lease(&request, &reservation);
        let draining = admission(
            &request,
            &reservation,
            ExecutionAdmissionState::Draining,
            common_observed_at,
            Some(DrainAcknowledgement::Drain),
            None,
        );
        let terminal = admission(
            &request,
            &reservation,
            ExecutionAdmissionState::Unavailable,
            common_observed_at + 10,
            Some(DrainAcknowledgement::Drain),
            Some(UnavailableReason::Drained),
        );
        request_ids.push(request.identity.request_id.clone());
        terminals.push(terminal);
        active.push(PersonalWorkerActiveReservation {
            request,
            admission: draining,
            started_at: Some(time(base + offset + 40)),
        });
        leases.push(cache_lease);
    }
    let queue = PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
        observed_at: time(common_observed_at),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Work,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(common_observed_at)),
        queued: vec![],
        active,
        pending_profile_change: None,
    };
    let document = PersonalWorkerStoreDocument::new_with_terminal_tombstones(queue, leases, vec![])
        .expect("multi-active document");
    (document, terminals, request_ids)
}

#[test]
fn terminal_successor_rejects_unrelated_queue_profile_activity_intent_and_time_drift() {
    let (document, release_request, _, terminal) =
        active_document("exact-delta-job", BASE + 200_000, vec![]);

    let (mut queue, leases, tombstones) =
        terminal_release_parts(&document, &release_request.identity.request_id, &terminal);
    queue
        .queued
        .push(request("unrelated-queued", BASE + 199_000));
    assert_terminal_successor_rejected(
        &document,
        queue,
        leases,
        tombstones,
        "terminal release must not submit unrelated work",
    );

    let (mut queue, leases, tombstones) =
        terminal_release_parts(&document, &release_request.identity.request_id, &terminal);
    queue.profile_observation =
        PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Interactive);
    assert_terminal_successor_rejected(
        &document,
        queue,
        leases,
        tombstones,
        "terminal release must not change current profile",
    );

    let (mut queue, leases, tombstones) =
        terminal_release_parts(&document, &release_request.identity.request_id, &terminal);
    queue.activity_evidence =
        PersonalWorkerActivityEvidence::observed(time(terminal.observed_at().get() - 1));
    assert_terminal_successor_rejected(
        &document,
        queue,
        leases,
        tombstones,
        "terminal release must not rewrite last activity",
    );

    let (mut queue, leases, tombstones) =
        terminal_release_parts(&document, &release_request.identity.request_id, &terminal);
    queue.pending_profile_change = Some(
        glaeda::personal_worker_queue::PersonalWorkerPendingProfileChange {
            target: PersonalWorkerProfile::Interactive,
            requested_at: terminal.observed_at(),
        },
    );
    assert_terminal_successor_rejected(
        &document,
        queue,
        leases,
        tombstones,
        "terminal release must not create profile intent",
    );

    let (mut queue, leases, tombstones) =
        terminal_release_parts(&document, &release_request.identity.request_id, &terminal);
    queue.observed_at = time(terminal.observed_at().get() + 1);
    assert_terminal_successor_rejected(
        &document,
        queue,
        leases,
        tombstones,
        "terminal observation must equal completion",
    );
}

#[test]
fn terminal_successor_rejects_retained_active_and_lease_mutation() {
    let (document, terminals, request_ids) =
        multi_active_document(&["release-a", "retain-b"], BASE + 300_000);

    let (mut queue, leases, tombstones) =
        terminal_release_parts(&document, &request_ids[0], &terminals[0]);
    queue.active[0].started_at = Some(time(BASE + 301_041));
    assert_terminal_successor_rejected(
        &document,
        queue,
        leases,
        tombstones,
        "terminal release must preserve retained active evidence",
    );

    let (queue, mut leases, tombstones) =
        terminal_release_parts(&document, &request_ids[0], &terminals[0]);
    let retained = &queue.active[0];
    let reservation = retained
        .admission
        .reservation()
        .expect("retained reservation");
    leases[0] = PersonalWorkerDurableCacheLease::new(
        retained.request.identity.request_id.clone(),
        retained.request.cache_namespace.clone(),
        retained.request.cache_access,
        reservation.id.clone(),
        reservation.generation,
        time(reservation.reserved_at.get() + 1),
    );
    assert_terminal_successor_rejected(
        &document,
        queue,
        leases,
        tombstones,
        "terminal release must preserve retained lease evidence",
    );
}
