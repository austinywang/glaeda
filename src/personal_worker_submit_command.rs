use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, RunnerProfileId,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerCancellationState,
    PersonalWorkerJobRequest, PersonalWorkerPriority, PersonalWorkerQueueGeneration,
    PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_store::{
    PersonalWorkerStoreError, PersonalWorkerStoreErrorKind, PersonalWorkerStoreRevision,
};
use glaeda::personal_worker_store_transaction::{
    PersonalWorkerStoreMutation, PersonalWorkerStoreMutationError,
    PersonalWorkerStoreMutationErrorKind, PersonalWorkerStoreMutationReceipt,
    apply_personal_worker_store_mutation,
};
#[cfg(unix)]
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};
use serde::Serialize;

pub const PERSONAL_WORKER_SUBMIT_COMMAND_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersonalWorkerSubmitCommandErrorKind {
    InvalidStoreRoot,
    #[cfg(not(unix))]
    UnsupportedPlatform,
    MissingStore,
    UnsafeStore,
    StoreUnavailable,
    InvalidRevision,
    InvalidGeneration,
    InvalidObservationTime,
    InvalidRequestId,
    InvalidVerificationProfile,
    InvalidRunnerProfile,
    InvalidRepository,
    InvalidCommit,
    InvalidTree,
    InvalidPriority,
    InvalidResources,
    InvalidCacheId,
    InvalidCacheDigest,
    InvalidCacheAccess,
    InvalidSubmissionTime,
    InvalidOperatorDeadline,
    StaleRevision,
    StaleQueueGeneration,
    NotFound,
    Conflict,
    InvalidMutation,
    Busy,
    DurableStateVersionIncompatible,
    CorruptStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct PersonalWorkerSubmitCommandError {
    schema_version: u8,
    kind: PersonalWorkerSubmitCommandErrorKind,
    message: &'static str,
}

impl PersonalWorkerSubmitCommandError {
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn kind(&self) -> PersonalWorkerSubmitCommandErrorKind {
        self.kind
    }

    #[must_use]
    pub(crate) const fn message(&self) -> &'static str {
        self.message
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PersonalWorkerSubmitCommandInput<'a> {
    pub(crate) revision: &'a str,
    pub(crate) generation: &'a str,
    pub(crate) observed_at: &'a str,
    pub(crate) request_id: &'a str,
    pub(crate) verification_profile: &'a str,
    pub(crate) runner_profile: &'a str,
    pub(crate) repository: &'a str,
    pub(crate) commit: &'a str,
    pub(crate) tree: &'a str,
    pub(crate) priority: &'a str,
    pub(crate) cpu_millis: &'a str,
    pub(crate) memory_bytes: &'a str,
    pub(crate) pids: &'a str,
    pub(crate) cache_id: &'a str,
    pub(crate) cache_namespace_digest: &'a str,
    pub(crate) cache_access: &'a str,
    pub(crate) submitted_at: &'a str,
    pub(crate) operator_deadline: Option<&'a str>,
}

pub(crate) fn submit_queued_job(
    store_root: &Path,
    input: PersonalWorkerSubmitCommandInput<'_>,
) -> Result<PersonalWorkerStoreMutationReceipt, PersonalWorkerSubmitCommandError> {
    validate_store_root(store_root)?;
    let revision = PersonalWorkerStoreRevision::new(parse_u64(
        input.revision,
        PersonalWorkerSubmitCommandErrorKind::InvalidRevision,
        "personal worker store revision is invalid",
    )?)
    .map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidRevision,
            "personal worker store revision is outside the bounded positive range",
        )
    })?;
    let generation = PersonalWorkerQueueGeneration::new(parse_u64(
        input.generation,
        PersonalWorkerSubmitCommandErrorKind::InvalidGeneration,
        "personal worker queue generation is invalid",
    )?)
    .map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidGeneration,
            "personal worker queue generation is outside the bounded positive range",
        )
    })?;
    let observed_at = parse_epoch(
        input.observed_at,
        PersonalWorkerSubmitCommandErrorKind::InvalidObservationTime,
        "personal worker submission observation time must be greater than zero",
    )?;
    let submitted_at = parse_epoch(
        input.submitted_at,
        PersonalWorkerSubmitCommandErrorKind::InvalidSubmissionTime,
        "personal worker submission time must be greater than zero",
    )?;
    if submitted_at > observed_at {
        return Err(command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidSubmissionTime,
            "personal worker submission time cannot be newer than its observation",
        ));
    }
    let operator_deadline = input
        .operator_deadline
        .map(|value| {
            parse_epoch(
                value,
                PersonalWorkerSubmitCommandErrorKind::InvalidOperatorDeadline,
                "personal worker operator deadline must be greater than zero",
            )
        })
        .transpose()?;
    if operator_deadline.is_some_and(|deadline| deadline <= submitted_at) {
        return Err(command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidOperatorDeadline,
            "personal worker operator deadline must be later than submission",
        ));
    }

    let request_id = ExecutionRequestId::parse(input.request_id).map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidRequestId,
            "personal worker request ID is invalid",
        )
    })?;
    let verification_profile_id = VerificationProfileId::parse(input.verification_profile)
        .map_err(|_| {
            command_error(
                PersonalWorkerSubmitCommandErrorKind::InvalidVerificationProfile,
                "personal worker verification profile ID is invalid",
            )
        })?;
    let runner_profile_id = RunnerProfileId::parse(input.runner_profile).map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidRunnerProfile,
            "personal worker runner profile ID is invalid",
        )
    })?;
    let repository = RepositoryRef::parse(input.repository).map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidRepository,
            "personal worker repository identity is invalid",
        )
    })?;
    let commit = CommitId::parse(input.commit).map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidCommit,
            "personal worker commit identity is invalid",
        )
    })?;
    let tree = GitTreeId::parse(input.tree).map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidTree,
            "personal worker tree identity is invalid",
        )
    })?;
    let priority = parse_priority(input.priority)?;
    let requested_limits = ExecutionResourceLimits::new(
        parse_u32(
            input.cpu_millis,
            PersonalWorkerSubmitCommandErrorKind::InvalidResources,
            "personal worker CPU limit is invalid",
        )?,
        parse_u64(
            input.memory_bytes,
            PersonalWorkerSubmitCommandErrorKind::InvalidResources,
            "personal worker memory limit is invalid",
        )?,
        parse_u32(
            input.pids,
            PersonalWorkerSubmitCommandErrorKind::InvalidResources,
            "personal worker PID limit is invalid",
        )?,
    )
    .map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidResources,
            "personal worker resource limits are outside the bounded positive range",
        )
    })?;
    let cache_id = CacheId::parse(input.cache_id).map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidCacheId,
            "personal worker cache ID is invalid",
        )
    })?;
    let namespace_digest = Sha256Digest::parse(input.cache_namespace_digest).map_err(|_| {
        command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidCacheDigest,
            "personal worker cache namespace digest is invalid",
        )
    })?;
    let cache_access = parse_cache_access(input.cache_access)?;

    let request = PersonalWorkerJobRequest {
        identity: ExecutionAdmissionIdentity::new(
            request_id,
            verification_profile_id,
            runner_profile_id,
        ),
        source: PersonalWorkerSourceIdentity::new(repository.clone(), commit, tree),
        priority,
        requested_limits,
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id,
            repository,
            namespace_digest,
        },
        cache_access,
        submitted_at,
        operator_deadline,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    };

    apply_submit(store_root, revision, generation, observed_at, request)
}

