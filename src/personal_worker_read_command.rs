use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use glaeda::execution_admission::ExecutionRequestId;
use glaeda::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerQueueGeneration, PersonalWorkerQueueVisibility,
};
use glaeda::personal_worker_read_model::{
    PersonalWorkerJobReadRequest, PersonalWorkerJobStateView, PersonalWorkerJobView,
    PersonalWorkerQueuePage, PersonalWorkerQueuePageRequest, PersonalWorkerReadError,
    PersonalWorkerReadErrorKind, PersonalWorkerStatusView, personal_worker_job_view,
    personal_worker_queue_page, personal_worker_status,
};
use glaeda::personal_worker_store::{
    PersonalWorkerStore, PersonalWorkerStoreDocument, PersonalWorkerStoreError,
    PersonalWorkerStoreErrorKind, PersonalWorkerStoreRevision,
};
#[cfg(unix)]
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use serde::Serialize;

pub const PERSONAL_WORKER_READ_COMMAND_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersonalWorkerReadCommandErrorKind {
    InvalidStoreRoot,
    #[cfg(not(unix))]
    UnsupportedPlatform,
    MissingStore,
    UnsafeStore,
    DurableStateVersionIncompatible,
    CorruptStore,
    StoreUnavailable,
    InvalidRevision,
    InvalidGeneration,
    InvalidRequestId,
    StaleRevision,
    StaleQueueGeneration,
    InvalidPage,
    OffsetOutOfBounds,
    NotFound,
    InvalidDocument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct PersonalWorkerReadCommandError {
    schema_version: u8,
    kind: PersonalWorkerReadCommandErrorKind,
    message: &'static str,
}

impl PersonalWorkerReadCommandError {
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn kind(&self) -> PersonalWorkerReadCommandErrorKind {
        self.kind
    }

    #[must_use]
    pub(crate) const fn message(&self) -> &'static str {
        self.message
    }
}

pub(crate) fn read_status(
    store_root: &Path,
) -> Result<PersonalWorkerStatusView, PersonalWorkerReadCommandError> {
    let document = load_snapshot(store_root)?;
    personal_worker_status(&document).map_err(map_read_error)
}

pub(crate) fn read_queue_page(
    store_root: &Path,
    revision: u64,
    generation: u64,
    offset: u32,
    limit: u16,
) -> Result<PersonalWorkerQueuePage, PersonalWorkerReadCommandError> {
    let document = load_snapshot(store_root)?;
    let revision = PersonalWorkerStoreRevision::new(revision).map_err(|_| {
        command_error(
            PersonalWorkerReadCommandErrorKind::InvalidRevision,
            "personal worker store revision is outside the bounded positive range",
        )
    })?;
    let generation = PersonalWorkerQueueGeneration::new(generation).map_err(|_| {
        command_error(
            PersonalWorkerReadCommandErrorKind::InvalidGeneration,
            "personal worker queue generation is outside the bounded positive range",
        )
    })?;
    let request = PersonalWorkerQueuePageRequest::new(revision, generation, offset, limit)
        .map_err(map_read_error)?;
    personal_worker_queue_page(&document, request).map_err(map_read_error)
}

pub(crate) fn read_job(
    store_root: &Path,
    revision: u64,
    generation: u64,
    request_id: &str,
) -> Result<PersonalWorkerJobView, PersonalWorkerReadCommandError> {
    let document = load_snapshot(store_root)?;
    let revision = PersonalWorkerStoreRevision::new(revision).map_err(|_| {
        command_error(
            PersonalWorkerReadCommandErrorKind::InvalidRevision,
            "personal worker store revision is outside the bounded positive range",
        )
    })?;
    let generation = PersonalWorkerQueueGeneration::new(generation).map_err(|_| {
        command_error(
            PersonalWorkerReadCommandErrorKind::InvalidGeneration,
            "personal worker queue generation is outside the bounded positive range",
        )
    })?;
    let request_id = ExecutionRequestId::parse(request_id).map_err(|_| {
        command_error(
            PersonalWorkerReadCommandErrorKind::InvalidRequestId,
            "personal worker request ID is invalid",
        )
    })?;
    personal_worker_job_view(
        &document,
        PersonalWorkerJobReadRequest::new(revision, generation, request_id),
    )
    .map_err(map_read_error)
}

