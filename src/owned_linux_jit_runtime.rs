//! Native-Linux physical backend for one trusted GitHub Actions JIT task.
//!
//! The GitHub delivery/catalog remains the canonical scheduler ledger. This module owns only the
//! exact Big Red task identity and the fixed helper boundary proven by the owned-Linux primitive.

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::path::{Component, Path};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::artifact::Sha256Digest;
use crate::disposable_attempt_catalog::DisposableAttemptReservation;
use crate::disposable_clone_runtime::{CloneRuntimeClock, DisposableCloneRuntimeError};
use crate::disposable_prepared_template::DisposablePreparedTemplateIdentity;
use crate::disposable_worker_reconciler::DisposableVmIdentity;
#[cfg(target_os = "linux")]
use crate::disposable_worker_reconciler::DisposableWorkerResources;
use crate::execution_admission::EpochMillis;
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};

const PYTHON: &str = "/usr/bin/python3";
const PROFILE: &str = "github-actions-trusted-v1";
const NETWORK: &str = "github_actions_trusted_egress";
const RUNNER_VERSION: &str = "2.336.0";
const RUNNER_ARCHITECTURE: &str = "x64";
const RUNNER_ARCHIVE_SHA256: &str =
    "sha256:04cf0be1aff4c3ec3554466c39124ca250e3effd8873bb7e8d68535aa9505d5d";
const MAX_HELPER_BYTES: u64 = 512 * 1024;
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(30);
const MUTATION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RUNNER_DEADLINE_SECONDS: u64 = 6 * 60 * 60;
const GENERATION_DOMAIN: &[u8] = b"glaeda.owned-linux-jit-generation.v1\0";
const TASK_DOMAIN: &[u8] = b"glaeda.owned-linux-jit-task.v1\0";
const COMMAND_DOMAIN: &[u8] = b"glaeda.owned-linux-jit-command.v1\0";
const BINDING_DOMAIN: &[u8] = b"glaeda.owned-linux-jit-binding.v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnedLinuxTaskState {
    Absent,
    Preparing,
    Launching,
}

#[derive(Clone)]
pub(crate) struct OwnedLinuxJitRuntime {
    helper: PathBuf,
    helper_digest: Sha256Digest,
    admission_root: PathBuf,
    task_root: PathBuf,
    payload_root: PathBuf,
    payload_tree_digest: Sha256Digest,
    launcher: PathBuf,
    egress_guard: PathBuf,
    egress_guard_digest: Sha256Digest,
}

impl std::fmt::Debug for OwnedLinuxJitRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedLinuxJitRuntime")
            .field("profile", &PROFILE)
            .field("network", &NETWORK)
            .field("paths", &"<private>")
            .finish()
    }
}

