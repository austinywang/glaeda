use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use glaeda::execution_admission::{EpochMillis, ExecutionRequestId};
use glaeda::personal_worker_queue::PersonalWorkerQueueGeneration;
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
use serde::Serialize;

pub const PERSONAL_WORKER_CANCEL_COMMAND_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersonalWorkerCancelCommandErrorKind {
    InvalidStoreRoot,
    #[cfg(not(unix))]
    UnsupportedPlatform,
    MissingStore,
    UnsafeStore,
    StoreUnavailable,
    InvalidRevision,
    InvalidGeneration,
    InvalidCancellationTime,
    InvalidRequestId,
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
pub(crate) struct PersonalWorkerCancelCommandError {
    schema_version: u8,
    kind: PersonalWorkerCancelCommandErrorKind,
    message: &'static str,
}

impl PersonalWorkerCancelCommandError {
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn kind(&self) -> PersonalWorkerCancelCommandErrorKind {
        self.kind
    }

    #[must_use]
    pub(crate) const fn message(&self) -> &'static str {
        self.message
    }
}

pub(crate) fn cancel_queued_job(
    store_root: &Path,
    revision: u64,
    generation: u64,
    cancelled_at: u64,
    request_id: &str,
) -> Result<PersonalWorkerStoreMutationReceipt, PersonalWorkerCancelCommandError> {
    validate_store_root(store_root)?;
    let revision = PersonalWorkerStoreRevision::new(revision).map_err(|_| {
        command_error(
            PersonalWorkerCancelCommandErrorKind::InvalidRevision,
            "personal worker store revision is outside the bounded positive range",
        )
    })?;
    let generation = PersonalWorkerQueueGeneration::new(generation).map_err(|_| {
        command_error(
            PersonalWorkerCancelCommandErrorKind::InvalidGeneration,
            "personal worker queue generation is outside the bounded positive range",
        )
    })?;
    let cancelled_at = EpochMillis::new(cancelled_at).map_err(|_| {
        command_error(
            PersonalWorkerCancelCommandErrorKind::InvalidCancellationTime,
            "personal worker cancellation time must be greater than zero",
        )
    })?;
    let request_id = ExecutionRequestId::parse(request_id).map_err(|_| {
        command_error(
            PersonalWorkerCancelCommandErrorKind::InvalidRequestId,
            "personal worker request ID is invalid",
        )
    })?;
    apply_cancel(store_root, revision, generation, request_id, cancelled_at)
}

#[must_use]
pub(crate) fn render_cancel_receipt_human(receipt: &PersonalWorkerStoreMutationReceipt) -> String {
    let mut output = String::new();
    writeln!(output, "Personal worker cancellation").expect("writing to a String cannot fail");
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
fn apply_cancel(
    store_root: &Path,
    revision: PersonalWorkerStoreRevision,
    generation: PersonalWorkerQueueGeneration,
    request_id: ExecutionRequestId,
    cancelled_at: EpochMillis,
) -> Result<PersonalWorkerStoreMutationReceipt, PersonalWorkerCancelCommandError> {
    let mut store =
        UnixPersonalWorkerStore::open_existing_read_only(store_root).map_err(map_store_error)?;
    apply_personal_worker_store_mutation(
        &mut store,
        revision,
        generation,
        PersonalWorkerStoreMutation::Cancel {
            request_id,
            cancelled_at,
            draining_admission: None,
        },
    )
    .map_err(map_mutation_error)
}

#[cfg(not(unix))]
fn apply_cancel(
    _store_root: &Path,
    _revision: PersonalWorkerStoreRevision,
    _generation: PersonalWorkerQueueGeneration,
    _request_id: ExecutionRequestId,
    _cancelled_at: EpochMillis,
) -> Result<PersonalWorkerStoreMutationReceipt, PersonalWorkerCancelCommandError> {
    Err(command_error(
        PersonalWorkerCancelCommandErrorKind::UnsupportedPlatform,
        "personal worker durable cancellation currently supports Unix platforms only",
    ))
}

fn validate_store_root(store_root: &Path) -> Result<(), PersonalWorkerCancelCommandError> {
    let normalized = store_root.components().collect::<PathBuf>();
    if !store_root.is_absolute()
        || normalized.as_os_str() != store_root.as_os_str()
        || store_root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(command_error(
            PersonalWorkerCancelCommandErrorKind::InvalidStoreRoot,
            "personal worker store root must be an explicit absolute normalized path",
        ));
    }
    Ok(())
}