#[must_use]
pub(crate) fn render_submit_receipt_human(receipt: &PersonalWorkerStoreMutationReceipt) -> String {
    let mut output = String::new();
    writeln!(output, "Personal worker submission").expect("writing to a String cannot fail");
    writeln!(
        output,
        "  disposition: {}",
        serialized_label(&receipt.disposition())
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  mutation: {}",
        serialized_label(&receipt.mutation())
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  store revision: {} -> {}",
        receipt.old_revision().get(),
        receipt.new_revision().get()
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  queue generation: {} -> {}",
        receipt.old_queue_generation().get(),
        receipt.new_queue_generation().get()
    )
    .expect("writing to a String cannot fail");
    output
}

fn serialized_label(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(value)) => value,
        Ok(value) => value.to_string(),
        Err(_) => "unavailable".to_owned(),
    }
}

#[cfg(unix)]
fn apply_submit(
    store_root: &Path,
    revision: PersonalWorkerStoreRevision,
    generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
    request: PersonalWorkerJobRequest,
) -> Result<PersonalWorkerStoreMutationReceipt, PersonalWorkerSubmitCommandError> {
    let mut store =
        UnixPersonalWorkerStore::open_existing_read_only(store_root).map_err(map_store_error)?;
    apply_personal_worker_store_mutation(
        &mut store,
        revision,
        generation,
        PersonalWorkerStoreMutation::Submit {
            request,
            observed_at,
        },
    )
    .map_err(map_mutation_error)
}