impl OwnedLinuxJitRuntime {
    // Constructed only by `owned_linux_jit_enrollment`, which is `target_os = "linux"`. The type
    // itself stays `cfg(unix)` because the Lima-capable unix backends name it in their signatures
    // and enum arms, but nothing on a non-Linux host can build one. Gating the construction path to
    // its consumer keeps `dead_code` honest on both platforms instead of silencing it.
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        helper: impl Into<PathBuf>,
        helper_digest: Sha256Digest,
        admission_root: impl Into<PathBuf>,
        task_root: impl Into<PathBuf>,
        payload_root: impl Into<PathBuf>,
        payload_tree_digest: Sha256Digest,
        launcher: impl Into<PathBuf>,
        egress_guard: impl Into<PathBuf>,
        egress_guard_digest: Sha256Digest,
    ) -> Result<Self, DisposableCloneRuntimeError> {
        let runtime = Self {
            helper: validate_path(helper.into())?,
            helper_digest,
            admission_root: validate_path(admission_root.into())?,
            task_root: validate_path(task_root.into())?,
            payload_root: validate_path(payload_root.into())?,
            payload_tree_digest,
            launcher: validate_path(launcher.into())?,
            egress_guard: validate_path(egress_guard.into())?,
            egress_guard_digest,
        };
        runtime.verify_helper()?;
        Ok(runtime)
    }

    pub(crate) fn generation_identity(
        &self,
    ) -> Result<DisposablePreparedTemplateIdentity, DisposableCloneRuntimeError> {
        let digest = digest_parts(
            GENERATION_DOMAIN,
            &[
                PROFILE,
                NETWORK,
                RUNNER_VERSION,
                RUNNER_ARCHITECTURE,
                RUNNER_ARCHIVE_SHA256,
                self.helper_digest.as_str(),
                self.payload_tree_digest.as_str(),
                self.egress_guard_digest.as_str(),
                self.egress_guard
                    .to_str()
                    .ok_or_else(|| config("owned_linux_egress_guard_path_invalid"))?,
                self.launcher
                    .to_str()
                    .ok_or_else(|| config("owned_linux_launcher_path_invalid"))?,
            ],
        );
        DisposablePreparedTemplateIdentity::parse(&digest)
            .map_err(|_| config("owned_linux_generation_identity_invalid"))
    }

    pub(crate) fn validate_reservation_generation(
        &self,
        reservation: &DisposableAttemptReservation,
    ) -> Result<(), DisposableCloneRuntimeError> {
        if reservation.prepared_template_identity() != &self.generation_identity()? {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_generation_drift",
            ));
        }
        Ok(())
    }

    pub(crate) fn probe(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
    ) -> Result<OwnedLinuxTaskState, DisposableCloneRuntimeError> {
        self.validate_reservation_generation(reservation)?;
        let record = self.execute("probe", reservation, None, executor, OBSERVE_TIMEOUT)?;
        let receipt: ProbeReceipt = parse_receipt(&record, "glaeda-owned-linux-jit-probe")?;
        self.validate_receipt_identity(reservation, &receipt.task_identity_sha256)?;
        match (receipt.state.as_str(), receipt.reservation_phase.as_deref()) {
            ("absent", None) => Ok(OwnedLinuxTaskState::Absent),
            ("present", Some("preparing")) => Ok(OwnedLinuxTaskState::Preparing),
            ("present", Some("launching")) => Ok(OwnedLinuxTaskState::Launching),
            _ => Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_probe_state_invalid",
            )),
        }
    }

    pub(crate) fn prepare(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
    ) -> Result<DisposableVmIdentity, DisposableCloneRuntimeError> {
        self.validate_reservation_generation(reservation)?;
        let record = self.execute("prepare", reservation, None, executor, MUTATION_TIMEOUT)?;
        let receipt: PrepareReceipt =
            parse_receipt(&record, "glaeda-owned-linux-jit-prepare-receipt")?;
        if receipt.reservation_phase != "preparing"
            || receipt.profile != PROFILE
            || receipt.network != NETWORK
            || receipt.payload_tree_sha256 != self.payload_tree_digest.as_str()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_prepare_receipt_invalid",
            ));
        }
        self.validate_receipt_identity(reservation, &receipt.task_identity_sha256)?;
        DisposableVmIdentity::parse(&receipt.task_identity_sha256)
            .map_err(|_| DisposableCloneRuntimeError::recovery("owned_linux_task_identity_invalid"))
    }

    pub(crate) fn confirm(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
    ) -> Result<String, DisposableCloneRuntimeError> {
        self.validate_reservation_generation(reservation)?;
        let record = self.execute("observe", reservation, None, executor, OBSERVE_TIMEOUT)?;
        let receipt: ObservationReceipt =
            parse_receipt(&record, "glaeda-owned-linux-jit-observation")?;
        if !receipt.settled
            || !matches!(
                receipt.reservation_phase.as_str(),
                "preparing" | "launching"
            )
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_observation_invalid",
            ));
        }
        self.validate_receipt_identity(reservation, &receipt.task_identity_sha256)?;
        Ok(receipt.task_identity_sha256)
    }

    pub(crate) fn cleanup(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
    ) -> Result<(), DisposableCloneRuntimeError> {
        self.validate_reservation_generation(reservation)?;
        let record = self.execute("cleanup", reservation, None, executor, MUTATION_TIMEOUT)?;
        let receipt: CleanupReceipt =
            parse_receipt(&record, "glaeda-owned-linux-jit-cleanup-receipt")?;
        if !receipt.settled || !receipt.task_state_absent || !receipt.capacity_released {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_cleanup_receipt_invalid",
            ));
        }
        self.validate_receipt_identity(reservation, &receipt.task_identity_sha256)?;
        match self.probe(reservation, executor)? {
            OwnedLinuxTaskState::Absent => Ok(()),
            _ => Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_cleanup_not_observed",
            )),
        }
    }

    pub(crate) fn runner_command(
        &self,
        reservation: &DisposableAttemptReservation,
        now: EpochMillis,
        jit: Zeroizing<String>,
    ) -> Result<CommandSpec, crate::disposable_runner_runtime::DisposableRunnerRuntimeError> {
        self.verify_helper().map_err(|_| {
            crate::disposable_runner_runtime::DisposableRunnerRuntimeError::observation(
                "owned_linux_runner_helper_drift",
            )
        })?;
        self.validate_reservation_generation(reservation)
            .map_err(|_| {
                crate::disposable_runner_runtime::DisposableRunnerRuntimeError::recovery(
                    "owned_linux_runner_generation_drift",
                )
            })?;
        let remaining_millis = reservation
            .attempt()
            .not_after()
            .get()
            .checked_sub(now.get())
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                crate::disposable_runner_runtime::DisposableRunnerRuntimeError::recovery(
                    "runner_attempt_expired",
                )
            })?;
        let seconds = remaining_millis
            .saturating_add(999)
            .div_ceil(1_000)
            .min(MAX_RUNNER_DEADLINE_SECONDS);
        let mut command = self.base_command("launch", reservation).map_err(|_| {
            crate::disposable_runner_runtime::DisposableRunnerRuntimeError::recovery(
                "owned_linux_runner_command_invalid",
            )
        })?;
        command = command
            .argument("--deadline-seconds")
            .argument(seconds.to_string())
            .zeroizing_secret_stdin_line(jit);
        Ok(command)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn fixed_resources() -> Result<DisposableWorkerResources, DisposableCloneRuntimeError>
    {
        DisposableWorkerResources::new(4_000, 8 * 1024 * 1024 * 1024, 20 * 1024 * 1024 * 1024)
            .map_err(|_| config("owned_linux_resource_profile_invalid"))
    }

    fn execute(
        &self,
        operation: &str,
        reservation: &DisposableAttemptReservation,
        deadline_seconds: Option<u64>,
        executor: &impl TimedCommandExecutor,
        timeout: Duration,
    ) -> Result<ExecutionRecord, DisposableCloneRuntimeError> {
        self.verify_helper()?;
        let mut command = self.base_command(operation, reservation)?;
        if let Some(deadline_seconds) = deadline_seconds {
            command = command
                .argument("--deadline-seconds")
                .argument(deadline_seconds.to_string());
        }
        let record = executor
            .execute_with_timeout(&command, timeout)
            .map_err(|_| {
                DisposableCloneRuntimeError::command("owned_linux_helper_command_failed")
            })?;
        validate_record(&command, &record)?;
        Ok(record)
    }

    fn base_command(
        &self,
        operation: &str,
        reservation: &DisposableAttemptReservation,
    ) -> Result<CommandSpec, DisposableCloneRuntimeError> {
        let identity = self.identity(reservation)?;
        Ok(CommandSpec::new(PYTHON)
            .argument(self.helper.to_string_lossy())
            .argument(operation)
            .argument("--admission-root")
            .argument(self.admission_root.to_string_lossy())
            .argument("--task-root")
            .argument(identity.task_root.to_string_lossy())
            .argument("--payload-root")
            .argument(self.payload_root.to_string_lossy())
            .argument("--payload-tree-sha256")
            .argument(self.payload_tree_digest.as_str())
            .argument("--launcher")
            .argument(self.launcher.to_string_lossy())
            .argument("--egress-guard")
            .argument(self.egress_guard.to_string_lossy())
            .argument("--egress-guard-sha256")
            .argument(self.egress_guard_digest.as_str())
            .argument("--command-fingerprint")
            .argument(identity.command_fingerprint)
            .argument("--unit")
            .argument(identity.unit)
            .argument("--binding-sha256")
            .argument(identity.binding_sha256))
    }

    fn identity(
        &self,
        reservation: &DisposableAttemptReservation,
    ) -> Result<TaskIdentity, DisposableCloneRuntimeError> {
        self.validate_reservation_generation(reservation)?;
        let attempt = reservation.attempt();
        let task_digest = digest_parts(
            TASK_DOMAIN,
            &[
                attempt.attempt_id().as_str(),
                attempt.capacity_claim_id().as_str(),
                attempt.vm_id().as_str(),
                attempt.runner_name().as_str(),
                &attempt.runner_request_id().get().to_string(),
                reservation.prepared_template_identity().as_str(),
            ],
        );
        let token = task_digest
            .strip_prefix("sha256:")
            .ok_or_else(|| config("owned_linux_task_token_invalid"))?
            .chars()
            .take(32)
            .collect::<String>();
        let unit = format!("glaeda-gha-{token}.service");
        let task_root = self.task_root.join(&token);
        let command_fingerprint = digest_parts(
            COMMAND_DOMAIN,
            &[
                PROFILE,
                NETWORK,
                attempt.attempt_id().as_str(),
                attempt.runner_name().as_str(),
                self.payload_tree_digest.as_str(),
                self.egress_guard_digest.as_str(),
            ],
        );
        let binding_sha256 = digest_parts(
            BINDING_DOMAIN,
            &[
                attempt.capacity_claim_id().as_str(),
                reservation.prepared_template_identity().as_str(),
                &reservation.resources().cpu_millis().to_string(),
                &reservation.resources().memory_bytes().to_string(),
                &reservation.resources().disk_bytes().to_string(),
            ],
        );
        Ok(TaskIdentity {
            task_root,
            unit,
            command_fingerprint,
            binding_sha256,
        })
    }

    fn validate_receipt_identity(
        &self,
        reservation: &DisposableAttemptReservation,
        observed: &str,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let expected = reservation
            .attempt()
            .vm_identity()
            .map(DisposableVmIdentity::as_str);
        if let Some(expected) = expected
            && observed != expected
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_task_identity_drift",
            ));
        }
        if Sha256Digest::parse(observed).is_err() {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_task_identity_invalid",
            ));
        }
        Ok(())
    }

    fn verify_helper(&self) -> Result<(), DisposableCloneRuntimeError> {
        let metadata = fs::symlink_metadata(&self.helper)
            .map_err(|_| config("owned_linux_helper_unavailable"))?;
        let mode = metadata.permissions().mode();
        let effective = rustix::process::geteuid().as_raw();
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_HELPER_BYTES
            || metadata.nlink() != 1
            || mode & 0o022 != 0
            || !matches!(metadata.uid(), 0) && metadata.uid() != effective
        {
            return Err(config("owned_linux_helper_unsafe"));
        }
        let bytes = fs::read(&self.helper).map_err(|_| config("owned_linux_helper_unavailable"))?;
        let observed = format!("sha256:{:x}", Sha256::digest(&bytes));
        if observed != self.helper_digest.as_str() {
            return Err(config("owned_linux_helper_digest_mismatch"));
        }
        Ok(())
    }
}

