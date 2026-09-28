//! Production composition for one enrolled disposable Scale Set worker.
//!
//! Local durable recovery and the process-lifetime service lease complete before a bridge process
//! is started or a GitHub App key leaves Keychain. The service then delegates one bounded step at
//! a time to the existing coordinator and supervisor. Template readiness gates new capacity inside
//! the coordinator; it does not gate delivery recovery or cleanup of an existing attempt.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::fmt;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::disposable_attempt_catalog::DisposableAttemptCatalog;
use crate::disposable_clone_runtime::CloneRuntimeClock;
#[cfg(test)]
use crate::disposable_service_failure_receipt::{
    DisposableServiceFailureCode, DisposableServiceFailureKind, DisposableServiceFailureReceipt,
    DisposableServiceFailureReceiptError,
};
use crate::disposable_worker_coordinator::{
    DisposableWorkerCoordinator, DisposableWorkerCoordinatorDisposition,
};
use crate::disposable_worker_enrollment::{
    DisposableWorkerEnrollment, DisposableWorkerEnrollmentParts,
};
use crate::disposable_worker_supervisor::{
    DisposableWorkerSupervisorControl, DisposableWorkerSupervisorDisposition,
    DisposableWorkerSupervisorDriver, DisposableWorkerSupervisorError, supervise_disposable_worker,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{ScaleSetBridgeClient, ScaleSetBridgeConfig};
use crate::lima_observation::LimaObservationClock;
#[cfg(target_os = "linux")]
use crate::owned_linux_jit_enrollment::{OwnedLinuxJitEnrollment, OwnedLinuxJitEnrollmentParts};
use crate::personal_worker_store::PersonalWorkerStoreErrorKind;
use crate::process::ProcessExecutor;
use crate::unix_personal_worker_store::{DisposableWorkerServiceLock, UnixPersonalWorkerStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisposableWorkerServiceErrorKind {
    DurableState,
    Supervisor,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DisposableWorkerServiceError {
    kind: DisposableWorkerServiceErrorKind,
    code: &'static str,
}

impl DisposableWorkerServiceError {
    pub const fn kind(self) -> DisposableWorkerServiceErrorKind {
        self.kind
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableWorkerServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableWorkerServiceError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableWorkerServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disposable worker service could not continue")
    }
}

impl std::error::Error for DisposableWorkerServiceError {}

/// Map one already-bounded service failure into the accepted v1 diagnostic receipt.
///
/// This pure adapter grants no persistence, retry, cleanup, runtime-health, or capacity authority.
/// The caller remains responsible for supplying the exact accepted installation identities and
/// explicit timing/generation evidence that a later #515 persistence slice will own.
///
/// # Errors
///
/// Returns the receipt contract's fixed public error when a service machine code falls outside the
/// closed v1 grammar or when the supplied failure time predates process start.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn build_disposable_worker_service_failure_receipt(
    error: DisposableWorkerServiceError,
    program_digest: crate::artifact::Sha256Digest,
    enrollment_digest: crate::artifact::Sha256Digest,
    service_plan_identity: crate::artifact::Sha256Digest,
    process_started_at_epoch_ms: u64,
    failed_at_epoch_ms: u64,
    restart_generation: u64,
    durable_recovery_present: bool,
) -> Result<DisposableServiceFailureReceipt, DisposableServiceFailureReceiptError> {
    let failure_kind = match error.kind() {
        DisposableWorkerServiceErrorKind::DurableState => {
            DisposableServiceFailureKind::DurableState
        }
        DisposableWorkerServiceErrorKind::Supervisor => DisposableServiceFailureKind::Supervisor,
    };
    let failure_code = DisposableServiceFailureCode::from_static(error.code())?;
    DisposableServiceFailureReceipt::new(
        program_digest,
        enrollment_digest,
        service_plan_identity,
        failure_kind,
        failure_code,
        process_started_at_epoch_ms,
        failed_at_epoch_ms,
        restart_generation,
        durable_recovery_present,
    )
}

#[cfg(target_os = "linux")]
pub(crate) struct OwnedLinuxJitServiceDriver {
    _service_lock: DisposableWorkerServiceLock,
    bridge_config: ScaleSetBridgeConfig,
    private_key_path: std::path::PathBuf,
    private_key_digest: crate::artifact::Sha256Digest,
    coordinator: DisposableWorkerCoordinator,
    runtime: crate::owned_linux_jit_runtime::OwnedLinuxJitRuntime,
    runner_runtime: crate::disposable_runner_runtime::DisposableRunnerRuntime,
    executor: ProcessExecutor,
    clock: SystemDisposableWorkerClock,
}

pub(crate) struct DisposableWorkerServiceDriver {
    _service_lock: DisposableWorkerServiceLock,
    bridge_config: ScaleSetBridgeConfig,
    coordinator: DisposableWorkerCoordinator,
    clone_runtime: crate::disposable_clone_runtime::DisposableCloneRuntime,
    runner_runtime: crate::disposable_runner_runtime::DisposableRunnerRuntime,
    executor: ProcessExecutor,
    clock: SystemDisposableWorkerClock,
}

/// Recover local durable state and acquire exclusive process-lifetime service ownership.
///
/// This boundary performs no Keychain read and starts no bridge, Lima, or guest process.
pub(crate) fn prepare_disposable_worker_service(
    enrollment: DisposableWorkerEnrollment,
) -> Result<DisposableWorkerServiceDriver, DisposableWorkerServiceError> {
    let parts = enrollment.into_parts();
    let neutral =
        UnixPersonalWorkerStore::open_or_create_disposable_worker_service_store(&parts.state_root)
            .map_err(|_| durable_error("disposable_worker_store_unavailable"))?;
    let service_lock = neutral
        .acquire_disposable_worker_service_lock()
        .map_err(|error| {
            if error.kind() == PersonalWorkerStoreErrorKind::Busy {
                durable_error("disposable_worker_service_busy")
            } else {
                durable_error("disposable_worker_service_lock_unavailable")
            }
        })?;

    // A live delivery owns the catalog boundary until its external transaction settles. Starting
    // the bridge is what permits that recovery, so unrelated template/catalog opens must not gate
    // startup here.
    let delivery_evidence = neutral
        .has_scale_set_delivery_recovery_evidence()
        .map_err(|_| durable_error("disposable_worker_delivery_recovery_unavailable"))?;
    drop(neutral);
    if !delivery_evidence {
        recover_local_documents(&parts.state_root)?;
    }
    UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(&parts.state_root)
        .map_err(|_| durable_error("disposable_worker_delivery_recovery_unavailable"))?;

    Ok(build_driver(parts, service_lock))
}

#[cfg(target_os = "linux")]
pub(crate) fn prepare_owned_linux_jit_service(
    enrollment: OwnedLinuxJitEnrollment,
) -> Result<OwnedLinuxJitServiceDriver, DisposableWorkerServiceError> {
    let parts = enrollment.into_parts();
    let neutral =
        UnixPersonalWorkerStore::open_or_create_disposable_worker_service_store(&parts.state_root)
            .map_err(|_| durable_error("owned_linux_jit_store_unavailable"))?;
    let service_lock = neutral
        .acquire_disposable_worker_service_lock()
        .map_err(|error| {
            if error.kind() == PersonalWorkerStoreErrorKind::Busy {
                durable_error("owned_linux_jit_service_busy")
            } else {
                durable_error("owned_linux_jit_service_lock_unavailable")
            }
        })?;
    let delivery_evidence = neutral
        .has_scale_set_delivery_recovery_evidence()
        .map_err(|_| durable_error("owned_linux_jit_delivery_recovery_unavailable"))?;
    drop(neutral);
    if !delivery_evidence {
        recover_local_documents(&parts.state_root)?;
    }
    UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(&parts.state_root)
        .map_err(|_| durable_error("owned_linux_jit_delivery_recovery_unavailable"))?;
    Ok(build_owned_linux_driver(parts, service_lock))
}

fn recover_local_documents(path: &std::path::Path) -> Result<(), DisposableWorkerServiceError> {
    // One staged document can temporarily fence recovery of the other. Each successful opener
    // removes or publishes only its own classified stage, so two bounded passes are sufficient.
    for _ in 0..2 {
        let template_clean =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(path).is_ok();
        let catalog_clean = UnixPersonalWorkerStore::open_or_create_disposable_catalog(path)
            .ok()
            .and_then(|store| {
                let mut catalog = DisposableAttemptCatalog::new(store);
                catalog.initialize().ok()
            })
            .is_some();
        if template_clean && catalog_clean {
            return Ok(());
        }
    }
    Err(durable_error("disposable_worker_recovery_required"))
}

#[cfg(target_os = "linux")]
fn build_owned_linux_driver(
    parts: OwnedLinuxJitEnrollmentParts,
    service_lock: DisposableWorkerServiceLock,
) -> OwnedLinuxJitServiceDriver {
    OwnedLinuxJitServiceDriver {
        _service_lock: service_lock,
        bridge_config: parts.bridge_config,
        private_key_path: parts.private_key_path,
        private_key_digest: parts.private_key_digest,
        coordinator: DisposableWorkerCoordinator::new(
            parts.state_root,
            parts.consumer_policy,
            Box::new(parts.host_storage),
        ),
        runtime: parts.runtime,
        runner_runtime: parts.runner_runtime,
        executor: ProcessExecutor,
        clock: SystemDisposableWorkerClock,
    }
}

fn build_driver(
    parts: DisposableWorkerEnrollmentParts,
    service_lock: DisposableWorkerServiceLock,
) -> DisposableWorkerServiceDriver {
    DisposableWorkerServiceDriver {
        _service_lock: service_lock,
        bridge_config: parts.bridge_config,
        coordinator: DisposableWorkerCoordinator::new(
            parts.state_root,
            parts.consumer_policy,
            Box::new(parts.host_storage),
        ),
        clone_runtime: parts.clone_runtime,
        runner_runtime: parts.runner_runtime,
        executor: ProcessExecutor,
        clock: SystemDisposableWorkerClock,
    }
}

#[cfg(target_os = "linux")]
impl DisposableWorkerSupervisorDriver for OwnedLinuxJitServiceDriver {
    type Session = ScaleSetBridgeClient;

    fn connect(&mut self) -> Result<Self::Session, DisposableWorkerSupervisorError> {
        ScaleSetBridgeClient::connect_from_private_key_file(
            self.bridge_config.clone(),
            &self.private_key_path,
            &self.private_key_digest,
        )
        .map_err(|error| DisposableWorkerSupervisorError::new(error.code()))
    }

    fn supervise_once(
        &mut self,
        session: &mut Self::Session,
    ) -> Result<DisposableWorkerCoordinatorDisposition, DisposableWorkerSupervisorError> {
        self.coordinator
            .supervise_owned_linux_with_bridge(
                session,
                &self.runtime,
                &self.runner_runtime,
                &self.executor,
                &self.clock,
            )
            .map_err(|error| DisposableWorkerSupervisorError::new(error.code()))
    }
}

impl DisposableWorkerSupervisorDriver for DisposableWorkerServiceDriver {
    type Session = ScaleSetBridgeClient;

    fn connect(&mut self) -> Result<Self::Session, DisposableWorkerSupervisorError> {
        #[cfg(target_os = "macos")]
        {
            ScaleSetBridgeClient::connect_from_keychain(self.bridge_config.clone())
                .map_err(|error| DisposableWorkerSupervisorError::new(error.code()))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = &self.bridge_config;
            Err(DisposableWorkerSupervisorError::new(
                "disposable_worker_platform_unsupported",
            ))
        }
    }

    fn supervise_once(
        &mut self,
        session: &mut Self::Session,
    ) -> Result<DisposableWorkerCoordinatorDisposition, DisposableWorkerSupervisorError> {
        self.coordinator
            .supervise_once(
                session,
                &self.clone_runtime,
                &self.runner_runtime,
                &self.executor,
                &self.clock,
            )
            .map_err(|error| DisposableWorkerSupervisorError::new(error.code()))
    }
}

#[cfg(target_os = "linux")]
fn serve_owned_linux_jit_with_control(
    enrollment: OwnedLinuxJitEnrollment,
    control: &mut impl DisposableWorkerSupervisorControl,
) -> Result<DisposableWorkerSupervisorDisposition, DisposableWorkerServiceError> {
    let mut driver = prepare_owned_linux_jit_service(enrollment)?;
    supervise_disposable_worker(&mut driver, control)
        .map_err(|error| supervisor_error(error.code()))
}

fn serve_disposable_worker_with_control(
    enrollment: DisposableWorkerEnrollment,
    control: &mut impl DisposableWorkerSupervisorControl,
) -> Result<DisposableWorkerSupervisorDisposition, DisposableWorkerServiceError> {
    let mut driver = prepare_disposable_worker_service(enrollment)?;
    supervise_disposable_worker(&mut driver, control)
        .map_err(|error| supervisor_error(error.code()))
}

/// Run one enrolled native-Linux JIT worker until process termination is requested.
#[cfg(target_os = "linux")]
pub fn serve_owned_linux_jit_worker(
    enrollment: OwnedLinuxJitEnrollment,
) -> Result<(), DisposableWorkerServiceError> {
    let mut control = InterruptibleSupervisorControl::for_process_signals()
        .map_err(|error| supervisor_error(error.code()))?;
    serve_owned_linux_jit_with_control(enrollment, &mut control).map(|_| ())
}

/// Run one enrolled disposable-worker controller until macOS termination is requested.
///
/// SIGTERM from `launchd` and SIGINT from a foreground operator are converted into the supervisor's
/// existing bounded stop path. Local durable recovery and exclusive service ownership still
/// complete before the bridge reads the GitHub App key from Keychain. Stopping the outer loop does
/// not prove or roll back an in-flight external transaction; normal restart reconciliation owns any
/// unsettled durable or external state.
///
/// # Errors
///
/// Returns a bounded, path-free error if signal notification or local durable preparation cannot
/// begin, or if the supervisor cannot continue.
#[cfg(target_os = "macos")]
pub fn serve_disposable_worker(
    enrollment: DisposableWorkerEnrollment,
) -> Result<(), DisposableWorkerServiceError> {
    let mut control = InterruptibleSupervisorControl::for_process_signals()
        .map_err(|error| supervisor_error(error.code()))?;
    serve_disposable_worker_with_control(enrollment, &mut control).map(|_| ())
}

struct InterruptibleSupervisorControl {
    stop: Arc<AtomicBool>,
    wake_read: UnixStream,
    #[cfg(unix)]
    signal_actions: Vec<signal_hook::SigId>,
}

#[cfg(unix)]
impl InterruptibleSupervisorControl {
    fn for_process_signals() -> Result<Self, DisposableWorkerSupervisorError> {
        use signal_hook::consts::signal::{SIGINT, SIGTERM};

        let (wake_read, wake_write) = UnixStream::pair().map_err(|_| signal_control_error())?;
        let sigint_write = wake_write.try_clone().map_err(|_| signal_control_error())?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut signal_actions = Vec::with_capacity(4);

        let setup = (|| -> io::Result<()> {
            signal_actions.push(signal_hook::flag::register(SIGTERM, Arc::clone(&stop))?);
            signal_actions.push(signal_hook::low_level::pipe::register(SIGTERM, wake_write)?);
            signal_actions.push(signal_hook::flag::register(SIGINT, Arc::clone(&stop))?);
            signal_actions.push(signal_hook::low_level::pipe::register(
                SIGINT,
                sigint_write,
            )?);
            Ok(())
        })();
        if setup.is_err() {
            for action in signal_actions.drain(..) {
                let _ = signal_hook::low_level::unregister(action);
            }
            return Err(signal_control_error());
        }

        Ok(Self {
            stop,
            wake_read,
            signal_actions,
        })
    }
}

#[cfg(test)]
impl InterruptibleSupervisorControl {
    fn for_test(stop: Arc<AtomicBool>, wake_read: UnixStream) -> Self {
        Self {
            stop,
            wake_read,
            #[cfg(unix)]
            signal_actions: Vec::new(),
        }
    }
}

#[cfg(unix)]
impl Drop for InterruptibleSupervisorControl {
    fn drop(&mut self) {
        for action in self.signal_actions.drain(..) {
            let _ = signal_hook::low_level::unregister(action);
        }
    }
}

impl DisposableWorkerSupervisorControl for InterruptibleSupervisorControl {
    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn wait_or_stop(
        &mut self,
        duration: Duration,
    ) -> Result<bool, DisposableWorkerSupervisorError> {
        if self.stop_requested() {
            return Ok(true);
        }

        let started = Instant::now();
        loop {
            let remaining = duration.saturating_sub(started.elapsed());
            let timeout =
                rustix::event::Timespec::try_from(remaining).map_err(|_| signal_control_error())?;
            let mut fds = [rustix::event::PollFd::new(
                &self.wake_read,
                rustix::event::PollFlags::IN,
            )];
            match rustix::event::poll(&mut fds, Some(&timeout)) {
                Ok(0) => return Ok(self.stop_requested()),
                Ok(_) if self.stop_requested() => return Ok(true),
                Ok(_) => {
                    return Err(DisposableWorkerSupervisorError::new(
                        "disposable_worker_signal_wakeup_inconsistent",
                    ));
                }
                Err(rustix::io::Errno::INTR) if self.stop_requested() => return Ok(true),
                Err(rustix::io::Errno::INTR) => {
                    if started.elapsed() >= duration {
                        return Ok(false);
                    }
                }
                Err(_) => return Err(signal_control_error()),
            }
        }
    }
}

const fn signal_control_error() -> DisposableWorkerSupervisorError {
    DisposableWorkerSupervisorError::new("disposable_worker_signal_control_unavailable")
}

#[derive(Debug, Clone, Copy)]
struct SystemDisposableWorkerClock;

impl LimaObservationClock for SystemDisposableWorkerClock {
    fn unix_seconds(&self) -> io::Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))
    }
}