#[cfg(not(unix))]
fn apply_submit(
    _store_root: &Path,
    _revision: PersonalWorkerStoreRevision,
    _generation: PersonalWorkerQueueGeneration,
    _observed_at: EpochMillis,
    _request: PersonalWorkerJobRequest,
) -> Result<PersonalWorkerStoreMutationReceipt, PersonalWorkerSubmitCommandError> {
    Err(command_error(
        PersonalWorkerSubmitCommandErrorKind::UnsupportedPlatform,
        "personal worker durable submission currently supports Unix platforms only",
    ))
}

fn parse_priority(value: &str) -> Result<PersonalWorkerPriority, PersonalWorkerSubmitCommandError> {
    match value {
        "background" => Ok(PersonalWorkerPriority::Background),
        "normal" => Ok(PersonalWorkerPriority::Normal),
        "interactive" => Ok(PersonalWorkerPriority::Interactive),
        _ => Err(command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidPriority,
            "personal worker priority must be background, normal, or interactive",
        )),
    }
}

fn parse_cache_access(
    value: &str,
) -> Result<PersonalWorkerCacheAccessMode, PersonalWorkerSubmitCommandError> {
    match value {
        "read" => Ok(PersonalWorkerCacheAccessMode::Read),
        "write" => Ok(PersonalWorkerCacheAccessMode::Write),
        "exclusive" => Ok(PersonalWorkerCacheAccessMode::Exclusive),
        _ => Err(command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidCacheAccess,
            "personal worker cache access must be read, write, or exclusive",
        )),
    }
}

fn parse_epoch(
    value: &str,
    kind: PersonalWorkerSubmitCommandErrorKind,
    message: &'static str,
) -> Result<EpochMillis, PersonalWorkerSubmitCommandError> {
    EpochMillis::new(parse_u64(value, kind, message)?).map_err(|_| command_error(kind, message))
}

fn parse_u64(
    value: &str,
    kind: PersonalWorkerSubmitCommandErrorKind,
    message: &'static str,
) -> Result<u64, PersonalWorkerSubmitCommandError> {
    value.parse().map_err(|_| command_error(kind, message))
}

fn parse_u32(
    value: &str,
    kind: PersonalWorkerSubmitCommandErrorKind,
    message: &'static str,
) -> Result<u32, PersonalWorkerSubmitCommandError> {
    value.parse().map_err(|_| command_error(kind, message))
}

fn validate_store_root(store_root: &Path) -> Result<(), PersonalWorkerSubmitCommandError> {
    let normalized = store_root.components().collect::<PathBuf>();
    if !store_root.is_absolute()
        || normalized.as_os_str() != store_root.as_os_str()
        || store_root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(command_error(
            PersonalWorkerSubmitCommandErrorKind::InvalidStoreRoot,
            "personal worker store root must be an explicit absolute normalized path",
        ));
    }
    Ok(())
}

fn map_store_error(error: PersonalWorkerStoreError) -> PersonalWorkerSubmitCommandError {
    match error.kind() {
        PersonalWorkerStoreErrorKind::Missing => command_error(
            PersonalWorkerSubmitCommandErrorKind::MissingStore,
            "durable personal worker state does not exist",
        ),
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => command_error(
            PersonalWorkerSubmitCommandErrorKind::UnsafeStore,
            "durable personal worker state filesystem is unsafe",
        ),
        PersonalWorkerStoreErrorKind::Busy => command_error(
            PersonalWorkerSubmitCommandErrorKind::Busy,
            "another personal worker store mutation holds the writer lock",
        ),
        PersonalWorkerStoreErrorKind::VersionIncompatible => command_error(
            PersonalWorkerSubmitCommandErrorKind::DurableStateVersionIncompatible,
            "durable personal worker state schema is incompatible; explicit migration is required",
        ),
        PersonalWorkerStoreErrorKind::CorruptState
        | PersonalWorkerStoreErrorKind::InvalidDocument => command_error(
            PersonalWorkerSubmitCommandErrorKind::CorruptStore,
            "durable personal worker state is corrupt or noncanonical",
        ),
        PersonalWorkerStoreErrorKind::RevisionConflict | PersonalWorkerStoreErrorKind::Io => {
            command_error(
                PersonalWorkerSubmitCommandErrorKind::StoreUnavailable,
                "durable personal worker state is unavailable",
            )
        }
    }
}