impl crate::disposable_runner_runtime::DisposableRunnerTargetRuntime for OwnedLinuxJitRuntime {
    type Confirmation = String;

    fn confirm_runner_target(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
        _clock: &impl CloneRuntimeClock,
    ) -> Result<Self::Confirmation, crate::disposable_runner_runtime::DisposableRunnerRuntimeError>
    {
        self.confirm(reservation, executor).map_err(|_| {
            crate::disposable_runner_runtime::DisposableRunnerRuntimeError::observation(
                "runner_target_not_ready",
            )
        })
    }

    fn reconfirm_runner_target(
        &self,
        confirmation: &Self::Confirmation,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
        _clock: &impl CloneRuntimeClock,
    ) -> Result<(), crate::disposable_runner_runtime::DisposableRunnerRuntimeError> {
        let observed = self.confirm(reservation, executor).map_err(|_| {
            crate::disposable_runner_runtime::DisposableRunnerRuntimeError::observation(
                "runner_target_not_ready",
            )
        })?;
        if &observed != confirmation {
            return Err(
                crate::disposable_runner_runtime::DisposableRunnerRuntimeError::recovery(
                    "runner_target_identity_drift",
                ),
            );
        }
        Ok(())
    }
}

#[derive(Debug)]
struct TaskIdentity {
    task_root: PathBuf,
    unit: String,
    command_fingerprint: String,
    binding_sha256: String,
}

