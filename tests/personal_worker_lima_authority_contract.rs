#![cfg(unix)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

mod lima_host_identity_support;

use lima_host_identity_support::LimaHostIdentityFixture;

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::disposable_attempt_catalog::{
    DisposableAttemptCatalogDocument, encode_disposable_attempt_catalog,
};
use glaeda::execution_admission::{
    EpochMillis, ExecutionRequestId, ExecutionResourceLimits, HostCapacityObservation,
    RunnerProfileId,
};
use glaeda::lima_lifecycle::{
    LimaCacheDiskId, LimaCacheDiskIdentity, LimaInstanceId, LimaInstanceIdentity,
    LimaLifecycleObservation, LimaLifecycleObservationDefinition, LimaLifecyclePolicy,
    LimaLifecycleState, LimaObservedResources, LimaProfileGeneration, LimaResourceProfile,
};
use glaeda::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationAdapter, LimaObservationRequest, LimaVmType,
};
use glaeda::mac_availability::{
    AvailabilityRequest, EffectiveAvailabilityMode, HostPowerSource, JobActivity,
    MacAvailabilityObservation, MemoryPressure, ObservationFreshness, VmPowerState,
};
use glaeda::macos_resource_observation::{
    lima_process_command, memory_pressure_command, power_command, swap_command,
};
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::personal_worker_host_broker::{HostBrokerRunnerObservation, HostBrokerRunnerState};
use glaeda::personal_worker_lima_authority::{
    PERSONAL_WORKER_LIMA_AUTHORITY_SCHEMA_VERSION, PERSONAL_WORKER_LIMA_RECOVERY_SCHEMA_VERSION,
    PersonalWorkerLimaAttemptInput, PersonalWorkerLimaAttemptPhase,
    PersonalWorkerLimaAuthorityDocument, PersonalWorkerLimaAuthorityErrorKind,
    PersonalWorkerLimaRecoveryDisposition, decode_personal_worker_lima_authority,
    encode_personal_worker_lima_authority, personal_worker_lima_enrollment_confirmation,
};
use glaeda::personal_worker_mac_observation::{
    PersonalWorkerMacObservation, PersonalWorkerMacObservationAdapter,
    PersonalWorkerMacObservationClock, logical_cpu_command, vm_stat_command,
};
use glaeda::personal_worker_queue::{
    PERSONAL_WORKER_QUEUE_SCHEMA_VERSION, PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
    PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES, PersonalWorkerActivityEvidence,
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheLeaseState, PersonalWorkerCacheNamespace,
    PersonalWorkerJobClass, PersonalWorkerPriority, PersonalWorkerProfile,
    PersonalWorkerProfileObservation, PersonalWorkerQueueDecision, PersonalWorkerQueueEntryState,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerQueueVisibility,
    PersonalWorkerSelection,
};
use glaeda::personal_worker_store::{
    PersonalWorkerStore, PersonalWorkerStoreDocument, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreRevision,
};
use glaeda::personal_worker_tick::{
    PersonalWorkerTickInput, PersonalWorkerTickPlan, PersonalWorkerTickPolicy,
};
use glaeda::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::unix_personal_worker_store::lima_authority::{
    UnixPersonalWorkerLimaAuthorityErrorKind, UnixPersonalWorkerLimaAuthorityGuard,
};
use glaeda::verification_profile::{CacheId, VerificationProfileId};

const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const DISK_BYTES: u64 = 80 * 1024 * 1024 * 1024;
static NEXT_STATE_ROOT: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static LIMA_HOST_FIXTURE: LimaHostIdentityFixture =
        LimaHostIdentityFixture::new("lima-authority", "smolrunner");
}

fn lima_home() -> String {
    LIMA_HOST_FIXTURE.with(LimaHostIdentityFixture::lima_home_string)
}

struct TempStateRoot(PathBuf);

impl TempStateRoot {
    fn new() -> Self {
        let sequence = NEXT_STATE_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-lima-authority-store-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("state root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempStateRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ScriptedExecutor {
    outputs: RefCell<VecDeque<String>>,
    seen: RefCell<Vec<(CommandSpec, Duration)>>,
}

impl ScriptedExecutor {
    fn new(outputs: Vec<String>) -> Self {
        Self {
            outputs: RefCell::new(outputs.into()),
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other("untimed execution is forbidden"))
    }
}

impl TimedCommandExecutor for ScriptedExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        self.seen.borrow_mut().push((spec.clone(), timeout));
        let stdout = self
            .outputs
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| io::Error::other("missing scripted observation output"))?;
        Ok(ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout,
            stderr: String::new(),
        })
    }
}

struct ScriptedClock(RefCell<VecDeque<u64>>);

impl PersonalWorkerMacObservationClock for ScriptedClock {
    fn unix_millis(&self) -> io::Result<u64> {
        self.0
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| io::Error::other("missing scripted observation time"))
    }
}

fn digest(byte: &str) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", byte.repeat(64))).expect("digest")
}

fn epoch(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("epoch")
}

fn config() -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse("/Users/operator/Library/Application Support/SmolRunner")
            .expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/runner/workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        AvailabilityRequest::Away,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::CodesOnly,
    )
    .expect("config")
}

fn request(home: &str, cache: &str) -> LimaObservationRequest {
    LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        cache,
        30,
    )
    .expect("request")
}

fn broker_identity() -> LimaInstanceIdentity {
    LimaInstanceIdentity::new(
        LimaInstanceId::parse("smolrunner").expect("instance ID"),
        LimaCacheDiskIdentity::new(
            LimaCacheDiskId::parse("smolrunner-cache").expect("cache disk ID"),
            digest("b"),
        ),
    )
}

fn lifecycle_profile(
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    observed_at: u64,
) -> LimaLifecycleObservation {
    lifecycle_with_generation(state, profile, observed_at, 1)
}

fn lifecycle_with_generation(
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    observed_at: u64,
    generation: u64,
) -> LimaLifecycleObservation {
    let last_activity_at = epoch(1_000);
    LimaLifecycleObservation::new(LimaLifecycleObservationDefinition {
        identity: broker_identity(),
        state,
        profile,
        profile_generation: LimaProfileGeneration::new(generation).expect("generation"),
        observed_resources: LimaObservedResources::for_profile(profile),
        observed_at: epoch(observed_at),
        active_reservation_id: None,
        last_activity_at,
        idle_deadline: epoch(last_activity_at.get() + profile.idle_deadline_offset_millis()),
        graceful_stop_acknowledgement: None,
    })
    .expect("lifecycle")
}

fn host_outputs() -> Vec<String> {
    vec![
        "1\n".to_owned(),
        "total = 4096.00M used = 1024.00M free = 3072.00M (encrypted)\n".to_owned(),
        "Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged;\n".to_owned(),
        " 100 1 1.0 2048 00:05 /opt/homebrew/bin/limactl\n".to_owned(),
        concat!(
            "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n",
            "Pages free: 100000.\n",
            "Pages active: 200000.\n",
            "Pages inactive: 300000.\n",
            "Pages speculative: 100000.\n",
            "Pages purgeable: 50000.\n",
        )
        .to_owned(),
        "10\n".to_owned(),
    ]
}