#[must_use]
pub(crate) fn render_status_human(view: &PersonalWorkerStatusView) -> String {
    let mut output = String::new();
    writeln!(output, "Personal worker status").expect("writing to a String cannot fail");
    writeln!(output, "  store revision: {}", view.store_revision().get())
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  queue generation: {}",
        view.queue_generation().get()
    )
    .expect("writing to a String cannot fail");
    match view.activity_evidence() {
        PersonalWorkerActivityEvidence::Never => {
            writeln!(output, "  activity: never").expect("writing to a String cannot fail");
        }
        PersonalWorkerActivityEvidence::Observed { last_activity_at } => {
            writeln!(output, "  activity: observed at {}", last_activity_at.get())
                .expect("writing to a String cannot fail");
        }
    }
    writeln!(
        output,
        "  profile: {} -> {}",
        view.current_profile()
            .map(|profile| serialized_label(&profile))
            .unwrap_or_else(|| "unobserved".to_owned()),
        serialized_label(&view.desired_profile())
    )
    .expect("writing to a String cannot fail");
    writeln!(output, "  queued: {}", view.queued_entry_count())
        .expect("writing to a String cannot fail");
    writeln!(output, "  eligible: {}", view.eligible_queue_count())
        .expect("writing to a String cannot fail");
    writeln!(output, "  cancelled: {}", view.cancelled_queue_count())
        .expect("writing to a String cannot fail");
    writeln!(output, "  selected: {}", view.selected_count())
        .expect("writing to a String cannot fail");
    writeln!(output, "  active: {}", view.active_count()).expect("writing to a String cannot fail");
    writeln!(output, "  draining: {}", view.draining_count())
        .expect("writing to a String cannot fail");
    writeln!(output, "  cache leases: {}", view.cache_lease_count())
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  retained terminal jobs: {}",
        view.terminal_tombstone_count()
    )
    .expect("writing to a String cannot fail");
    if let Some(intent) = view.pending_profile_change() {
        writeln!(
            output,
            "  pending profile: {} at {}",
            serialized_label(&intent.target()),
            intent.requested_at().get()
        )
        .expect("writing to a String cannot fail");
    }
    output
}

#[must_use]
pub(crate) fn render_queue_page_human(view: &PersonalWorkerQueuePage) -> String {
    let mut output = String::new();
    writeln!(output, "Personal worker queue").expect("writing to a String cannot fail");
    writeln!(output, "  store revision: {}", view.store_revision().get())
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  queue generation: {}",
        view.queue_generation().get()
    )
    .expect("writing to a String cannot fail");
    writeln!(output, "  total live jobs: {}", view.total())
        .expect("writing to a String cannot fail");
    match view.next_offset() {
        Some(offset) => writeln!(output, "  next offset: {offset}"),
        None => writeln!(output, "  next offset: none"),
    }
    .expect("writing to a String cannot fail");
    for entry in view.items() {
        write_queue_entry(&mut output, entry, "  - ");
    }
    output
}

