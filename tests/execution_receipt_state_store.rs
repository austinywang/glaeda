use std::collections::BTreeMap;

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::artifact::{RepositoryRef, Sha256Digest};
use glaeda::execution_receipt::{
    ExecutionReceipt, ExecutionReceiptAction, ExecutionReceiptActionOutcome,
    ExecutionReceiptContinuation, ExecutionReceiptDisposition, ReceiptTimestamp,
    encode_execution_receipt,
};
use glaeda::execution_receipt_store::{
    ExecutionReceiptPublicationDisposition, ExecutionReceiptStoreErrorKind,
    publish_execution_receipt, read_execution_receipt,
};
use glaeda::journal::{ExecutionLane, RollbackClass};
#[cfg(target_os = "linux")]
use glaeda::linux_state::LinuxStateRoot;
#[cfg(target_os = "linux")]
use glaeda::linux_state_prepare::prepare_installation;
use glaeda::state::{InstallationId, JournalId, StateComponent, StatePath};
use glaeda::state_store::{
    StateRead, StateRecord, StateStore, StateStoreError, StateStoreErrorKind,
    StateWriteDisposition, StateWriteReceipt,
};

#[derive(Default)]
struct MemoryStore {
    entries: BTreeMap<Vec<String>, Vec<u8>>,
}

impl MemoryStore {
    fn key(path: &StatePath) -> Vec<String> {
        path.components()
            .iter()
            .map(StateComponent::as_str)
            .map(str::to_owned)
            .collect()
    }
}

impl StateStore for MemoryStore {
    fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError> {
        Ok(self
            .entries
            .get(&Self::key(path))
            .cloned()
            .map_or(StateRead::Missing, StateRead::Present))
    }

    fn create_atomic(
        &mut self,
        record: &StateRecord,
    ) -> Result<StateWriteReceipt, StateStoreError> {
        match self.entries.entry(Self::key(record.path())) {
            std::collections::btree_map::Entry::Occupied(_) => Err(StateStoreError::public(
                StateStoreErrorKind::Conflict,
                "state destination already exists",
            )),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(record.bytes().to_vec());
                Ok(StateWriteReceipt::new(
                    StateWriteDisposition::Created,
                    record.bytes().len(),
                ))
            }
        }
    }

    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError> {
        let replaced = self
            .entries
            .insert(Self::key(record.path()), record.bytes().to_vec())
            .is_some();
        Ok(StateWriteReceipt::new(
            if replaced {
                StateWriteDisposition::Replaced
            } else {
                StateWriteDisposition::Created
            },
            record.bytes().len(),
        ))
    }
}

fn installation_id() -> InstallationId {
    InstallationId::parse("0123456789abcdef").expect("installation ID")
}

fn execution_id() -> JournalId {
    JournalId::parse("host-prepare-0001").expect("execution ID")
}

fn receipt(action_id: &str, terminal_at: &str) -> ExecutionReceipt {
    ExecutionReceipt::new_host_preparation(
        execution_id(),
        RepositoryRef::parse("example/project").expect("repository"),
        Sha256Digest::parse(&format!("sha256:{}", "a".repeat(64))).expect("source digest"),
        "host-preparation-root-phase",
        ReceiptTimestamp::parse("2026-07-26T20:00:00.000Z").expect("start time"),
        ReceiptTimestamp::parse(terminal_at).expect("terminal time"),
        ExecutionReceiptDisposition::Completed,
        vec![
            ExecutionReceiptAction::new(
                action_id,
                ExecutionLane::Root,
                RollbackClass::Reversible,
                ExecutionReceiptActionOutcome::Completed,
                None,
            )
            .expect("receipt action"),
        ],
        ExecutionReceiptContinuation::new(
            false,
            std::iter::empty::<&str>(),
            std::iter::empty::<&str>(),
        )
        .expect("continuation"),
    )
    .expect("execution receipt")
}

#[test]
fn atomic_publication_creates_then_exact_replay_is_duplicate() {
    let mut store = MemoryStore::default();
    let value = receipt("ensure-host", "2026-07-26T20:00:01.000Z");

    let created =
        publish_execution_receipt(&mut store, &installation_id(), &value).expect("publish receipt");
    assert_eq!(
        created.disposition(),
        ExecutionReceiptPublicationDisposition::Created
    );
    assert_eq!(created.execution_id(), value.execution_id());
    assert_eq!(
        created.bytes_written(),
        encode_execution_receipt(&value).unwrap().len()
    );

    let duplicate = publish_execution_receipt(&mut store, &installation_id(), &value)
        .expect("replay exact receipt");
    assert_eq!(
        duplicate.disposition(),
        ExecutionReceiptPublicationDisposition::Duplicate
    );
    assert_eq!(duplicate.bytes_written(), 0);
    assert_eq!(store.entries.len(), 1);

    let read = read_execution_receipt(&store, &installation_id(), value.execution_id())
        .expect("read receipt")
        .expect("present receipt");
    assert_eq!(read, value);
    assert_eq!(
        store.entries.keys().next().unwrap(),
        &vec![
            "installations".to_owned(),
            "0123456789abcdef".to_owned(),
            "receipts".to_owned(),
            "host-prepare-0001.json".to_owned(),
        ]
    );
}