#[derive(Deserialize)]
struct ProbeReceipt {
    document_type: String,
    schema_version: u8,
    task_identity_sha256: String,
    state: String,
    reservation_phase: Option<String>,
}

#[derive(Deserialize)]
struct PrepareReceipt {
    document_type: String,
    schema_version: u8,
    task_identity_sha256: String,
    profile: String,
    network: String,
    payload_tree_sha256: String,
    reservation_phase: String,
}

#[derive(Deserialize)]
struct ObservationReceipt {
    document_type: String,
    schema_version: u8,
    task_identity_sha256: String,
    reservation_phase: String,
    settled: bool,
}

#[derive(Deserialize)]
struct CleanupReceipt {
    document_type: String,
    schema_version: u8,
    task_identity_sha256: String,
    settled: bool,
    task_state_absent: bool,
    capacity_released: bool,
}

trait Receipt {
    fn document_type(&self) -> &str;
    fn schema_version(&self) -> u8;
}

impl Receipt for ProbeReceipt {
    fn document_type(&self) -> &str {
        &self.document_type
    }
    fn schema_version(&self) -> u8 {
        self.schema_version
    }
}
impl Receipt for PrepareReceipt {
    fn document_type(&self) -> &str {
        &self.document_type
    }
    fn schema_version(&self) -> u8 {
        self.schema_version
    }
}
impl Receipt for ObservationReceipt {
    fn document_type(&self) -> &str {
        &self.document_type
    }
    fn schema_version(&self) -> u8 {
        self.schema_version
    }
}
impl Receipt for CleanupReceipt {
    fn document_type(&self) -> &str {
        &self.document_type
    }
    fn schema_version(&self) -> u8 {
        self.schema_version
    }
}

