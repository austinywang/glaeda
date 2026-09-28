use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_attempt_catalog::DisposableAttemptReservation;
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, DisposablePreparedTemplateManifest,
};
use crate::disposable_worker_reconciler::{
    DisposableAttemptPhase, DisposableWorkerAction, DisposableWorkerResources,
};
use crate::execution_admission::EpochMillis;
use crate::lima_observation::{
    LIMACTL_SAFE_HOME, LIMACTL_SAFE_PATH, LimaArchitecture, LimaInstanceName,
    LimaObservationRequest, LimaVmType,
};
use crate::process::CommandSpec;

pub const DISPOSABLE_LIMA_WORKER_SCHEMA_VERSION: u8 = 4;
/// Hex characters retained in a generated disposable Lima VM name (96 bits).
pub(crate) const DISPOSABLE_LIMA_VM_ID_HEX_CHARS: usize = 24;

const GIB: u64 = 1 << 30;
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
// Lima 2.2.0 validates this longest candidate SSH socket path before cloning on macOS. The pinned
// Lima contract requires the terminating NUL to fit in sockaddr_un.sun_path[104].
const LIMA_UNIX_PATH_MAX: usize = 104;
const LIMA_SSH_SOCKET_SUFFIX: &str = "/ssh.sock.1234567890123456";
const CLONE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DESTROY_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const OBSERVATION_MAX_AGE_SECONDS: u64 = 30;
const GUEST_CACHE_PATH: &str = "/opt/smolrunner/actions-runner/_work";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLimaWorkerCommandKind {
    Clone,
    DiscardIncomplete,
    Destroy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLimaWorkerErrorKind {
    InvalidConfiguration,
    InvalidAction,
    UnsupportedResources,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableLimaWorkerError {
    kind: DisposableLimaWorkerErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableLimaWorkerError {
    #[must_use]
    pub const fn kind(&self) -> DisposableLimaWorkerErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for DisposableLimaWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableLimaWorkerError {}

/// Fixed command adapter for disposable Lima/VZ workers.
///
/// Lima owns VM creation, startup, process supervision, and deletion. This adapter contributes no
/// shell, arbitrary guest command, network policy, runner credential, caller-selected environment,
/// or caller-selected deadline. The source template must be prepared and validated by the trusted
/// controller before this boundary; the adapter binds its exact prepared-template digest to the
/// durable reservation because a Lima instance name alone is not ownership or isolation evidence.
/// The clone plan removes inherited mounts and requests startup only as the final clone operation;
/// this adapter never emits Lima's create-or-start `start NAME` command. No plan is executable until
/// a separate boundary supplies sealed current evidence and durable authority.
#[derive(Clone, PartialEq, Eq)]
pub struct DisposableLimaWorkerAdapter {
    limactl_program: PathBuf,
    lima_home: PathBuf,
    source_template: LimaInstanceName,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
    source_cpu_count: u32,
    source_memory_bytes: u64,
    source_disk_bytes: u64,
    lima_version: String,
}

impl fmt::Debug for DisposableLimaWorkerAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLimaWorkerAdapter")
            .field("limactl_program", &"<private-program-path>")
            .field("lima_home", &"<private-lima-home>")
            .field("source_template", &self.source_template)
            .field(
                "prepared_template_identity",
                &self.prepared_template_identity,
            )
            .finish()
    }
}

impl DisposableLimaWorkerAdapter {
    /// Construct the fixed adapter for one configured prepared source template.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless both private paths are canonical absolute UTF-8 paths.
    pub fn new(
        limactl_program: impl Into<PathBuf>,
        lima_home: impl Into<PathBuf>,
        source_template: LimaInstanceName,
        prepared_template: &DisposablePreparedTemplateManifest,
    ) -> Result<Self, DisposableLimaWorkerError> {
        let limactl_program = validate_private_path(limactl_program.into())?;
        let lima_home = validate_private_path(lima_home.into())?;
        let prepared_template_identity = prepared_template
            .identity()
            .map_err(|_| invalid_configuration())?;
        Ok(Self {
            limactl_program,
            lima_home,
            source_template,
            prepared_template_identity,
            source_cpu_count: prepared_template.source_cpu_count(),
            source_memory_bytes: prepared_template.source_memory_bytes(),
            source_disk_bytes: prepared_template.source_disk_bytes(),
            lima_version: prepared_template.lima_version().to_owned(),
        })
    }

    /// Construct one closed command plan for an action selected by the durable reconciler.
    ///
    /// This is deliberately non-executing. The returned plan exposes no `CommandSpec`, private
    /// path, or execution method. A later same-lock durable service must reopen current catalog
    /// state, obtain its own time, bind a sealed exact VM observation, and then consume the private
    /// command. A command completion will still require fresh observation before phase advance.
    ///
    /// # Errors
    ///
    /// Returns when the action, durable phase, VM name, or resource granularity is outside the
    /// closed adapter policy.
    pub fn plan(
        &self,
        now: EpochMillis,
        reservation: &DisposableAttemptReservation,
        action: &DisposableWorkerAction,
    ) -> Result<DisposableLimaWorkerCommandPlan, DisposableLimaWorkerError> {
        let attempt = reservation.attempt();
        // Creation consumes the currently configured source template. Destruction does not: an
        // already-owned worker must remain removable after a deliberate template upgrade. The
        // delete plan binds the reservation's original template identity below instead.
        if matches!(action, DisposableWorkerAction::CheckpointAndCloneVm)
            && reservation.prepared_template_identity() != &self.prepared_template_identity
        {
            return Err(error(
                DisposableLimaWorkerErrorKind::InvalidAction,
                "prepared_template_identity_drift",
                "the durable prepared-template identity differs from the configured Lima source",
            ));
        }
        let instance = LimaInstanceName::parse(attempt.vm_id().as_str()).map_err(|_| {
            error(
                DisposableLimaWorkerErrorKind::InvalidAction,
                "invalid_vm_identity",
                "the durable VM identity is not a supported Lima instance name",
            )
        })?;
        if instance == self.source_template {
            return Err(error(
                DisposableLimaWorkerErrorKind::InvalidAction,
                "source_template_conflict",
                "the disposable VM identity conflicts with the prepared source template",
            ));
        }
        if matches!(action, DisposableWorkerAction::CheckpointAndCloneVm)
            && now > attempt.not_after()
        {
            return Err(error(
                DisposableLimaWorkerErrorKind::InvalidAction,
                "attempt_expired",
                "the disposable attempt expired before the Lima provisioning command",
            ));
        }

        let (kind, command, timeout) = match action {
            DisposableWorkerAction::CheckpointAndCloneVm
                if attempt.phase() == DisposableAttemptPhase::CloneAuthorized =>
            {
                let resources = lima_resources(
                    reservation.resources(),
                    self.source_cpu_count,
                    self.source_memory_bytes,
                    self.source_disk_bytes,
                )?;
                (
                    DisposableLimaWorkerCommandKind::Clone,
                    self.base_command()
                        .argument("clone")
                        .argument(self.source_template.as_str())
                        .argument(instance.as_str())
                        .argument("--cpus")
                        .argument(resources.cpus)
                        .argument("--memory")
                        .argument(resources.memory_gib)
                        .argument("--disk")
                        .argument(resources.disk_gib)
                        .argument("--mount-none")
                        .argument("--start"),
                    CLONE_TIMEOUT,
                )
            }
            DisposableWorkerAction::DiscardIncompleteVm
                if attempt.phase() == DisposableAttemptPhase::CloneStarted =>
            {
                (
                    DisposableLimaWorkerCommandKind::DiscardIncomplete,
                    self.base_command()
                        .argument("delete")
                        .argument("--force")
                        .argument(instance.as_str()),
                    DESTROY_TIMEOUT,
                )
            }
            DisposableWorkerAction::DestroyVm
                if matches!(
                    attempt.phase(),
                    DisposableAttemptPhase::Destroying
                        | DisposableAttemptPhase::Deregistering
                        | DisposableAttemptPhase::Releasing
                ) =>
            {
                (
                    DisposableLimaWorkerCommandKind::Destroy,
                    self.base_command()
                        .argument("delete")
                        .argument("--force")
                        .argument(instance.as_str()),
                    DESTROY_TIMEOUT,
                )
            }
            DisposableWorkerAction::CheckpointAndCloneVm
            | DisposableWorkerAction::DiscardIncompleteVm
            | DisposableWorkerAction::DestroyVm => {
                return Err(error(
                    DisposableLimaWorkerErrorKind::InvalidAction,
                    "invalid_durable_phase",
                    "the durable attempt phase does not authorize the requested Lima command",
                ));
            }
            _ => {
                return Err(error(
                    DisposableLimaWorkerErrorKind::InvalidAction,
                    "unsupported_action",
                    "the reconciler action is not a disposable Lima lifecycle command",
                ));
            }
        };
        let command_identity = command_identity(
            &command,
            timeout,
            &self.lima_home,
            reservation.prepared_template_identity(),
        )?;
        Ok(DisposableLimaWorkerCommandPlan {
            schema_version: DISPOSABLE_LIMA_WORKER_SCHEMA_VERSION,
            kind,
            attempt_id: attempt.attempt_id().as_str().to_owned(),
            attempt_revision: attempt.revision().get(),
            vm_id: attempt.vm_id().as_str().to_owned(),
            prepared_template_identity: reservation.prepared_template_identity().clone(),
            command_identity,
            timeout_seconds: timeout.as_secs(),
            observation_required: true,
            command,
            timeout,
        })
    }

    pub(crate) fn target_observation_request(
        &self,
        reservation: &DisposableAttemptReservation,
    ) -> Result<LimaObservationRequest, DisposableLimaWorkerError> {
        let instance =
            LimaInstanceName::parse(reservation.attempt().vm_id().as_str()).map_err(|_| {
                error(
                    DisposableLimaWorkerErrorKind::InvalidAction,
                    "invalid_vm_identity",
                    "the durable VM identity is not a supported Lima instance name",
                )
            })?;
        LimaObservationRequest::new(
            instance,
            self.lima_home.clone(),
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            GUEST_CACHE_PATH,
            OBSERVATION_MAX_AGE_SECONDS,
        )
        .map_err(|_| invalid_configuration())
    }

    pub(crate) fn version_command(&self) -> (CommandSpec, String) {
        (
            self.base_command().argument("--version"),
            format!("limactl version {}\n", self.lima_version),
        )
    }

    fn base_command(&self) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .argument("--tty=false")
            .environment("HOME", LIMACTL_SAFE_HOME)
            .secret_environment("LIMA_HOME", exact_path(&self.lima_home))
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
            .environment("PATH", LIMACTL_SAFE_PATH)
    }
}

pub struct DisposableLimaWorkerCommandPlan {
    schema_version: u8,
    kind: DisposableLimaWorkerCommandKind,
    attempt_id: String,
    attempt_revision: u64,
    vm_id: String,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
    command_identity: Sha256Digest,
    timeout_seconds: u64,
    observation_required: bool,
    command: CommandSpec,
    timeout: Duration,
}

impl DisposableLimaWorkerCommandPlan {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn kind(&self) -> DisposableLimaWorkerCommandKind {
        self.kind
    }

    #[must_use]
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    #[must_use]
    pub const fn attempt_revision(&self) -> u64 {
        self.attempt_revision
    }

    #[must_use]
    pub fn vm_id(&self) -> &str {
        &self.vm_id
    }

    #[must_use]
    pub const fn prepared_template_identity(&self) -> &DisposablePreparedTemplateIdentity {
        &self.prepared_template_identity
    }

    #[must_use]
    pub const fn command_identity(&self) -> &Sha256Digest {
        &self.command_identity
    }

    #[must_use]
    pub const fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
    }

    #[must_use]
    pub const fn observation_required(&self) -> bool {
        self.observation_required
    }

    pub(crate) const fn command(&self) -> &CommandSpec {
        &self.command
    }

    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl fmt::Debug for DisposableLimaWorkerCommandPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLimaWorkerCommandPlan")
            .field("schema_version", &self.schema_version)
            .field("kind", &self.kind)
            .field("attempt_id", &self.attempt_id)
            .field("attempt_revision", &self.attempt_revision)
            .field("vm_id", &self.vm_id)
            .field(
                "prepared_template_identity",
                &self.prepared_template_identity,
            )
            .field("command_identity", &self.command_identity)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("observation_required", &self.observation_required)
            .finish()
    }
}