fn instance_json(state: &str, profile: LimaResourceProfile) -> String {
    let envelope = profile.envelope();
    format!(
        "{{\"name\":\"smolrunner\",\"status\":\"{state}\",\"dir\":\"{}/smolrunner\",\"vmType\":\"vz\",\"arch\":\"aarch64\",\"cpus\":{},\"memory\":{},\"disk\":{DISK_BYTES},\"errors\":[]}}\n",
        lima_home(),
        envelope.vcpus,
        envelope.memory_bytes
    )
}

fn running_outputs(profile: LimaResourceProfile) -> Vec<String> {
    let envelope = profile.envelope();
    let mut outputs = host_outputs();
    outputs.extend([
        instance_json("Running", profile),
        "aarch64\n".to_owned(),
        format!("{}\n", envelope.vcpus),
        "4096\n".to_owned(),
        format!("{}\n", (envelope.memory_bytes - 16 * 1024 * 1024) / 4_096),
        format!("{}  /etc/machine-id\n", "a".repeat(64)),
        "2049:2\n".to_owned(),
        "2049:12345\n".to_owned(),
        instance_json("Running", profile),
    ]);
    outputs
}

fn stopped_outputs(profile: LimaResourceProfile) -> Vec<String> {
    let mut outputs = host_outputs();
    outputs.extend([
        instance_json("Stopped", profile),
        instance_json("Stopped", profile),
    ]);
    outputs
}

fn mac_observation(running: bool, start: u64) -> PersonalWorkerMacObservation {
    mac_observation_profile(running, start, LimaResourceProfile::Interactive)
}

fn mac_observation_profile(
    running: bool,
    start: u64,
    profile: LimaResourceProfile,
) -> PersonalWorkerMacObservation {
    let executor = ScriptedExecutor::new(if running {
        running_outputs(profile)
    } else {
        stopped_outputs(profile)
    });
    let clock = ScriptedClock(RefCell::new(
        [start, start + 1_000, start + 2_000, start + 3_000].into(),
    ));
    let observation = PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(5))
        .expect("Mac adapter")
        .observe(
            &config(),
            &request(&lima_home(), CACHE_PATH),
            &LimaObservationAdapter::new("/opt/homebrew/bin/limactl").expect("Lima adapter"),
            &executor,
            &clock,
        )
        .expect("sealed B02 observation");
    assert!(executor.outputs.borrow().is_empty());
    assert!(
        executor
            .seen
            .borrow()
            .iter()
            .all(|(_, timeout)| *timeout == Duration::from_secs(5))
    );
    observation
}

fn worker_tick(lifecycle: &LimaLifecycleObservation) -> PersonalWorkerTickPlan {
    worker_tick_with_snapshot(lifecycle, 5, 7)
}

fn worker_tick_with_snapshot(
    lifecycle: &LimaLifecycleObservation,
    store_revision: u64,
    queue_generation: u64,
) -> PersonalWorkerTickPlan {
    let current_profile = if lifecycle.state() == LimaLifecycleState::Stopped {
        PersonalWorkerProfile::Stopped
    } else {
        match lifecycle.profile() {
            LimaResourceProfile::Interactive => PersonalWorkerProfile::Interactive,
            LimaResourceProfile::Work => PersonalWorkerProfile::Work,
        }
    };
    let request_id = ExecutionRequestId::parse("request-one").expect("request ID");
    let repository = RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository");
    let profile_id = VerificationProfileId::parse("smolrunner.required").expect("profile");
    let runner_profile = RunnerProfileId::parse("work").expect("runner profile");
    let namespace = PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("cargo-build").expect("cache ID"),
        repository: repository.clone(),
        namespace_digest: digest("c"),
    };
    let limits = ExecutionResourceLimits::new(2_000, 2 * 1024 * 1024 * 1024, 768).expect("limits");
    let selection = PersonalWorkerSelection {
        request_id: request_id.clone(),
        repository: repository.clone(),
        verification_profile_id: profile_id.clone(),
        runner_profile_id: runner_profile.clone(),
        priority: PersonalWorkerPriority::Normal,
        effective_priority_rank: 1,
        job_class: PersonalWorkerJobClass::Light,
        reserved_limits: limits,
        cache_namespace: namespace.clone(),
        cache_access: PersonalWorkerCacheAccessMode::Write,
    };
    let visibility = PersonalWorkerQueueVisibility {
        request_id,
        repository,
        commit: CommitId::parse(&"12".repeat(20)).expect("commit"),
        tree: GitTreeId::parse(&"34".repeat(20)).expect("tree"),
        verification_profile_id: profile_id,
        runner_profile_id: runner_profile,
        priority: PersonalWorkerPriority::Normal,
        effective_priority_rank: 1,
        age_millis: 10,
        state: PersonalWorkerQueueEntryState::Selected,
        queue_position: None,
        requested_cpu_millis: limits.cpu_millis,
        requested_memory_bytes: limits.memory_bytes,
        reserved_cpu_millis: None,
        reserved_memory_bytes: None,
        cache_namespace: namespace,
        cache_access: PersonalWorkerCacheAccessMode::Write,
        cache_lease: PersonalWorkerCacheLeaseState::Available,
        start_time: None,
        worker_profile: PersonalWorkerProfile::Work,
    };
    let queue = PersonalWorkerQueueDecision {
        schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
        generation: PersonalWorkerQueueGeneration::new(queue_generation).expect("queue generation"),
        observed_at: epoch(204_000),
        profile_observation: PersonalWorkerProfileObservation::observed(current_profile),
        activity_evidence: PersonalWorkerActivityEvidence::observed(epoch(203_000)),
        desired_profile: PersonalWorkerProfile::Work,
        cancel_pending_downscale: false,
        profile_change_permitted: true,
        schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
        schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        schedulable_pids: 3_072,
        selected: vec![selection],
        visibility: vec![visibility],
    };
    let stopped = lifecycle.state() == LimaLifecycleState::Stopped;
    let availability = MacAvailabilityObservation {
        effective_mode: if stopped {
            EffectiveAvailabilityMode::Off
        } else {
            EffectiveAvailabilityMode::Away
        },
        vm_power: if stopped {
            VmPowerState::Stopped
        } else {
            VmPowerState::Running
        },
        job_activity: JobActivity::Idle,
        freshness: ObservationFreshness::Fresh,
        host_power: HostPowerSource::Ac,
        memory_pressure: MemoryPressure::Normal,
        operator_hold: false,
    };
    let capacity = HostCapacityObservation::new(
        epoch(204_000),
        ExecutionResourceLimits::new(8_000, 12 * 1024 * 1024 * 1024, 4_096).expect("capacity"),
    );
    let runner = (!stopped).then(|| {
        HostBrokerRunnerObservation::new(
            broker_identity().instance_id().clone(),
            lifecycle.profile_generation(),
            epoch(204_000),
            HostBrokerRunnerState::IdleReady,
        )
    });
    PersonalWorkerTickPolicy::new(30_000, 30_000, 30_000)
        .expect("tick policy")
        .plan(PersonalWorkerTickInput {
            store_revision: PersonalWorkerStoreRevision::new(store_revision).expect("revision"),
            decision_at: epoch(205_000),
            queue: &queue,
            lifecycle_policy: &LimaLifecyclePolicy::new(30_000).expect("lifecycle policy"),
            lifecycle: Some(lifecycle),
            runner: runner.as_ref(),
            capacity: Some(capacity),
            availability_request: AvailabilityRequest::Active,
            availability: Some(availability),
        })
        .expect("sealed B01 lifecycle tick")
}