fn map_mutation_error(error: PersonalWorkerStoreMutationError) -> PersonalWorkerSubmitCommandError {
    let kind = match error.kind() {
        PersonalWorkerStoreMutationErrorKind::MissingState => {
            PersonalWorkerSubmitCommandErrorKind::MissingStore
        }
        PersonalWorkerStoreMutationErrorKind::StaleRevision => {
            PersonalWorkerSubmitCommandErrorKind::StaleRevision
        }
        PersonalWorkerStoreMutationErrorKind::StaleQueueGeneration => {
            PersonalWorkerSubmitCommandErrorKind::StaleQueueGeneration
        }
        PersonalWorkerStoreMutationErrorKind::NotFound => {
            PersonalWorkerSubmitCommandErrorKind::NotFound
        }
        PersonalWorkerStoreMutationErrorKind::Conflict => {
            PersonalWorkerSubmitCommandErrorKind::Conflict
        }
        PersonalWorkerStoreMutationErrorKind::CapacityReached => {
            PersonalWorkerSubmitCommandErrorKind::InvalidMutation
        }
        PersonalWorkerStoreMutationErrorKind::InvalidMutation => {
            PersonalWorkerSubmitCommandErrorKind::InvalidMutation
        }
        PersonalWorkerStoreMutationErrorKind::Busy => PersonalWorkerSubmitCommandErrorKind::Busy,
        PersonalWorkerStoreMutationErrorKind::Io => {
            PersonalWorkerSubmitCommandErrorKind::StoreUnavailable
        }
        PersonalWorkerStoreMutationErrorKind::UnsafeFilesystem => {
            PersonalWorkerSubmitCommandErrorKind::UnsafeStore
        }
        PersonalWorkerStoreMutationErrorKind::VersionIncompatible => {
            PersonalWorkerSubmitCommandErrorKind::DurableStateVersionIncompatible
        }
        PersonalWorkerStoreMutationErrorKind::CorruptState => {
            PersonalWorkerSubmitCommandErrorKind::CorruptStore
        }
    };
    command_error(kind, error.message())
}

const fn command_error(
    kind: PersonalWorkerSubmitCommandErrorKind,
    message: &'static str,
) -> PersonalWorkerSubmitCommandError {
    PersonalWorkerSubmitCommandError {
        schema_version: PERSONAL_WORKER_SUBMIT_COMMAND_SCHEMA_VERSION,
        kind,
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        PersonalWorkerSubmitCommandErrorKind, PersonalWorkerSubmitCommandInput, submit_queued_job,
        validate_store_root,
    };

    fn input<'a>() -> PersonalWorkerSubmitCommandInput<'a> {
        PersonalWorkerSubmitCommandInput {
            revision: "1",
            generation: "1",
            observed_at: "1000",
            request_id: "request-one",
            verification_profile: "smolrunner.required",
            runner_profile: "personal-lima-work",
            repository: "example/project",
            commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            tree: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            priority: "normal",
            cpu_millis: "2000",
            memory_bytes: "2147483648",
            pids: "2048",
            cache_id: "build-cache",
            cache_namespace_digest: "sha256:abababababababababababababababababababababababababababababababab",
            cache_access: "write",
            submitted_at: "900",
            operator_deadline: None,
        }
    }

    #[test]
    fn submission_root_requires_an_absolute_normalized_path() {
        for invalid in [
            "relative/root",
            "/tmp/./private-root",
            "/tmp//private-root",
            "/tmp/../private-root",
        ] {
            assert_eq!(
                validate_store_root(Path::new(invalid))
                    .expect_err("invalid root")
                    .kind(),
                PersonalWorkerSubmitCommandErrorKind::InvalidStoreRoot
            );
        }
        assert!(validate_store_root(Path::new("/tmp/private-root")).is_ok());
    }

    #[test]
    fn invalid_input_errors_do_not_disclose_private_values() {
        let mut input = input();
        input.repository = "private repository sentinel";
        let error = submit_queued_job(Path::new("/tmp/private-root"), input)
            .expect_err("invalid repository must fail before I/O");
        let encoded = serde_json::to_string(&error).expect("serialize error");
        assert!(!encoded.contains("private repository sentinel"));
        assert!(!encoded.contains("/tmp/private-root"));
    }
}