#[derive(Serialize)]
struct CommandIdentityDocument<'a> {
    schema_version: u8,
    argv: Vec<String>,
    home: &'static str,
    lima_home: &'a str,
    lang: &'static str,
    lc_all: &'static str,
    path: &'static str,
    timeout_seconds: u64,
    prepared_template_identity: &'a str,
}

fn command_identity(
    command: &CommandSpec,
    timeout: Duration,
    lima_home: &Path,
    prepared_template_identity: &DisposablePreparedTemplateIdentity,
) -> Result<Sha256Digest, DisposableLimaWorkerError> {
    let document = CommandIdentityDocument {
        schema_version: DISPOSABLE_LIMA_WORKER_SCHEMA_VERSION,
        argv: command.displayed_argv(),
        home: LIMACTL_SAFE_HOME,
        lima_home: lima_home
            .to_str()
            .expect("validated private Lima path remains UTF-8"),
        lang: "C",
        lc_all: "C",
        path: LIMACTL_SAFE_PATH,
        timeout_seconds: timeout.as_secs(),
        prepared_template_identity: prepared_template_identity.as_str(),
    };
    let bytes = serde_json::to_vec(&document).map_err(|_| invalid_configuration())?;
    let digest = Sha256::digest(bytes);
    Sha256Digest::parse(&format!("sha256:{digest:x}")).map_err(|_| invalid_configuration())
}