#[test]
fn changed_reuse_of_one_execution_identity_fails_closed() {
    let mut store = MemoryStore::default();
    let first = receipt("ensure-host", "2026-07-26T20:00:01.000Z");
    publish_execution_receipt(&mut store, &installation_id(), &first).expect("publish first");

    for changed in [
        receipt("ensure-other-host", "2026-07-26T20:00:01.000Z"),
        receipt("ensure-host", "2026-07-26T20:00:02.000Z"),
    ] {
        let error = publish_execution_receipt(&mut store, &installation_id(), &changed)
            .expect_err("changed execution identity must conflict");
        assert_eq!(error.kind(), ExecutionReceiptStoreErrorKind::Conflict);
        assert!(!error.message().contains("ensure-other-host"));
    }
    assert_eq!(store.entries.len(), 1);
}

#[test]
fn restart_read_back_requires_exact_canonical_bytes() {
    let value = receipt("ensure-host", "2026-07-26T20:00:01.000Z");
    let record = StateRecord::execution_receipt(&installation_id(), &value).expect("state record");
    let key = MemoryStore::key(record.path());

    let mut restarted = MemoryStore::default();
    restarted
        .entries
        .insert(key.clone(), record.bytes().to_vec());
    assert_eq!(
        read_execution_receipt(&restarted, &installation_id(), value.execution_id())
            .expect("read after restart"),
        Some(value.clone())
    );

    let mut noncanonical = record.bytes().to_vec();
    assert_eq!(noncanonical.pop(), Some(b'\n'));
    restarted.entries.insert(key.clone(), noncanonical);
    let error = read_execution_receipt(&restarted, &installation_id(), value.execution_id())
        .expect_err("noncanonical JSON must fail");
    assert_eq!(error.kind(), ExecutionReceiptStoreErrorKind::CorruptState);

    restarted.entries.insert(key, vec![0xff, 0xfe, 0xfd]);
    let error = read_execution_receipt(&restarted, &installation_id(), value.execution_id())
        .expect_err("invalid UTF-8 must fail");
    assert_eq!(error.kind(), ExecutionReceiptStoreErrorKind::CorruptState);
}

#[test]
fn missing_receipt_is_distinct_from_corrupt_or_conflicting_state() {
    let store = MemoryStore::default();
    assert_eq!(
        read_execution_receipt(&store, &installation_id(), &execution_id())
            .expect("read missing receipt"),
        None
    );
}

#[cfg(target_os = "linux")]
static NEXT_LINUX_ROOT: AtomicU64 = AtomicU64::new(1);

#[cfg(target_os = "linux")]
struct TempLinuxRoot(PathBuf);

#[cfg(target_os = "linux")]
impl TempLinuxRoot {
    fn new() -> Self {
        let sequence = NEXT_LINUX_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-receipt-store-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create Linux state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
            .expect("set Linux state-root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(target_os = "linux")]
impl Drop for TempLinuxRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_state_store_publishes_private_receipt_and_replays_without_replacement() {
    let root = TempLinuxRoot::new();
    let installation = installation_id();
    prepare_installation(root.path(), &installation).expect("prepare receipt state");
    let mut store = LinuxStateRoot::open(root.path()).expect("open Linux state store");
    let value = receipt("ensure-host", "2026-07-26T20:00:01.000Z");

    let created = publish_execution_receipt(&mut store, &installation, &value)
        .expect("publish Linux receipt");
    assert_eq!(
        created.disposition(),
        ExecutionReceiptPublicationDisposition::Created
    );

    let path = root
        .path()
        .join("installations")
        .join(installation.as_str())
        .join("receipts")
        .join("host-prepare-0001.json");
    let metadata = fs::metadata(&path).expect("inspect published receipt");
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
    assert_eq!(
        read_execution_receipt(&store, &installation, value.execution_id())
            .expect("read Linux receipt"),
        Some(value.clone())
    );

    let duplicate =
        publish_execution_receipt(&mut store, &installation, &value).expect("replay Linux receipt");
    assert_eq!(
        duplicate.disposition(),
        ExecutionReceiptPublicationDisposition::Duplicate
    );
    assert_eq!(duplicate.bytes_written(), 0);
    assert_eq!(
        fs::read(&path).expect("read persisted bytes"),
        encode_execution_receipt(&value)
            .expect("encode receipt")
            .into_bytes()
    );
}