fn stop_tick(lifecycle: &LimaLifecycleObservation) -> PersonalWorkerTickPlan {
    let queue = PersonalWorkerQueueDecision {
        schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
        generation: PersonalWorkerQueueGeneration::new(7).expect("queue generation"),
        observed_at: epoch(1_900_000),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Work,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(epoch(1_000)),
        desired_profile: PersonalWorkerProfile::Stopped,
        cancel_pending_downscale: false,
        profile_change_permitted: true,
        schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
        schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        schedulable_pids: 3_072,
        selected: Vec::new(),
        visibility: Vec::new(),
    };
    let runner = HostBrokerRunnerObservation::new(
        broker_identity().instance_id().clone(),
        LimaProfileGeneration::new(1).expect("generation"),
        epoch(1_900_000),
        HostBrokerRunnerState::Offline,
    );
    PersonalWorkerTickPolicy::new(30_000, 30_000, 30_000)
        .expect("tick policy")
        .plan(PersonalWorkerTickInput {
            store_revision: PersonalWorkerStoreRevision::new(5).expect("revision"),
            decision_at: epoch(1_900_001),
            queue: &queue,
            lifecycle_policy: &LimaLifecyclePolicy::new(30_000).expect("lifecycle policy"),
            lifecycle: Some(lifecycle),
            runner: Some(&runner),
            capacity: None,
            availability_request: AvailabilityRequest::Off,
            availability: Some(MacAvailabilityObservation {
                effective_mode: EffectiveAvailabilityMode::Active,
                vm_power: VmPowerState::Running,
                job_activity: JobActivity::Idle,
                freshness: ObservationFreshness::Fresh,
                host_power: HostPowerSource::Ac,
                memory_pressure: MemoryPressure::Normal,
                operator_hold: false,
            }),
        })
        .expect("sealed B01 stop tick")
}

fn enrolled() -> PersonalWorkerLimaAuthorityDocument {
    let config = config();
    let running = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation = personal_worker_lima_enrollment_confirmation(
        &config,
        &running,
        &request,
        broker_identity(),
    )
    .expect("confirmation");
    assert_eq!(
        confirmation.schema_version(),
        PERSONAL_WORKER_LIMA_AUTHORITY_SCHEMA_VERSION
    );
    assert!(
        confirmation
            .value()
            .starts_with("personal-worker-lima-enrollment-v3.sha256:")
    );
    PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &running,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("enrollment")
    .into_document()
}

fn assert_recovery(
    document: &PersonalWorkerLimaAuthorityDocument,
    disposition: PersonalWorkerLimaRecoveryDisposition,
    phase: PersonalWorkerLimaAttemptPhase,
    checkpoint_at: u64,
) {
    let report = document.recovery_report();
    let attempt = document.attempt().expect("durable attempt");
    let binding = report.attempt().expect("recovery attempt binding");
    assert_eq!(
        report.schema_version(),
        PERSONAL_WORKER_LIMA_RECOVERY_SCHEMA_VERSION
    );
    assert_eq!(
        report.authority_generation(),
        document.authority_generation()
    );
    assert_eq!(report.disposition(), disposition);
    assert_eq!(binding.generation(), attempt.generation());
    assert_eq!(binding.store_revision(), attempt.store_revision());
    assert_eq!(binding.queue_generation(), attempt.queue_generation());
    assert_eq!(binding.action(), attempt.action());
    assert_eq!(binding.phase(), phase);
    assert_eq!(binding.checkpoint_at(), epoch(checkpoint_at));
}

fn persist_completed_work_attempt(
    guard: &mut UnixPersonalWorkerLimaAuthorityGuard,
) -> PersonalWorkerLimaAuthorityDocument {
    let config = config();
    let request = request(&lima_home(), CACHE_PATH);
    let before = lifecycle_profile(
        LimaLifecycleState::Running,
        LimaResourceProfile::Interactive,
        204_000,
    );
    let before_mac = mac_observation_profile(true, 200_000, LimaResourceProfile::Interactive);
    let tick = worker_tick_with_snapshot(
        &before,
        guard.store_revision().get(),
        guard.queue_generation().get(),
    );
    let mut current = guard.authority().expect("enrollment").clone();
    let prepared = current
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config,
            mac: &before_mac,
            request: &request,
            lifecycle: &before,
            tick: &tick,
        })
        .expect("prepare work transition");
    guard
        .replace_authority(current.authority_generation(), &prepared)
        .expect("persist prepared");
    current = prepared;
    let generation = current.attempt().expect("attempt").generation();
    for (phase, at) in [
        (PersonalWorkerLimaAttemptPhase::StopStarted, 206_000),
        (PersonalWorkerLimaAttemptPhase::StopCompleted, 207_000),
        (PersonalWorkerLimaAttemptPhase::EditStarted, 208_000),
        (PersonalWorkerLimaAttemptPhase::EditCompleted, 209_000),
        (PersonalWorkerLimaAttemptPhase::StartStarted, 210_000),
        (PersonalWorkerLimaAttemptPhase::StartCompleted, 211_000),
        (PersonalWorkerLimaAttemptPhase::VerifyStarted, 212_000),
    ] {
        let next = current
            .checkpoint(generation, phase, epoch(at))
            .expect("checkpoint work transition");
        guard
            .replace_authority(current.authority_generation(), &next)
            .expect("persist checkpoint");
        current = next;
    }
    let post_mac = mac_observation_profile(true, 214_000, LimaResourceProfile::Work);
    let completed = current
        .complete_attempt(
            generation,
            &config,
            &post_mac,
            &request,
            &lifecycle_with_generation(
                LimaLifecycleState::Running,
                LimaResourceProfile::Work,
                218_000,
                2,
            ),
            epoch(219_000),
        )
        .expect("complete work transition");
    guard
        .replace_authority(current.authority_generation(), &completed)
        .expect("persist completion");
    completed
}