struct LimaResources {
    cpus: String,
    memory_gib: String,
    disk_gib: String,
}

/// Validate that one resource reservation is exactly representable by the fixed Lima backend.
///
/// The enrollment boundary uses this before GitHub demand can persist the reservation. Keeping
/// the check here prevents configuration admission and command planning from drifting apart.
pub(crate) fn validate_disposable_lima_resources(
    resources: DisposableWorkerResources,
    prepared_template: &DisposablePreparedTemplateManifest,
) -> Result<(), DisposableLimaWorkerError> {
    lima_resources(
        resources,
        prepared_template.source_cpu_count(),
        prepared_template.source_memory_bytes(),
        prepared_template.source_disk_bytes(),
    )
    .map(|_| ())
}

/// Validate that the enrolled Lima home can contain every newly generated disposable VM name.
///
/// This is a composition-time gate: an invalid operator enrollment is refused before GitHub demand
/// can create durable clone authority. Existing durable VM names remain decodable for recovery.
pub(crate) fn validate_disposable_lima_home_path_budget(
    lima_home: &Path,
) -> Result<(), DisposableLimaWorkerError> {
    let generated_instance_bytes = 2_usize
        .checked_add(DISPOSABLE_LIMA_VM_ID_HEX_CHARS)
        .ok_or_else(invalid_configuration)?;
    let fits = lima_home
        .to_str()
        .and_then(|home| home.len().checked_add(1))
        .and_then(|length| length.checked_add(generated_instance_bytes))
        .and_then(|length| length.checked_add(LIMA_SSH_SOCKET_SUFFIX.len()))
        .is_some_and(|length| length < LIMA_UNIX_PATH_MAX);
    if !fits {
        return Err(error(
            DisposableLimaWorkerErrorKind::InvalidConfiguration,
            "lima_home_socket_path_too_long",
            "the Lima home cannot fit a generated worker within the pinned Unix-socket budget",
        ));
    }
    Ok(())
}