#[must_use]
pub(crate) fn render_job_human(view: &PersonalWorkerJobView) -> String {
    let mut output = String::new();
    writeln!(output, "Personal worker job").expect("writing to a String cannot fail");
    writeln!(output, "  store revision: {}", view.store_revision().get())
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  queue generation: {}",
        view.queue_generation().get()
    )
    .expect("writing to a String cannot fail");
    writeln!(output, "  submitted at: {}", view.submitted_at().get())
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  cancellation: {}",
        serialized_label(&view.cancellation())
    )
    .expect("writing to a String cannot fail");

    match view.state() {
        PersonalWorkerJobStateView::Queued { entry } => {
            writeln!(output, "  state: queued").expect("writing to a String cannot fail");
            write_queue_entry(&mut output, entry, "  ");
        }
        PersonalWorkerJobStateView::Active {
            entry,
            admission,
            durable_cache_lease,
        } => {
            writeln!(output, "  state: active").expect("writing to a String cannot fail");
            write_queue_entry(&mut output, entry, "  ");
            writeln!(
                output,
                "  admission: {} at {}",
                serialized_label(&admission.state()),
                admission.observed_at().get()
            )
            .expect("writing to a String cannot fail");
            writeln!(
                output,
                "  reservation: {} generation {}",
                admission.reservation().id().as_str(),
                admission.reservation().generation().get()
            )
            .expect("writing to a String cannot fail");
            writeln!(
                output,
                "  cache lease reservation: {} generation {}",
                durable_cache_lease.reservation_id().as_str(),
                durable_cache_lease.reservation_generation().get()
            )
            .expect("writing to a String cannot fail");
        }
        PersonalWorkerJobStateView::Terminal { terminal } => {
            writeln!(output, "  state: terminal").expect("writing to a String cannot fail");
            writeln!(
                output,
                "  request ID: {}",
                terminal.request().identity().request_id.as_str()
            )
            .expect("writing to a String cannot fail");
            writeln!(
                output,
                "  repository: {}",
                terminal.request().source().repository.as_str()
            )
            .expect("writing to a String cannot fail");
            writeln!(
                output,
                "  admission: {}",
                serialized_label(&terminal.admission_state())
            )
            .expect("writing to a String cannot fail");
            writeln!(output, "  completed at: {}", terminal.completed_at().get())
                .expect("writing to a String cannot fail");
            writeln!(
                output,
                "  reason: {}",
                serialized_label(&terminal.unavailable_reason())
            )
            .expect("writing to a String cannot fail");
            writeln!(
                output,
                "  reservation: {} generation {}",
                terminal.reservation().id().as_str(),
                terminal.reservation().generation().get()
            )
            .expect("writing to a String cannot fail");
        }
    }
    output
}

fn write_queue_entry(output: &mut String, entry: &PersonalWorkerQueueVisibility, prefix: &str) {
    writeln!(
        output,
        "{prefix}request={} state={} repository={} commit={} priority={} position={}",
        entry.request_id.as_str(),
        serialized_label(&entry.state),
        entry.repository.as_str(),
        entry.commit.as_str(),
        serialized_label(&entry.priority),
        entry
            .queue_position
            .map_or_else(|| "none".to_owned(), |position| position.to_string())
    )
    .expect("writing to a String cannot fail");
}

fn serialized_label(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(value)) => value,
        Ok(value) => value.to_string(),
        Err(_) => "unavailable".to_owned(),
    }
}