#[test]
fn enrollment_uses_sealed_running_identity_and_has_canonical_private_encoding() {
    let running = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation = personal_worker_lima_enrollment_confirmation(
        &config(),
        &running,
        &request,
        broker_identity(),
    )
    .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        &config(),
        &running,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("enrollment");
    let document = enrollment.into_document();
    assert_eq!(
        document.persistent_identity(),
        match &running.report().lima.guest {
            glaeda::lima_observation::LimaGuestObservation::Observed(guest) => {
                &guest.persistent_identity
            }
            _ => panic!("running guest"),
        }
    );

    let bytes = encode_personal_worker_lima_authority(&document).expect("encode");
    let decoded = decode_personal_worker_lima_authority(&bytes).expect("decode");
    assert_eq!(decoded, document);
    let text = String::from_utf8(bytes).expect("JSON");
    let value: serde_json::Value = serde_json::from_str(&text).expect("authority JSON");
    assert_eq!(
        value["host_identity_digest"],
        running.lima_host_identity().digest().as_str()
    );
    let debug = format!("{document:?}");
    let instance_directory = format!("{}/smolrunner", lima_home());
    let lima_home_path = lima_home();
    for private in [
        lima_home_path.as_str(),
        CACHE_PATH,
        instance_directory.as_str(),
        "/Users/operator",
    ] {
        assert!(!text.contains(private), "encoding leaked {private}");
        assert!(!debug.contains(private), "Debug leaked {private}");
    }

    let unconfirmed = PersonalWorkerLimaAuthorityDocument::enroll(
        &config(),
        &running,
        &request,
        broker_identity(),
        None,
    )
    .expect_err("confirmation required");
    assert_eq!(
        unconfirmed.kind,
        PersonalWorkerLimaAuthorityErrorKind::ConfirmationRequired
    );
    let stopped = personal_worker_lima_enrollment_confirmation(
        &config(),
        &mac_observation(false, 200_000),
        &request,
        broker_identity(),
    )
    .expect_err("stopped evidence cannot enroll");
    assert_eq!(
        stopped.kind,
        PersonalWorkerLimaAuthorityErrorKind::InvalidInput
    );

    let other_identity = LimaInstanceIdentity::new(
        LimaInstanceId::parse("other").expect("other instance"),
        broker_identity().cache_disk().clone(),
    );
    let mismatch =
        personal_worker_lima_enrollment_confirmation(&config(), &running, &request, other_identity)
            .expect_err("logical and physical instances must match");
    assert_eq!(
        mismatch.kind,
        PersonalWorkerLimaAuthorityErrorKind::Conflict
    );
}

#[test]
fn complete_private_request_drift_is_refused_before_an_attempt() {
    let authority = enrolled();
    let stopped = mac_observation_profile(false, 200_000, LimaResourceProfile::Work);
    let stopped_lifecycle = lifecycle_profile(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        204_000,
    );
    let tick = worker_tick(&stopped_lifecycle);
    for drifted in [
        request("/Users/operator/.other-lima", CACHE_PATH),
        request(&lima_home(), "/home/runner/.cache/other"),
    ] {
        let error = authority
            .begin_attempt(PersonalWorkerLimaAttemptInput {
                config: &config(),
                mac: &stopped,
                request: &drifted,
                lifecycle: &stopped_lifecycle,
                tick: &tick,
            })
            .expect_err("request drift");
        assert_eq!(error.kind, PersonalWorkerLimaAuthorityErrorKind::Conflict);
    }
    assert!(authority.attempt().is_none());
}

#[test]
fn prepared_attempt_blocks_replay_and_follows_exact_durable_phase_graph() {
    let authority = enrolled();
    let clean = authority.recovery_report();
    assert_eq!(
        clean.disposition(),
        PersonalWorkerLimaRecoveryDisposition::Clean
    );
    assert!(clean.attempt().is_none());
    let before_mac = mac_observation_profile(true, 200_000, LimaResourceProfile::Interactive);
    let before = lifecycle_profile(
        LimaLifecycleState::Running,
        LimaResourceProfile::Interactive,
        204_000,
    );
    let tick = worker_tick(&before);
    assert!(
        matches!(
            tick.action(),
            glaeda::personal_worker_tick::PersonalWorkerTickAction::StopVm {
                current_profile: LimaResourceProfile::Interactive,
                target_after_stop: PersonalWorkerProfile::Work,
                ..
            }
        ),
        "unexpected running profile tick: {:?}",
        tick.action()
    );
    let prepared = authority
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &before_mac,
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &before,
            tick: &tick,
        })
        .expect("prepared attempt");
    let attempt = prepared.attempt().expect("attempt");
    assert_eq!(
        attempt.action(),
        glaeda::personal_worker_lima_authority::PersonalWorkerLimaAction::StopToWork
    );
    assert_eq!(attempt.store_revision().get(), 5);
    assert_eq!(attempt.queue_generation().get(), 7);
    assert_eq!(attempt.phase(), PersonalWorkerLimaAttemptPhase::Prepared);
    assert_recovery(
        &prepared,
        PersonalWorkerLimaRecoveryDisposition::ReobserveBeforeFirstMutation,
        PersonalWorkerLimaAttemptPhase::Prepared,
        205_000,
    );

    let replay = prepared
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &before_mac,
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &before,
            tick: &tick,
        })
        .expect_err("existing attempt blocks replay");
    assert_eq!(
        replay.kind,
        PersonalWorkerLimaAuthorityErrorKind::RecoveryRequired
    );

    let generation = attempt.generation();
    let stopping = prepared
        .checkpoint(
            generation,
            PersonalWorkerLimaAttemptPhase::StopStarted,
            epoch(206_000),
        )
        .expect("stop began");
    assert_recovery(
        &stopping,
        PersonalWorkerLimaRecoveryDisposition::ReobserveUncertainMutation,
        PersonalWorkerLimaAttemptPhase::StopStarted,
        206_000,
    );
    let stopped = stopping
        .checkpoint(
            generation,
            PersonalWorkerLimaAttemptPhase::StopCompleted,
            epoch(207_000),
        )
        .expect("stop completed");
    assert_recovery(
        &stopped,
        PersonalWorkerLimaRecoveryDisposition::ReobserveCheckpointedMutation,
        PersonalWorkerLimaAttemptPhase::StopCompleted,
        207_000,
    );
    let editing = stopped
        .checkpoint(
            generation,
            PersonalWorkerLimaAttemptPhase::EditStarted,
            epoch(208_000),
        )
        .expect("edit began");
    assert_recovery(
        &editing,
        PersonalWorkerLimaRecoveryDisposition::ReobserveUncertainMutation,
        PersonalWorkerLimaAttemptPhase::EditStarted,
        208_000,
    );
    let edited = editing
        .checkpoint(
            generation,
            PersonalWorkerLimaAttemptPhase::EditCompleted,
            epoch(209_000),
        )
        .expect("edit completed");
    assert_recovery(
        &edited,
        PersonalWorkerLimaRecoveryDisposition::ReobserveCheckpointedMutation,
        PersonalWorkerLimaAttemptPhase::EditCompleted,
        209_000,
    );
    let starting = edited
        .checkpoint(
            generation,
            PersonalWorkerLimaAttemptPhase::StartStarted,
            epoch(210_000),
        )
        .expect("start began");
    assert_recovery(
        &starting,
        PersonalWorkerLimaRecoveryDisposition::ReobserveUncertainMutation,
        PersonalWorkerLimaAttemptPhase::StartStarted,
        210_000,
    );
    let started = starting
        .checkpoint(
            generation,
            PersonalWorkerLimaAttemptPhase::StartCompleted,
            epoch(211_000),
        )
        .expect("start completed");
    assert_recovery(
        &started,
        PersonalWorkerLimaRecoveryDisposition::ReobserveCheckpointedMutation,
        PersonalWorkerLimaAttemptPhase::StartCompleted,
        211_000,
    );
    let verifying = started
        .checkpoint(
            generation,
            PersonalWorkerLimaAttemptPhase::VerifyStarted,
            epoch(212_000),
        )
        .expect("verification began");
    assert_recovery(
        &verifying,
        PersonalWorkerLimaRecoveryDisposition::Reverify,
        PersonalWorkerLimaAttemptPhase::VerifyStarted,
        212_000,
    );
    let recovery_json = serde_json::to_string(&verifying.recovery_report()).expect("JSON");
    let recovery_debug = format!("{:?}", verifying.recovery_report());
    let machine_identity = "a".repeat(64);
    let cache_identity = "b".repeat(64);
    let instance_directory = format!("{}/smolrunner", lima_home());
    let lima_home_path = lima_home();
    for forbidden in [
        lima_home_path.as_str(),
        CACHE_PATH,
        instance_directory.as_str(),
        machine_identity.as_str(),
        cache_identity.as_str(),
        "/proc/self/fd",
    ] {
        assert!(!recovery_json.contains(forbidden));
        assert!(!recovery_debug.contains(forbidden));
    }
    let running = mac_observation_profile(true, 214_000, LimaResourceProfile::Work);
    let completed = verifying
        .complete_attempt(
            generation,
            &config(),
            &running,
            &request(&lima_home(), CACHE_PATH),
            &lifecycle_with_generation(
                LimaLifecycleState::Running,
                LimaResourceProfile::Work,
                218_000,
                2,
            ),
            epoch(219_000),
        )
        .expect("verified completion");
    assert_eq!(
        completed.attempt().expect("attempt").phase(),
        PersonalWorkerLimaAttemptPhase::Completed
    );
    assert_recovery(
        &completed,
        PersonalWorkerLimaRecoveryDisposition::SettleCompletedAttempt,
        PersonalWorkerLimaAttemptPhase::Completed,
        219_000,
    );
    assert_eq!(
        decode_personal_worker_lima_authority(
            &encode_personal_worker_lima_authority(&completed).expect("encode")
        )
        .expect("decode"),
        completed
    );
}