fn map_store_error(error: PersonalWorkerStoreError) -> PersonalWorkerCancelCommandError {
    match error.kind() {
        PersonalWorkerStoreErrorKind::Missing => command_error(
            PersonalWorkerCancelCommandErrorKind::MissingStore,
            "durable personal worker state does not exist",
        ),
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => command_error(
            PersonalWorkerCancelCommandErrorKind::UnsafeStore,
            "durable personal worker state filesystem is unsafe",
        ),
        PersonalWorkerStoreErrorKind::Busy => command_error(
            PersonalWorkerCancelCommandErrorKind::Busy,
            "another personal worker store mutation holds the writer lock",
        ),
        PersonalWorkerStoreErrorKind::VersionIncompatible => command_error(
            PersonalWorkerCancelCommandErrorKind::DurableStateVersionIncompatible,
            "durable personal worker state schema is incompatible; explicit migration is required",
        ),
        PersonalWorkerStoreErrorKind::CorruptState
        | PersonalWorkerStoreErrorKind::InvalidDocument => command_error(
            PersonalWorkerCancelCommandErrorKind::CorruptStore,
            "durable personal worker state is corrupt or noncanonical",
        ),
        PersonalWorkerStoreErrorKind::RevisionConflict | PersonalWorkerStoreErrorKind::Io => {
            command_error(
                PersonalWorkerCancelCommandErrorKind::StoreUnavailable,
                "durable personal worker state is unavailable",
            )
        }
    }
}

fn map_mutation_error(error: PersonalWorkerStoreMutationError) -> PersonalWorkerCancelCommandError {
    let kind = match error.kind() {
        PersonalWorkerStoreMutationErrorKind::MissingState => {
            PersonalWorkerCancelCommandErrorKind::MissingStore
        }
        PersonalWorkerStoreMutationErrorKind::StaleRevision => {
            PersonalWorkerCancelCommandErrorKind::StaleRevision
        }
        PersonalWorkerStoreMutationErrorKind::StaleQueueGeneration => {
            PersonalWorkerCancelCommandErrorKind::StaleQueueGeneration
        }
        PersonalWorkerStoreMutationErrorKind::NotFound => {
            PersonalWorkerCancelCommandErrorKind::NotFound
        }
        PersonalWorkerStoreMutationErrorKind::Conflict => {
            PersonalWorkerCancelCommandErrorKind::Conflict
        }
        PersonalWorkerStoreMutationErrorKind::CapacityReached => {
            PersonalWorkerCancelCommandErrorKind::InvalidMutation
        }
        PersonalWorkerStoreMutationErrorKind::InvalidMutation => {
            PersonalWorkerCancelCommandErrorKind::InvalidMutation
        }
        PersonalWorkerStoreMutationErrorKind::Busy => PersonalWorkerCancelCommandErrorKind::Busy,
        PersonalWorkerStoreMutationErrorKind::Io => {
            PersonalWorkerCancelCommandErrorKind::StoreUnavailable
        }
        PersonalWorkerStoreMutationErrorKind::UnsafeFilesystem => {
            PersonalWorkerCancelCommandErrorKind::UnsafeStore
        }
        PersonalWorkerStoreMutationErrorKind::VersionIncompatible => {
            PersonalWorkerCancelCommandErrorKind::DurableStateVersionIncompatible
        }
        PersonalWorkerStoreMutationErrorKind::CorruptState => {
            PersonalWorkerCancelCommandErrorKind::CorruptStore
        }
    };
    command_error(kind, error.message())
}

const fn command_error(
    kind: PersonalWorkerCancelCommandErrorKind,
    message: &'static str,
) -> PersonalWorkerCancelCommandError {
    PersonalWorkerCancelCommandError {
        schema_version: PERSONAL_WORKER_CANCEL_COMMAND_SCHEMA_VERSION,
        kind,
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{PersonalWorkerCancelCommandErrorKind, cancel_queued_job, validate_store_root};

    #[test]
    fn cancellation_root_requires_an_absolute_normalized_path() {
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
                PersonalWorkerCancelCommandErrorKind::InvalidStoreRoot
            );
        }
        assert!(validate_store_root(Path::new("/tmp/private-root")).is_ok());
    }

    #[test]
    fn invalid_input_errors_do_not_disclose_private_values() {
        let error = cancel_queued_job(
            Path::new("private-relative-root"),
            1,
            1,
            1,
            "private-request-id",
        )
        .expect_err("invalid root must fail before I/O");
        let encoded = serde_json::to_string(&error).expect("serialize error");
        assert!(!encoded.contains("private-relative-root"));
        assert!(!encoded.contains("private-request-id"));
    }
}