fn parse_receipt<T: for<'de> Deserialize<'de> + Receipt>(
    record: &ExecutionRecord,
    document_type: &str,
) -> Result<T, DisposableCloneRuntimeError> {
    let receipt: T = serde_json::from_str(&record.stdout)
        .map_err(|_| DisposableCloneRuntimeError::recovery("owned_linux_receipt_invalid"))?;
    if receipt.document_type() != document_type || receipt.schema_version() != 1 {
        return Err(DisposableCloneRuntimeError::recovery(
            "owned_linux_receipt_invalid",
        ));
    }
    Ok(receipt)
}

fn validate_record(
    command: &CommandSpec,
    record: &ExecutionRecord,
) -> Result<(), DisposableCloneRuntimeError> {
    if record.argv != command.displayed_argv()
        || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        || record.status != Some(0)
        || !record.success
        || !record.stderr.is_empty()
    {
        return Err(DisposableCloneRuntimeError::command(
            "owned_linux_helper_record_mismatch",
        ));
    }
    Ok(())
}

fn digest_parts(domain: &[u8], values: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for value in values {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(target_os = "linux")]
fn validate_path(path: PathBuf) -> Result<PathBuf, DisposableCloneRuntimeError> {
    let raw = path
        .to_str()
        .ok_or_else(|| config("owned_linux_path_invalid"))?;
    if !path.is_absolute()
        || path == Path::new("/")
        || raw.len() > 2_048
        || raw.bytes().any(|byte| byte.is_ascii_control())
        || raw.contains("//")
        || raw.ends_with('/')
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(config("owned_linux_path_invalid"));
    }
    Ok(path)
}

const fn config(code: &'static str) -> DisposableCloneRuntimeError {
    DisposableCloneRuntimeError::configuration(code)
}