#[test]
fn stopped_start_and_profile_change_require_the_enrolled_host_identity() {
    let authority = enrolled();
    let before = lifecycle_profile(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        204_000,
    );
    let tick = worker_tick(&before);
    assert!(matches!(
        tick.action(),
        glaeda::personal_worker_tick::PersonalWorkerTickAction::ChangeProfile {
            from_profile: LimaResourceProfile::Interactive,
            to_profile: LimaResourceProfile::Work,
            ..
        }
    ));
    let prepared_edit = authority
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &mac_observation(false, 200_000),
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &before,
            tick: &tick,
        })
        .expect("stopped edit uses current immutable ownership proof");
    assert_eq!(
        prepared_edit.attempt().expect("attempt").action(),
        glaeda::personal_worker_lima_authority::PersonalWorkerLimaAction::EditToWork
    );
    assert_eq!(
        decode_personal_worker_lima_authority(
            &encode_personal_worker_lima_authority(&prepared_edit).expect("encode stopped edit")
        )
        .expect("decode stopped edit"),
        prepared_edit
    );
    let edit_generation = prepared_edit.attempt().expect("attempt").generation();
    let editing = prepared_edit
        .checkpoint(
            edit_generation,
            PersonalWorkerLimaAttemptPhase::EditStarted,
            epoch(206_000),
        )
        .expect("edit started");
    let edited = editing
        .checkpoint(
            edit_generation,
            PersonalWorkerLimaAttemptPhase::EditCompleted,
            epoch(207_000),
        )
        .expect("edit completed");
    let verifying = edited
        .checkpoint(
            edit_generation,
            PersonalWorkerLimaAttemptPhase::VerifyStarted,
            epoch(208_000),
        )
        .expect("verification started");
    let completed_edit = verifying
        .complete_attempt(
            edit_generation,
            &config(),
            &mac_observation_profile(false, 210_000, LimaResourceProfile::Work),
            &request(&lima_home(), CACHE_PATH),
            &lifecycle_with_generation(
                LimaLifecycleState::Stopped,
                LimaResourceProfile::Work,
                214_000,
                2,
            ),
            epoch(215_000),
        )
        .expect("stopped edit verified");
    assert_eq!(
        decode_personal_worker_lima_authority(
            &encode_personal_worker_lima_authority(&completed_edit)
                .expect("encode completed stopped edit")
        )
        .expect("decode completed stopped edit"),
        completed_edit
    );
    assert!(authority.attempt().is_none());

    let stopped_work = lifecycle_profile(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        204_000,
    );
    let start_tick = worker_tick(&stopped_work);
    assert!(matches!(
        start_tick.action(),
        glaeda::personal_worker_tick::PersonalWorkerTickAction::StartVm { .. }
    ));
    let prepared_start = authority
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &mac_observation_profile(false, 200_000, LimaResourceProfile::Work),
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &stopped_work,
            tick: &start_tick,
        })
        .expect("stopped start uses current immutable ownership proof");
    assert_eq!(
        prepared_start.attempt().expect("attempt").action(),
        glaeda::personal_worker_lima_authority::PersonalWorkerLimaAction::Start
    );
    assert_eq!(
        decode_personal_worker_lima_authority(
            &encode_personal_worker_lima_authority(&prepared_start).expect("encode stopped start")
        )
        .expect("decode stopped start"),
        prepared_start
    );
    let start_generation = prepared_start.attempt().expect("attempt").generation();
    let starting = prepared_start
        .checkpoint(
            start_generation,
            PersonalWorkerLimaAttemptPhase::StartStarted,
            epoch(206_000),
        )
        .expect("start started");
    let started = starting
        .checkpoint(
            start_generation,
            PersonalWorkerLimaAttemptPhase::StartCompleted,
            epoch(207_000),
        )
        .expect("start completed");
    let verifying = started
        .checkpoint(
            start_generation,
            PersonalWorkerLimaAttemptPhase::VerifyStarted,
            epoch(208_000),
        )
        .expect("verification started");
    let completed_start = verifying
        .complete_attempt(
            start_generation,
            &config(),
            &mac_observation_profile(true, 210_000, LimaResourceProfile::Work),
            &request(&lima_home(), CACHE_PATH),
            &lifecycle_with_generation(
                LimaLifecycleState::Running,
                LimaResourceProfile::Work,
                214_000,
                1,
            ),
            epoch(215_000),
        )
        .expect("stopped start verified");
    assert_eq!(
        decode_personal_worker_lima_authority(
            &encode_personal_worker_lima_authority(&completed_start)
                .expect("encode completed stopped start")
        )
        .expect("decode completed stopped start"),
        completed_start
    );
}