fn validate_store_root(store_root: &Path) -> Result<(), PersonalWorkerReadCommandError> {
    let normalized = store_root.components().collect::<PathBuf>();
    if !store_root.is_absolute()
        || normalized.as_os_str() != store_root.as_os_str()
        || store_root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(command_error(
            PersonalWorkerReadCommandErrorKind::InvalidStoreRoot,
            "personal worker store root must be an explicit absolute normalized path",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn load_snapshot(
    store_root: &Path,
) -> Result<PersonalWorkerStoreDocument, PersonalWorkerReadCommandError> {
    validate_store_root(store_root)?;
    let store =
        UnixPersonalWorkerStore::open_existing_read_only(store_root).map_err(map_store_error)?;
    store.load().map_err(map_store_error)?.ok_or_else(|| {
        command_error(
            PersonalWorkerReadCommandErrorKind::MissingStore,
            "durable personal worker state does not exist",
        )
    })
}

#[cfg(not(unix))]
fn load_snapshot(
    store_root: &Path,
) -> Result<PersonalWorkerStoreDocument, PersonalWorkerReadCommandError> {
    validate_store_root(store_root)?;
    Err(command_error(
        PersonalWorkerReadCommandErrorKind::UnsupportedPlatform,
        "personal worker durable reads currently support Unix platforms only",
    ))
}

fn map_store_error(error: PersonalWorkerStoreError) -> PersonalWorkerReadCommandError {
    match error.kind() {
        PersonalWorkerStoreErrorKind::Missing => command_error(
            PersonalWorkerReadCommandErrorKind::MissingStore,
            "durable personal worker state does not exist",
        ),
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => command_error(
            PersonalWorkerReadCommandErrorKind::UnsafeStore,
            "durable personal worker state filesystem is unsafe",
        ),
        PersonalWorkerStoreErrorKind::VersionIncompatible => command_error(
            PersonalWorkerReadCommandErrorKind::DurableStateVersionIncompatible,
            "durable personal worker state schema is incompatible; explicit migration is required",
        ),
        PersonalWorkerStoreErrorKind::CorruptState
        | PersonalWorkerStoreErrorKind::InvalidDocument => command_error(
            PersonalWorkerReadCommandErrorKind::CorruptStore,
            "durable personal worker state is corrupt or noncanonical",
        ),
        PersonalWorkerStoreErrorKind::RevisionConflict
        | PersonalWorkerStoreErrorKind::Busy
        | PersonalWorkerStoreErrorKind::Io => command_error(
            PersonalWorkerReadCommandErrorKind::StoreUnavailable,
            "durable personal worker state is unavailable",
        ),
    }
}

fn map_read_error(error: PersonalWorkerReadError) -> PersonalWorkerReadCommandError {
    let kind = match error.kind() {
        PersonalWorkerReadErrorKind::StaleRevision => {
            PersonalWorkerReadCommandErrorKind::StaleRevision
        }
        PersonalWorkerReadErrorKind::StaleQueueGeneration => {
            PersonalWorkerReadCommandErrorKind::StaleQueueGeneration
        }
        PersonalWorkerReadErrorKind::InvalidPage => PersonalWorkerReadCommandErrorKind::InvalidPage,
        PersonalWorkerReadErrorKind::OffsetOutOfBounds => {
            PersonalWorkerReadCommandErrorKind::OffsetOutOfBounds
        }
        PersonalWorkerReadErrorKind::NotFound => PersonalWorkerReadCommandErrorKind::NotFound,
        PersonalWorkerReadErrorKind::InvalidDocument => {
            PersonalWorkerReadCommandErrorKind::InvalidDocument
        }
    };
    command_error(kind, error.message())
}

const fn command_error(
    kind: PersonalWorkerReadCommandErrorKind,
    message: &'static str,
) -> PersonalWorkerReadCommandError {
    PersonalWorkerReadCommandError {
        schema_version: PERSONAL_WORKER_READ_COMMAND_SCHEMA_VERSION,
        kind,
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{PersonalWorkerReadCommandErrorKind, read_status, validate_store_root};

    #[test]
    fn store_root_requires_an_absolute_normalized_path() {
        assert_eq!(
            validate_store_root(Path::new("relative/root"))
                .expect_err("relative path")
                .kind(),
            PersonalWorkerReadCommandErrorKind::InvalidStoreRoot
        );
        assert_eq!(
            validate_store_root(Path::new("/tmp/./private-root"))
                .expect_err("current-directory component")
                .kind(),
            PersonalWorkerReadCommandErrorKind::InvalidStoreRoot
        );
        assert_eq!(
            validate_store_root(Path::new("/tmp//private-root"))
                .expect_err("repeated separator")
                .kind(),
            PersonalWorkerReadCommandErrorKind::InvalidStoreRoot
        );
        assert_eq!(
            validate_store_root(Path::new("/tmp/../private-root"))
                .expect_err("parent component")
                .kind(),
            PersonalWorkerReadCommandErrorKind::InvalidStoreRoot
        );
        assert!(validate_store_root(Path::new("/tmp/private-root")).is_ok());
    }

    #[test]
    fn invalid_root_error_does_not_disclose_the_supplied_path() {
        let error = read_status(Path::new("private-relative-root"))
            .expect_err("invalid root must fail before I/O");
        let encoded = serde_json::to_string(&error).expect("serialize error");
        assert!(!encoded.contains("private-relative-root"));
        assert_eq!(
            error.kind(),
            PersonalWorkerReadCommandErrorKind::InvalidStoreRoot
        );
    }
}