impl CloneRuntimeClock for SystemDisposableWorkerClock {
    fn epoch_millis(&self) -> io::Result<EpochMillis> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))?
            .as_millis();
        let millis = u64::try_from(millis)
            .map_err(|_| io::Error::other("system clock exceeds the supported range"))?;
        EpochMillis::new(millis)
            .map_err(|_| io::Error::other("system clock is outside the supported range"))
    }
}

const fn durable_error(code: &'static str) -> DisposableWorkerServiceError {
    DisposableWorkerServiceError {
        kind: DisposableWorkerServiceErrorKind::DurableState,
        code,
    }
}

const fn supervisor_error(code: &'static str) -> DisposableWorkerServiceError {
    DisposableWorkerServiceError {
        kind: DisposableWorkerServiceErrorKind::Supervisor,
        code,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_service_failure_receipt::DisposableServiceFailureReceiptErrorKind;
    use crate::disposable_template_generation::{
        DisposableTemplateGenerationDocument, DisposableTemplateGenerationId,
        DisposableTemplateSourceIdentity, encode_disposable_template_generation,
    };
    use crate::disposable_worker_enrollment::decode_disposable_worker_enrollment;
    use crate::lima_observation::LimaInstanceName;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "smolrunner-disposable-service-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn enrollment(root: &std::path::Path) -> DisposableWorkerEnrollment {
        let root = serde_json::to_string(root.to_str().unwrap()).unwrap();
        let document = format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 1,\n",
                "  \"state_root\": {root},\n",
                "  \"lima\": {{\n",
                "    \"program\": \"/opt/homebrew/bin/limactl\",\n",
                "    \"home\": \"/private/var/lib/smolrunner/lima\",\n",
                "    \"source_instance\": \"smolrunner-prepared-template\"\n",
                "  }},\n",
                "  \"bridge\": {{\n",
                "    \"program_digest\": \"{DIGEST}\"\n",
                "  }},\n",
                "  \"github\": {{\n",
                "    \"config_url\": \"https://github.com/acme\",\n",
                "    \"client_id\": \"Iv1.0123456789abcdef\",\n",
                "    \"installation_id\": 42,\n",
                "    \"keychain_service\": \"smolrunner.github-app\",\n",
                "    \"keychain_account\": \"acme-ci\"\n",
                "  }},\n",
                "  \"scale_set\": {{\n",
                "    \"id\": 17,\n",
                "    \"name\": \"smolrunner-disposable\",\n",
                "    \"runner_group_id\": 3,\n",
                "    \"owner\": \"acme\",\n",
                "    \"repository\": \"widgets\",\n",
                "    \"labels\": [\n",
                "      \"self-hosted\",\n",
                "      \"smolrunner\"\n",
                "    ]\n",
                "  }},\n",
                "  \"resources\": {{\n",
                "    \"cpu_millis\": 2000,\n",
                "    \"memory_bytes\": 2147483648,\n",
                "    \"disk_bytes\": 21474836480\n",
                "  }}\n",
                "}}\n"
            ),
            root = root,
            DIGEST = DIGEST,
        );
        decode_disposable_worker_enrollment(document.as_bytes()).unwrap()
    }

    fn digest() -> Sha256Digest {
        Sha256Digest::parse(DIGEST).unwrap()
    }

    fn service_failure_receipt(
        error: DisposableWorkerServiceError,
        process_started_at_epoch_ms: u64,
        failed_at_epoch_ms: u64,
    ) -> Result<DisposableServiceFailureReceipt, DisposableServiceFailureReceiptError> {
        build_disposable_worker_service_failure_receipt(
            error,
            digest(),
            digest(),
            digest(),
            process_started_at_epoch_ms,
            failed_at_epoch_ms,
            7,
            true,
        )
    }

    #[test]
    fn bounded_durable_failure_maps_into_v1_receipt() {
        let receipt = service_failure_receipt(
            durable_error("disposable_worker_recovery_required"),
            1_725_000_000_000,
            1_725_000_001_234,
        )
        .unwrap();

        assert_eq!(
            receipt.failure_kind(),
            DisposableServiceFailureKind::DurableState
        );
        assert_eq!(
            receipt.failure_code().as_str(),
            "disposable_worker_recovery_required"
        );
        assert_eq!(receipt.restart_generation(), 7);
        assert!(receipt.durable_recovery_present());

        let json = String::from_utf8(receipt.canonical_json()).unwrap();
        assert!(json.contains("\"failure_kind\":\"durable_state\""));
        assert!(json.contains("\"failure_code\":\"disposable_worker_recovery_required\""));
        for forbidden in [
            "path",
            "argv",
            "environment",
            "stdout",
            "stderr",
            "credential",
            "token",
        ] {
            assert!(
                !json.contains(forbidden),
                "forbidden receipt field fragment: {forbidden}"
            );
        }
    }

    #[test]
    fn bounded_supervisor_failure_maps_into_v1_receipt() {
        let receipt = service_failure_receipt(
            supervisor_error("disposable_worker_signal_control_unavailable"),
            10,
            11,
        )
        .unwrap();

        assert_eq!(
            receipt.failure_kind(),
            DisposableServiceFailureKind::Supervisor
        );
        assert_eq!(
            receipt.failure_code().as_str(),
            "disposable_worker_signal_control_unavailable"
        );
    }

    #[test]
    fn receipt_mapping_delegates_timeline_validation() {
        let error = service_failure_receipt(
            supervisor_error("disposable_worker_signal_control_unavailable"),
            11,
            10,
        )
        .unwrap_err();

        assert_eq!(
            error.kind(),
            DisposableServiceFailureReceiptErrorKind::InvalidTimeline
        );
    }

    #[test]
    fn interruptible_control_observes_preexisting_stop_without_waiting() {
        let (wake_read, _wake_write) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(true));
        let mut control = InterruptibleSupervisorControl::for_test(stop, wake_read);

        let started = Instant::now();
        assert!(control.wait_or_stop(Duration::from_secs(2)).unwrap());
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn interruptible_control_wakes_promptly_when_stop_is_notified() {
        let (wake_read, mut wake_write) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = InterruptibleSupervisorControl::for_test(Arc::clone(&stop), wake_read);
        let notifier = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            stop.store(true, Ordering::SeqCst);
            wake_write.write_all(&[1]).unwrap();
        });

        let started = Instant::now();
        assert!(control.wait_or_stop(Duration::from_secs(5)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(1));
        notifier.join().unwrap();
    }

    #[test]
    fn interruptible_control_refuses_wakeup_without_stop_evidence() {
        let (wake_read, mut wake_write) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = InterruptibleSupervisorControl::for_test(stop, wake_read);
        wake_write.write_all(&[1]).unwrap();

        let error = control.wait_or_stop(Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.code(), "disposable_worker_signal_wakeup_inconsistent");
    }

    #[test]
    fn repeated_stop_notifications_are_idempotent() {
        let (wake_read, mut wake_write) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(true));
        let mut control = InterruptibleSupervisorControl::for_test(stop, wake_read);
        wake_write.write_all(&[1, 1]).unwrap();

        assert!(control.stop_requested());
        assert!(control.wait_or_stop(Duration::from_secs(1)).unwrap());
        assert!(control.wait_or_stop(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn preparation_recovers_state_and_excludes_a_second_service_before_bridge_start() {
        let root = TempRoot::new();
        let first = prepare_disposable_worker_service(enrollment(&root.0)).unwrap();
        let second = match prepare_disposable_worker_service(enrollment(&root.0)) {
            Ok(_) => panic!("a second service unexpectedly acquired the lifetime lease"),
            Err(error) => error,
        };
        assert_eq!(
            second.kind(),
            DisposableWorkerServiceErrorKind::DurableState
        );
        assert_eq!(second.code(), "disposable_worker_service_busy");

        drop(first);
        prepare_disposable_worker_service(enrollment(&root.0)).unwrap();
    }

    #[test]
    fn preparation_recovers_a_staged_template_before_opening_the_delivery_controller() {
        let root = TempRoot::new();
        UnixPersonalWorkerStore::open_or_create_disposable_worker_service_store(&root.0).unwrap();
        let prepared = current_disposable_prepared_template().unwrap();
        let document = DisposableTemplateGenerationDocument::runtime_initial(
            DisposableTemplateGenerationId::parse("staged-restart").unwrap(),
            prepared.identity().unwrap(),
            DisposableTemplateSourceIdentity::from_runtime_digest(
                Sha256Digest::parse(DIGEST).unwrap(),
            ),
            LimaInstanceName::parse("smolrunner-prepared-template").unwrap(),
        );
        let bytes = encode_disposable_template_generation(&document).unwrap();
        let store_directory = root.0.join("personal-worker");
        let staged = store_directory.join(".disposable-template-generation.next.json");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        drop(file);

        prepare_disposable_worker_service(enrollment(&root.0)).unwrap();
        assert!(!staged.exists());
        assert!(
            store_directory
                .join("disposable-template-generation.json")
                .is_file()
        );
    }
}