#[test]
fn stopped_mutation_refuses_same_path_host_identity_replacement() {
    let authority = enrolled();
    LIMA_HOST_FIXTURE.with(|fixture| fixture.rewrite_disk_identity("smolrunner", 222));
    let before = lifecycle_profile(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        204_000,
    );
    let tick = worker_tick(&before);
    let error = authority
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &mac_observation(false, 200_000),
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &before,
            tick: &tick,
        })
        .expect_err("replacement host identity cannot authorize stopped mutation");
    assert_eq!(error.kind, PersonalWorkerLimaAuthorityErrorKind::Conflict);
    assert!(authority.attempt().is_none());
}

#[test]
fn stop_attempt_preserves_the_exact_b01_target_after_stop() {
    let authority = enrolled();
    let before = lifecycle_profile(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        1_900_000,
    );
    let tick = stop_tick(&before);
    assert!(matches!(
        tick.action(),
        glaeda::personal_worker_tick::PersonalWorkerTickAction::StopVm {
            target_after_stop: PersonalWorkerProfile::Stopped,
            ..
        }
    ));
    let prepared = authority
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &mac_observation_profile(true, 1_896_000, LimaResourceProfile::Work),
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &before,
            tick: &tick,
        })
        .expect("prepared exact stop");
    assert!(matches!(
        prepared.attempt().expect("attempt").action(),
        glaeda::personal_worker_lima_authority::PersonalWorkerLimaAction::StopToStopped
    ));
    assert!(
        String::from_utf8(encode_personal_worker_lima_authority(&prepared).expect("encode"))
            .expect("JSON")
            .contains("stop_to_stopped")
    );
}

#[test]
fn exhausted_authority_generation_refuses_without_reusing_a_generation() {
    let canonical = String::from_utf8(
        encode_personal_worker_lima_authority(&enrolled()).expect("canonical authority"),
    )
    .expect("UTF-8 JSON");
    let exhausted_bytes = canonical
        .replacen(
            "\"authority_generation\":1",
            "\"authority_generation\":1000000000000",
            1,
        )
        .into_bytes();
    let exhausted =
        decode_personal_worker_lima_authority(&exhausted_bytes).expect("bounded maximum decodes");
    let before_mac = mac_observation_profile(true, 200_000, LimaResourceProfile::Interactive);
    let before = lifecycle_profile(
        LimaLifecycleState::Running,
        LimaResourceProfile::Interactive,
        204_000,
    );
    let tick = worker_tick(&before);
    let error = exhausted
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &before_mac,
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &before,
            tick: &tick,
        })
        .expect_err("an exhausted generation must not saturate and replay");
    assert_eq!(
        error.kind,
        PersonalWorkerLimaAuthorityErrorKind::InvalidInput
    );
    assert_eq!(exhausted.authority_generation().get(), 1_000_000_000_000);
    assert!(exhausted.attempt().is_none());
}

#[test]
fn settlement_reserves_its_clear_generation_before_publishing() {
    let root = TempStateRoot::new();
    let worker = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(90_000),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("initial worker");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect("initialize worker store");

    let config = config();
    let mac = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation =
        personal_worker_lima_enrollment_confirmation(&config, &mac, &request, broker_identity())
            .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &mac,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("confirmed enrollment");
    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("guard");
    guard
        .publish_enrollment(enrollment, epoch(104_000))
        .expect("publish enrollment");
    persist_completed_work_attempt(&mut guard);
    drop(guard);

    let authority_path = root.path().join("personal-worker/lima-authority.json");
    let completed = fs::read_to_string(&authority_path).expect("completed authority bytes");
    let penultimate = completed
        .replacen(
            "\"authority_generation\":10",
            "\"authority_generation\":999999999999",
            1,
        )
        .replacen("\"generation\":1", "\"generation\":999999999990", 1);
    decode_personal_worker_lima_authority(penultimate.as_bytes())
        .expect("penultimate completed authority is canonical");
    fs::write(&authority_path, penultimate).expect("write bounded authority fixture");
    fs::set_permissions(&authority_path, fs::Permissions::from_mode(0o600))
        .expect("private authority mode");

    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("reopen guard");
    let error = guard
        .settle_completed_attempt()
        .expect_err("settlement must reserve both remaining generations");
    assert_eq!(
        error.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::RevisionConflict
    );
    let retained = guard.authority().expect("completed authority retained");
    assert_eq!(retained.authority_generation().get(), 999_999_999_999);
    assert!(retained.settlement().is_none());
    assert!(!root.path().join("personal-worker/.next.json").exists());
    assert!(
        !root
            .path()
            .join("personal-worker/.lima-authority.next.json")
            .exists()
    );
}

#[test]
fn strict_decode_binds_each_phase_to_its_exact_durable_generation_delta() {
    let authority = enrolled();
    let before_mac = mac_observation_profile(true, 200_000, LimaResourceProfile::Interactive);
    let before = lifecycle_profile(
        LimaLifecycleState::Running,
        LimaResourceProfile::Interactive,
        204_000,
    );
    let tick = worker_tick(&before);
    let prepared = authority
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config(),
            mac: &before_mac,
            request: &request(&lima_home(), CACHE_PATH),
            lifecycle: &before,
            tick: &tick,
        })
        .expect("prepared attempt");
    let canonical = String::from_utf8(
        encode_personal_worker_lima_authority(&prepared).expect("canonical prepared authority"),
    )
    .expect("UTF-8 JSON");

    let forged_phase = canonical
        .replacen("\"phase\":\"prepared\"", "\"phase\":\"stop_completed\"", 1)
        .into_bytes();
    assert_eq!(
        decode_personal_worker_lima_authority(&forged_phase)
            .expect_err("phase cannot advance without its durable generations")
            .kind,
        PersonalWorkerLimaAuthorityErrorKind::CorruptDocument
    );

    let forged_generation = canonical
        .replacen(
            "\"authority_generation\":2",
            "\"authority_generation\":999",
            1,
        )
        .into_bytes();
    assert_eq!(
        decode_personal_worker_lima_authority(&forged_generation)
            .expect_err("prepared must be exactly one generation after its attempt")
            .kind,
        PersonalWorkerLimaAuthorityErrorKind::CorruptDocument
    );
}