fn lima_resources(
    resources: DisposableWorkerResources,
    source_cpu_count: u32,
    source_memory_bytes: u64,
    source_disk_bytes: u64,
) -> Result<LimaResources, DisposableLimaWorkerError> {
    if !resources.cpu_millis().is_multiple_of(1_000)
        || !resources.memory_bytes().is_multiple_of(GIB)
        || !resources.disk_bytes().is_multiple_of(GIB)
    {
        return Err(error(
            DisposableLimaWorkerErrorKind::UnsupportedResources,
            "unsupported_resource_granularity",
            "Lima worker resources must use whole CPUs and whole GiB values",
        ));
    }
    if resources.cpu_millis() < source_cpu_count.saturating_mul(1_000)
        || resources.memory_bytes() < source_memory_bytes
        || resources.disk_bytes() < source_disk_bytes
    {
        return Err(error(
            DisposableLimaWorkerErrorKind::UnsupportedResources,
            "resources_below_prepared_template",
            "Lima clone resources cannot be lower than the prepared source template",
        ));
    }
    Ok(LimaResources {
        cpus: (resources.cpu_millis() / 1_000).to_string(),
        memory_gib: (resources.memory_bytes() / GIB).to_string(),
        disk_gib: (resources.disk_bytes() / GIB).to_string(),
    })
}

fn validate_private_path(path: PathBuf) -> Result<PathBuf, DisposableLimaWorkerError> {
    let Some(text) = path.to_str() else {
        return Err(invalid_configuration());
    };
    if !path.is_absolute()
        || path == Path::new("/")
        || text.len() > MAX_PRIVATE_PATH_BYTES
        || text.bytes().any(|byte| byte.is_ascii_control())
        || text.contains("//")
        || text.ends_with('/')
        || text
            .split('/')
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(invalid_configuration());
    }
    Ok(path)
}

fn exact_path(path: &Path) -> String {
    path.to_str()
        .expect("validated private Lima path remains UTF-8")
        .to_owned()
}

const fn invalid_configuration() -> DisposableLimaWorkerError {
    error(
        DisposableLimaWorkerErrorKind::InvalidConfiguration,
        "invalid_private_path",
        "reviewed Lima paths must be bounded canonical absolute paths",
    )
}