#[test]
fn unknown_version_unknown_fields_and_noncanonical_bytes_fail_closed() {
    let bytes = encode_personal_worker_lima_authority(&enrolled()).expect("encode");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
    value["schema_version"] = serde_json::json!(4);
    let version = serde_json::to_vec(&value).expect("version bytes");
    assert_eq!(
        decode_personal_worker_lima_authority(&version)
            .expect_err("unknown version")
            .kind,
        PersonalWorkerLimaAuthorityErrorKind::VersionIncompatible
    );

    for previous_version in [1, 2] {
        value = serde_json::from_slice(&bytes).expect("current JSON");
        value["schema_version"] = serde_json::json!(previous_version);
        value
            .as_object_mut()
            .expect("authority object")
            .remove("host_identity_digest");
        let previous = serde_json::to_vec(&value).expect("real previous version bytes");
        assert_eq!(
            decode_personal_worker_lima_authority(&previous)
                .expect_err("previous version requires explicit migration")
                .kind,
            PersonalWorkerLimaAuthorityErrorKind::VersionIncompatible
        );
    }

    value = serde_json::from_slice(&bytes).expect("current JSON");
    value["identity"]["instance_id"] = serde_json::json!("other");
    assert_eq!(
        decode_personal_worker_lima_authority(
            &serde_json::to_vec(&value).expect("identity drift bytes")
        )
        .expect_err("logical and physical persisted instances must match")
        .kind,
        PersonalWorkerLimaAuthorityErrorKind::CorruptDocument
    );

    value["identity"]["instance_id"] = serde_json::json!("smolrunner");
    value["unknown"] = serde_json::json!(true);
    assert_eq!(
        decode_personal_worker_lima_authority(&serde_json::to_vec(&value).expect("unknown bytes"))
            .expect_err("unknown field")
            .kind,
        PersonalWorkerLimaAuthorityErrorKind::CorruptDocument
    );

    let mut noncanonical = bytes;
    noncanonical.push(b'\n');
    assert_eq!(
        decode_personal_worker_lima_authority(&noncanonical)
            .expect_err("noncanonical bytes")
            .kind,
        PersonalWorkerLimaAuthorityErrorKind::CorruptDocument
    );
}

#[test]
fn unix_sidecar_publishes_only_confirmed_enrollment_under_the_canonical_store_lock() {
    let root = TempStateRoot::new();
    let worker = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(90_000),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("initial worker");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect("initialize worker store");

    let config = config();
    let mac = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation =
        personal_worker_lima_enrollment_confirmation(&config, &mac, &request, broker_identity())
            .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &mac,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("confirmed enrollment");

    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("guard");
    assert_eq!(guard.store_revision(), worker.revision());
    assert_eq!(guard.queue_generation(), worker.queue().generation);
    assert!(guard.authority().is_none());
    assert!(!guard.has_active_work());
    assert!(!guard.recovery_required());
    guard
        .publish_enrollment(enrollment, epoch(104_000))
        .expect("publish confirmed enrollment");
    let published = guard.authority().expect("published authority").clone();
    let lifecycle = lifecycle_profile(
        LimaLifecycleState::Running,
        LimaResourceProfile::Interactive,
        204_000,
    );
    let mac = mac_observation_profile(true, 200_000, LimaResourceProfile::Interactive);
    let wrong_snapshot = published
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config,
            mac: &mac,
            request: &request,
            lifecycle: &lifecycle,
            tick: &worker_tick(&lifecycle),
        })
        .expect("valid but stale worker snapshot attempt");
    let mismatch = guard
        .replace_authority(published.authority_generation(), &wrong_snapshot)
        .expect_err("locked worker revision and generation must match Prepared");
    assert_eq!(
        mismatch.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::RevisionConflict
    );

    let exact_tick = worker_tick_with_snapshot(&lifecycle, 1, 1);
    let prepared = published
        .begin_attempt(PersonalWorkerLimaAttemptInput {
            config: &config,
            mac: &mac,
            request: &request,
            lifecycle: &lifecycle,
            tick: &exact_tick,
        })
        .expect("exact locked worker snapshot attempt");
    guard
        .replace_authority(published.authority_generation(), &prepared)
        .expect("publish exact Prepared attempt");

    let busy = UnixPersonalWorkerStore::open_or_create(root.path())
        .expect_err("the lifecycle guard retains the canonical writer lock");
    assert_eq!(busy.kind(), PersonalWorkerStoreErrorKind::Busy);
    drop(guard);

    let lifecycle_barrier = UnixPersonalWorkerStore::open_or_create(root.path())
        .expect_err("ordinary worker recovery cannot overtake a lifecycle attempt");
    assert_eq!(
        lifecycle_barrier.kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );

    let worker_stage_path = root.path().join("personal-worker/.next.json");
    fs::copy(
        root.path().join("personal-worker/current.json"),
        &worker_stage_path,
    )
    .expect("write duplicate worker crash stage");
    fs::set_permissions(&worker_stage_path, fs::Permissions::from_mode(0o600))
        .expect("private worker crash stage");
    let joint_recovery = UnixPersonalWorkerStore::open_or_create(root.path())
        .expect_err("unsettled authority must block worker-stage cleanup");
    assert_eq!(
        joint_recovery.kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );
    assert!(
        worker_stage_path.exists(),
        "worker recovery evidence must remain untouched"
    );
    fs::remove_file(&worker_stage_path).expect("remove test-only worker stage");

    let stop_started = prepared
        .checkpoint(
            prepared.attempt().expect("attempt").generation(),
            PersonalWorkerLimaAttemptPhase::StopStarted,
            epoch(206_000),
        )
        .expect("durable successor");
    let stage_path = root
        .path()
        .join("personal-worker/.lima-authority.next.json");
    fs::write(
        &stage_path,
        encode_personal_worker_lima_authority(&stop_started).expect("stage bytes"),
    )
    .expect("write crash stage");
    fs::set_permissions(&stage_path, fs::Permissions::from_mode(0o600))
        .expect("private crash stage");

    let reopened = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("reopen guard");
    assert_eq!(reopened.authority(), Some(&stop_started));
    assert!(!stage_path.exists());
    let mode = fs::metadata(root.path().join("personal-worker/lima-authority.json"))
        .expect("authority metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    drop(reopened);

    fs::write(
        &stage_path,
        encode_personal_worker_lima_authority(&published).expect("conflicting stage bytes"),
    )
    .expect("write conflicting stage");
    fs::set_permissions(&stage_path, fs::Permissions::from_mode(0o600))
        .expect("private conflicting stage");
    let recovery = UnixPersonalWorkerLimaAuthorityGuard::open(root.path())
        .expect_err("a canonical non-successor stage is recovery debt");
    assert_eq!(
        recovery.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::RecoveryRequired
    );
    assert!(stage_path.exists(), "recovery evidence must be preserved");
}

#[test]
fn unix_sidecar_requires_fresh_enrollment_evidence_at_publication() {
    let root = TempStateRoot::new();
    let worker = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(90_000),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("initial worker");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect("initialize worker store");

    let config = config();
    let mac = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation =
        personal_worker_lima_enrollment_confirmation(&config, &mac, &request, broker_identity())
            .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &mac,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("confirmed enrollment");
    let stale_time = epoch(
        mac.report()
            .timing
            .expires_at_millis
            .checked_add(1)
            .expect("bounded stale time"),
    );

    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("guard");
    let stale = guard
        .publish_enrollment(enrollment, stale_time)
        .expect_err("stale enrollment cannot become durable authority");
    assert_eq!(
        stale.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::InvalidDocument
    );
    assert!(guard.authority().is_none());
    assert!(
        !root
            .path()
            .join("personal-worker/lima-authority.json")
            .exists()
    );
}

#[test]
fn unix_sidecar_refuses_to_splice_orphan_authority_onto_a_fresh_worker() {
    let root = TempStateRoot::new();
    let worker = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(90_000),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("initial worker");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect("initialize worker store");

    let config = config();
    let mac = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation =
        personal_worker_lima_enrollment_confirmation(&config, &mac, &request, broker_identity())
            .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &mac,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("confirmed enrollment");
    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("guard");
    guard
        .publish_enrollment(enrollment, epoch(104_000))
        .expect("publish enrollment");
    drop(guard);

    let worker_path = root.path().join("personal-worker/current.json");
    fs::remove_file(&worker_path).expect("simulate lost worker document");
    let reinitialize = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect_err("orphan authority must prevent worker reinitialization");
    assert_eq!(
        reinitialize.kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );
    assert!(!worker_path.exists());
    assert!(
        root.path()
            .join("personal-worker/lima-authority.json")
            .exists()
    );
    let reopen = UnixPersonalWorkerLimaAuthorityGuard::open(root.path())
        .expect_err("the impossible combined state requires recovery");
    assert_eq!(
        reopen.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::RecoveryRequired
    );
}

#[test]
fn unix_completed_settlement_recovers_exact_worker_successor_before_clearing_authority() {
    let root = TempStateRoot::new();
    let worker = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(90_000),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("initial worker");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect("initialize worker store");

    let config = config();
    let mac = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation =
        personal_worker_lima_enrollment_confirmation(&config, &mac, &request, broker_identity())
            .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &mac,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("confirmed enrollment");
    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("guard");
    guard
        .publish_enrollment(enrollment, epoch(104_000))
        .expect("publish enrollment");
    let completed = persist_completed_work_attempt(&mut guard);
    assert_eq!(
        completed.attempt().expect("attempt").phase(),
        PersonalWorkerLimaAttemptPhase::Completed
    );

    let worker_stage = root.path().join("personal-worker/.next.json");
    fs::write(&worker_stage, []).expect("inject conflicting worker stage");
    fs::set_permissions(&worker_stage, fs::Permissions::from_mode(0o600))
        .expect("private worker stage");
    let interrupted = guard
        .settle_completed_attempt()
        .expect_err("conflicting worker stage interrupts settlement after its checkpoint");
    assert_eq!(
        interrupted.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::CorruptState
    );
    assert!(guard.recovery_required());
    assert!(
        guard
            .authority()
            .expect("prepared settlement authority")
            .settlement()
            .is_some()
    );
    assert_eq!(
        guard
            .authority()
            .expect("prepared settlement authority")
            .recovery_report()
            .disposition(),
        PersonalWorkerLimaRecoveryDisposition::RecoverPreparedSettlement
    );
    fs::remove_file(&worker_stage).expect("remove test-only conflicting stage");
    drop(guard);

    let recovered = UnixPersonalWorkerLimaAuthorityGuard::open(root.path())
        .expect("reopen deterministically finishes exact settlement");
    assert_eq!(recovered.store_revision().get(), 2);
    assert_eq!(recovered.queue_generation().get(), 2);
    let authority = recovered.authority().expect("settled enrollment remains");
    assert!(authority.attempt().is_none());
    assert!(authority.settlement().is_none());
    assert_eq!(
        authority.recovery_report().disposition(),
        PersonalWorkerLimaRecoveryDisposition::Clean
    );
    let busy = UnixPersonalWorkerStore::open_or_create(root.path())
        .expect_err("settlement guard still owns the canonical writer lock");
    assert_eq!(busy.kind(), PersonalWorkerStoreErrorKind::Busy);
    drop(recovered);

    let (store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("settled store");
    let settled_worker = store.load().expect("load settled worker").expect("worker");
    assert_eq!(settled_worker.revision().get(), 2);
    assert_eq!(settled_worker.queue().generation.get(), 2);
    assert_eq!(
        settled_worker.queue().profile_observation,
        PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Work)
    );
    assert_eq!(settled_worker.queue().observed_at, epoch(219_000));
    assert_eq!(
        settled_worker.queue().activity_evidence,
        PersonalWorkerActivityEvidence::Never
    );
}

#[test]
fn unix_sidecar_poisoned_publication_guard_cannot_retry_after_ambiguous_failure() {
    let root = TempStateRoot::new();
    let worker = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(90_000),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("initial worker");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect("initialize worker store");

    let config = config();
    let mac = mac_observation(true, 100_000);
    let request = request(&lima_home(), CACHE_PATH);
    let confirmation =
        personal_worker_lima_enrollment_confirmation(&config, &mac, &request, broker_identity())
            .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &mac,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("confirmed enrollment");
    let stage = root
        .path()
        .join("personal-worker/.lima-authority.next.json");
    fs::write(&stage, []).expect("pre-existing uncertain stage");
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).expect("private stage");

    let guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path())
        .expect_err("open must classify malformed pre-existing stage");
    assert_eq!(
        guard.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::CorruptState
    );
    fs::remove_file(&stage).expect("remove test-only malformed stage");

    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root.path()).expect("clean guard");
    fs::write(&stage, []).expect("race stage after lock acquisition");
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).expect("private race stage");
    let failure = guard
        .publish_enrollment(enrollment, epoch(104_000))
        .expect_err("existing stage makes publication uncertain");
    assert_eq!(
        failure.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::RecoveryRequired
    );
    assert!(guard.recovery_required());

    let confirmation =
        personal_worker_lima_enrollment_confirmation(&config, &mac, &request, broker_identity())
            .expect("second confirmation");
    let retry = PersonalWorkerLimaAuthorityDocument::enroll(
        &config,
        &mac,
        &request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("second confirmed enrollment");
    assert_eq!(
        guard
            .publish_enrollment(retry, epoch(104_000))
            .expect_err("poisoned guard cannot retry")
            .kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::RecoveryRequired
    );
}

#[test]
fn unix_sidecar_refuses_unsettled_disposable_catalog_before_recovery() {
    let root = TempStateRoot::new();
    let worker = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(90_000),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("initial worker");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &worker)
        .expect("initialize worker store");

    let stage = root
        .path()
        .join("personal-worker/.disposable-attempt-catalog.next.json");
    fs::write(
        &stage,
        encode_disposable_attempt_catalog(&DisposableAttemptCatalogDocument::empty())
            .expect("canonical empty catalog"),
    )
    .expect("write staged catalog");
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).expect("private stage");

    let error = UnixPersonalWorkerLimaAuthorityGuard::open(root.path())
        .expect_err("unsettled disposable catalog must block Lima authority");
    assert_eq!(
        error.kind(),
        UnixPersonalWorkerLimaAuthorityErrorKind::RecoveryRequired
    );
    assert!(
        stage.exists(),
        "refusal must preserve catalog recovery evidence"
    );
}

#[test]
fn fixture_uses_only_reviewed_observation_commands() {
    assert!(memory_pressure_command().environment.is_empty());
    assert!(swap_command().environment.is_empty());
    assert!(power_command().environment.is_empty());
    assert!(lima_process_command().environment.is_empty());
    assert!(vm_stat_command().environment.is_empty());
    assert!(logical_cpu_command().environment.is_empty());
}