const fn error(
    kind: DisposableLimaWorkerErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableLimaWorkerError {
    DisposableLimaWorkerError {
        kind,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
        MemoryDisposableAttemptCatalogStore, decode_disposable_attempt_catalog,
        encode_disposable_attempt_catalog,
    };
    use crate::disposable_attempt_state::{
        DisposableAttemptState, decode_disposable_attempt_state, encode_disposable_attempt_state,
    };
    use crate::disposable_prepared_template::{
        current_disposable_prepared_template, decode_disposable_prepared_template,
        encode_disposable_prepared_template,
    };
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableAttemptId, DisposableVmId,
    };
    use crate::execution_admission::EpochMillis;
    use crate::github_scale_set_protocol::{ScaleSetRunnerName, ScaleSetRunnerRequestId};

    fn resources() -> DisposableWorkerResources {
        DisposableWorkerResources::new(4_000, 8 * GIB, 64 * GIB).unwrap()
    }

    fn template_manifest() -> DisposablePreparedTemplateManifest {
        current_disposable_prepared_template().unwrap()
    }

    fn template_identity() -> DisposablePreparedTemplateIdentity {
        template_manifest().identity().unwrap()
    }

    fn other_template_identity() -> DisposablePreparedTemplateIdentity {
        let current = template_manifest();
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

    fn initial_reservation(resources: DisposableWorkerResources) -> DisposableAttemptReservation {
        initial_reservation_with_identity(resources, template_identity())
    }

    fn initial_reservation_with_identity(
        resources: DisposableWorkerResources,
        prepared_template_identity: DisposablePreparedTemplateIdentity,
    ) -> DisposableAttemptReservation {
        DisposableAttemptReservation::new(
            DisposableAttemptState::reserved(
                DisposableAttemptId::parse("attempt-1").unwrap(),
                CapacityClaimId::parse("claim-1").unwrap(),
                DisposableVmId::parse("smol-worker-1").unwrap(),
                ScaleSetRunnerName::parse("runner-1").unwrap(),
                ScaleSetRunnerRequestId::new(41).unwrap(),
                EpochMillis::new(10_000).unwrap(),
            ),
            resources,
            prepared_template_identity,
        )
        .unwrap()
    }

    fn reservation_in_phase(phase: DisposableAttemptPhase) -> DisposableAttemptReservation {
        reservation_in_phase_with_identity(phase, template_identity())
    }

    fn reservation_in_phase_with_identity(
        phase: DisposableAttemptPhase,
        prepared_template_identity: DisposablePreparedTemplateIdentity,
    ) -> DisposableAttemptReservation {
        let initial = initial_reservation_with_identity(resources(), prepared_template_identity);
        let attempt_id = initial.attempt().attempt_id().clone();
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        let (reserved, _) = catalog.reserve(empty.revision(), initial).unwrap();
        let reserved_attempt = reserved.find_active(&attempt_id).unwrap().attempt();
        let (provisioning, _) = catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                reserved_attempt.revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        if phase == DisposableAttemptPhase::CloneAuthorized {
            return provisioning.find_active(&attempt_id).unwrap().clone();
        }
        let provisioning_attempt = provisioning.find_active(&attempt_id).unwrap().attempt();
        let (started, _) = catalog
            .transition(
                provisioning.revision(),
                &attempt_id,
                provisioning_attempt.revision(),
                DisposableAttemptCatalogAction::RecordCloneStarted,
            )
            .unwrap();
        if phase == DisposableAttemptPhase::CloneStarted {
            return started.find_active(&attempt_id).unwrap().clone();
        }
        let started_attempt = started.find_active(&attempt_id).unwrap().attempt();
        let mut encoded =
            String::from_utf8(encode_disposable_attempt_state(started_attempt).unwrap()).unwrap();
        encoded = encoded.replacen("\"revision\":3", "\"revision\":4", 1);
        encoded = encoded.replacen(
            "\"vm_id\":\"smol-worker-1\",\"runner_name\"",
            &format!(
                "\"vm_id\":\"smol-worker-1\",\"vm_identity_digest\":\"sha256:{}\",\"runner_name\"",
                "44".repeat(32)
            ),
            1,
        );
        let bound = decode_disposable_attempt_state(encoded.as_bytes()).unwrap();
        let destroying_attempt = bound.begin_cleanup().unwrap();
        let destroying = replace_attempt_fixture(&started, started_attempt, &destroying_attempt, 2);
        if phase == DisposableAttemptPhase::Destroying {
            return destroying.find_active(&attempt_id).unwrap().clone();
        }
        let destroying_attempt = destroying.find_active(&attempt_id).unwrap().attempt();
        let deregistering_attempt = destroying_attempt
            .advance_cleanup(DisposableAttemptPhase::Deregistering)
            .unwrap();
        let deregistering =
            replace_attempt_fixture(&destroying, destroying_attempt, &deregistering_attempt, 1);
        if phase == DisposableAttemptPhase::Deregistering {
            return deregistering.find_active(&attempt_id).unwrap().clone();
        }
        assert_eq!(phase, DisposableAttemptPhase::Releasing);
        let deregistering_attempt = deregistering.find_active(&attempt_id).unwrap().attempt();
        let releasing_attempt = deregistering_attempt
            .advance_cleanup(DisposableAttemptPhase::Releasing)
            .unwrap();
        let releasing =
            replace_attempt_fixture(&deregistering, deregistering_attempt, &releasing_attempt, 1);
        releasing.find_active(&attempt_id).unwrap().clone()
    }

    fn replace_attempt_fixture(
        document: &DisposableAttemptCatalogDocument,
        current: &DisposableAttemptState,
        next: &DisposableAttemptState,
        catalog_revision_advance: u64,
    ) -> DisposableAttemptCatalogDocument {
        let current_value: serde_json::Value =
            serde_json::from_slice(&encode_disposable_attempt_state(current).unwrap()).unwrap();
        let current_json = serde_json::to_string(&current_value).unwrap();
        let next_value: serde_json::Value =
            serde_json::from_slice(&encode_disposable_attempt_state(next).unwrap()).unwrap();
        let next_json = serde_json::to_string(&next_value).unwrap();
        let mut catalog_json =
            String::from_utf8(encode_disposable_attempt_catalog(document).unwrap()).unwrap();
        catalog_json = catalog_json.replacen(
            &format!("\"revision\":{}", document.revision().get()),
            &format!(
                "\"revision\":{}",
                document.revision().get() + catalog_revision_advance
            ),
            1,
        );
        catalog_json = catalog_json.replacen(&current_json, &next_json, 1);
        decode_disposable_attempt_catalog(catalog_json.as_bytes()).unwrap()
    }

    fn clone_authorized_reservation(
        resources: DisposableWorkerResources,
    ) -> DisposableAttemptReservation {
        let initial = initial_reservation(resources);
        let attempt_id = initial.attempt().attempt_id().clone();
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        let (reserved, _) = catalog.reserve(empty.revision(), initial).unwrap();
        let attempt = reserved.find_active(&attempt_id).unwrap().attempt();
        let (authorized, _) = catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                attempt.revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        authorized.find_active(&attempt_id).unwrap().clone()
    }

    fn adapter() -> DisposableLimaWorkerAdapter {
        DisposableLimaWorkerAdapter::new(
            "/opt/homebrew/bin/limactl",
            "/Users/runner/.smolrunner/lima",
            LimaInstanceName::parse("smol-template").unwrap(),
            &template_manifest(),
        )
        .unwrap()
    }

    #[test]
    fn clone_plan_is_fixed_private_and_requires_observation() {
        let reservation = reservation_in_phase(DisposableAttemptPhase::CloneAuthorized);
        let plan = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &reservation,
                &DisposableWorkerAction::CheckpointAndCloneVm,
            )
            .unwrap();

        assert_eq!(plan.kind(), DisposableLimaWorkerCommandKind::Clone);
        assert_eq!(plan.schema_version(), 4);
        assert_eq!(plan.prepared_template_identity(), &template_identity());
        assert!(plan.observation_required());
        assert_eq!(plan.attempt_revision(), 2);
        assert_eq!(plan.timeout, CLONE_TIMEOUT);
        assert_eq!(
            plan.command.displayed_argv(),
            [
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "clone",
                "smol-template",
                "smol-worker-1",
                "--cpus",
                "4",
                "--memory",
                "8",
                "--disk",
                "64",
                "--mount-none",
                "--start",
            ]
        );
        assert_eq!(
            plan.command
                .environment
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["HOME", "LANG", "LC_ALL", "LIMA_HOME", "PATH"]
        );
        assert!(!format!("{:?}", adapter()).contains("/Users/runner"));
        assert!(!format!("{plan:?}").contains("/opt/homebrew"));
        assert!(!format!("{plan:?}").contains("/Users/runner"));
    }

    #[test]
    fn enrollment_refuses_a_lima_home_outside_the_pinned_socket_path_budget() {
        assert!(
            validate_disposable_lima_home_path_budget(Path::new(
                "/Users/leoli/.local/share/smolrunner/quarry/lima"
            ))
            .is_ok()
        );
        let error = validate_disposable_lima_home_path_budget(Path::new(
            "/Users/leoli/.local/share/smolrunner/quarry/lima-too-long",
        ))
        .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableLimaWorkerErrorKind::InvalidConfiguration
        );
        assert_eq!(error.code(), "lima_home_socket_path_too_long");
    }

    #[test]
    fn incomplete_clone_discard_and_cleanup_destroy_are_distinct_fixed_plans() {
        let provisioning = reservation_in_phase(DisposableAttemptPhase::CloneStarted);
        let discard = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &provisioning,
                &DisposableWorkerAction::DiscardIncompleteVm,
            )
            .unwrap();
        assert_eq!(
            discard.kind(),
            DisposableLimaWorkerCommandKind::DiscardIncomplete
        );
        assert_eq!(
            discard.command.displayed_argv(),
            [
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "delete",
                "--force",
                "smol-worker-1",
            ]
        );

        let destroying = reservation_in_phase(DisposableAttemptPhase::Destroying);
        let destroy = adapter()
            .plan(
                EpochMillis::new(20_000).unwrap(),
                &destroying,
                &DisposableWorkerAction::DestroyVm,
            )
            .unwrap();
        assert_eq!(
            destroy.command.displayed_argv(),
            [
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "delete",
                "--force",
                "smol-worker-1",
            ]
        );

        let old_template = other_template_identity();
        let destroying_old_template = reservation_in_phase_with_identity(
            DisposableAttemptPhase::Destroying,
            old_template.clone(),
        );
        let destroy_old_template = adapter()
            .plan(
                EpochMillis::new(20_000).unwrap(),
                &destroying_old_template,
                &DisposableWorkerAction::DestroyVm,
            )
            .unwrap();
        assert_eq!(
            destroy_old_template.prepared_template_identity(),
            &old_template
        );

        for cleanup_phase in [
            DisposableAttemptPhase::Deregistering,
            DisposableAttemptPhase::Releasing,
        ] {
            let late_worker = reservation_in_phase(cleanup_phase);
            assert_eq!(
                adapter()
                    .plan(
                        EpochMillis::new(20_000).unwrap(),
                        &late_worker,
                        &DisposableWorkerAction::DestroyVm,
                    )
                    .unwrap()
                    .kind(),
                DisposableLimaWorkerCommandKind::Destroy
            );
        }
    }

    #[test]
    fn invalid_phase_fractional_and_below_source_resources_fail_before_execution() {
        let invalid_path = DisposableLimaWorkerAdapter::new(
            "/opt/./homebrew/bin/limactl",
            "/Users/runner/.smolrunner/lima",
            LimaInstanceName::parse("smol-template").unwrap(),
            &template_manifest(),
        )
        .unwrap_err();
        assert_eq!(
            invalid_path.kind(),
            DisposableLimaWorkerErrorKind::InvalidConfiguration
        );

        let error = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &reservation_in_phase_with_identity(
                    DisposableAttemptPhase::CloneAuthorized,
                    other_template_identity(),
                ),
                &DisposableWorkerAction::CheckpointAndCloneVm,
            )
            .unwrap_err();
        assert_eq!(error.code(), "prepared_template_identity_drift");

        let reserved = initial_reservation(resources());
        let error = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &reserved,
                &DisposableWorkerAction::CheckpointAndCloneVm,
            )
            .unwrap_err();
        assert_eq!(error.kind(), DisposableLimaWorkerErrorKind::InvalidAction);

        let provisioning = reservation_in_phase(DisposableAttemptPhase::CloneAuthorized);
        let error = adapter()
            .plan(
                EpochMillis::new(10_001).unwrap(),
                &provisioning,
                &DisposableWorkerAction::CheckpointAndCloneVm,
            )
            .unwrap_err();
        assert_eq!(error.code(), "attempt_expired");

        let provisioning = clone_authorized_reservation(
            DisposableWorkerResources::new(3_500, 8 * GIB, 64 * GIB).unwrap(),
        );
        let error = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &provisioning,
                &DisposableWorkerAction::CheckpointAndCloneVm,
            )
            .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableLimaWorkerErrorKind::UnsupportedResources
        );

        for resources in [
            DisposableWorkerResources::new(1_000, 2 * GIB, 20 * GIB).unwrap(),
            DisposableWorkerResources::new(2_000, GIB, 20 * GIB).unwrap(),
            DisposableWorkerResources::new(2_000, 2 * GIB, 19 * GIB).unwrap(),
        ] {
            let error = adapter()
                .plan(
                    EpochMillis::new(1_000).unwrap(),
                    &clone_authorized_reservation(resources),
                    &DisposableWorkerAction::CheckpointAndCloneVm,
                )
                .unwrap_err();
            assert_eq!(error.code(), "resources_below_prepared_template");
        }

        let boundary = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &clone_authorized_reservation(
                    DisposableWorkerResources::new(2_000, 2 * GIB, 20 * GIB).unwrap(),
                ),
                &DisposableWorkerAction::CheckpointAndCloneVm,
            )
            .unwrap();
        assert_eq!(boundary.kind(), DisposableLimaWorkerCommandKind::Clone);
    }

    #[test]
    fn checked_in_lima_inputs_have_one_exact_arm64_image_without_a_moving_fallback() {
        let prepared_template = template_manifest();
        let location = prepared_template.guest_image_location();
        let digest = prepared_template.guest_image_digest().as_str();

        for input in [
            include_str!("../examples/lima/glaeda-work.yaml"),
            include_str!("../examples/lima/glaeda-interactive.yaml"),
            include_str!("../examples/lima/glaeda-prepared-template.yaml"),
        ] {
            let document: serde_yaml::Value = serde_yaml::from_str(input).unwrap();
            assert_eq!(
                document["minimumLimaVersion"].as_str(),
                Some(prepared_template.lima_version())
            );
            assert_eq!(
                document["vmType"].as_str(),
                Some(prepared_template.vm_type())
            );
            assert_eq!(
                document["arch"].as_str(),
                Some(prepared_template.guest_architecture())
            );
            assert!(document.get("base").is_none());
            let images = document["images"].as_sequence().unwrap();
            assert_eq!(images.len(), 1);
            assert_eq!(images[0]["location"].as_str(), Some(location));
            assert_eq!(images[0]["arch"].as_str(), Some("aarch64"));
            assert_eq!(images[0]["digest"].as_str(), Some(digest));
            assert!(!location.contains("/release/"));
            assert_eq!(document["plain"].as_bool(), Some(true));
            assert!(document["mounts"].as_sequence().unwrap().is_empty());
            assert!(document["portForwards"].as_sequence().unwrap().is_empty());
            assert_eq!(document["ssh"]["loadDotSSHPubKeys"].as_bool(), Some(false));
            assert_eq!(document["ssh"]["forwardAgent"].as_bool(), Some(false));
            assert_eq!(document["ssh"]["forwardX11"].as_bool(), Some(false));
            assert_eq!(document["ssh"]["forwardX11Trusted"].as_bool(), Some(false));
            assert_eq!(document["containerd"]["system"].as_bool(), Some(false));
            assert_eq!(document["containerd"]["user"].as_bool(), Some(false));
            assert_eq!(
                document["vmOpts"]["vz"]["rosetta"]["enabled"].as_bool(),
                Some(false)
            );
            assert_eq!(
                document["vmOpts"]["vz"]["rosetta"]["binfmt"].as_bool(),
                Some(false)
            );
            assert_eq!(document["video"]["display"].as_str(), Some("none"));
            assert_eq!(document["propagateProxyEnv"].as_bool(), Some(false));
        }

        let production: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../examples/lima/glaeda-prepared-template.yaml"
        ))
        .unwrap();
        assert_eq!(
            production["user"]["name"].as_str(),
            Some("smolrunner-admin")
        );
        assert_eq!(production["user"]["uid"].as_u64(), Some(1000));
        assert_eq!(
            production["user"]["comment"].as_str(),
            Some("SmolRunner controller")
        );
        assert_eq!(production["user"]["passwordlessSudo"].as_bool(), Some(true));
        assert_eq!(
            production["cpus"].as_u64(),
            Some(u64::from(prepared_template.source_cpu_count()))
        );
        assert_eq!(production["memory"].as_str(), Some("2GiB"));
        assert_eq!(production["disk"].as_str(), Some("20GiB"));
        assert!(production["networks"].as_sequence().unwrap().is_empty());
        assert_eq!(production["hostResolver"]["enabled"].as_bool(), Some(false));
        assert_eq!(
            production["dns"]
                .as_sequence()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>(),
            ["1.1.1.1", "1.0.0.1"]
        );
        assert_eq!(production["ssh"]["localPort"].as_u64(), Some(0));
        assert_eq!(production["ssh"]["overVsock"].as_bool(), Some(true));
        let provisions = production["provision"].as_sequence().unwrap();
        assert_eq!(provisions.len(), 1);
        assert_eq!(provisions[0]["mode"].as_str(), Some("system"));
        let script = provisions[0]["script"].as_str().unwrap();
        assert!(script.contains(prepared_template.actions_runner_location()));
        assert!(
            script.contains(
                prepared_template
                    .actions_runner_digest()
                    .as_str()
                    .strip_prefix("sha256:")
                    .unwrap()
            )
        );
        assert!(script.contains("smolrunner-runner"));
        assert!(script.contains("bin/installdependencies.sh"));
        assert!(script.contains("99-smolrunner-no-automatic-updates"));
        assert!(script.contains("systemctl mask --now"));
        let probes = production["probes"].as_sequence().unwrap();
        assert_eq!(probes.len(), 1);
        assert!(
            probes[0]["script"]
                .as_str()
                .unwrap()
                .contains(prepared_template.ready_marker_path())
        );
    }
}
