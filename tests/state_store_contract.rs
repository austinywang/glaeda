use glaeda::state::{InstallationId, StateLayout, StatePath};
use glaeda::state_store::{
    StateRead, StateRecord, StateStore, StateStoreError, StateStoreErrorKind,
    StateWriteDisposition, StateWriteReceipt,
};

struct ExternalStore;

impl StateStore for ExternalStore {
    fn read(&self, _path: &StatePath) -> Result<StateRead, StateStoreError> {
        Err(StateStoreError::public(
            StateStoreErrorKind::Busy,
            "state store is busy",
        ))
    }

    fn create_atomic(
        &mut self,
        record: &StateRecord,
    ) -> Result<StateWriteReceipt, StateStoreError> {
        Ok(StateWriteReceipt::new(
            StateWriteDisposition::Created,
            record.bytes().len(),
        ))
    }

    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError> {
        Ok(StateWriteReceipt::new(
            StateWriteDisposition::Created,
            record.bytes().len(),
        ))
    }
}

#[test]
fn external_store_implementations_can_return_bounded_public_results() {
    let installation_id = InstallationId::parse("0123456789abcdef").expect("valid installation ID");
    let path = StateLayout::installation(&installation_id);
    let store = ExternalStore;
    let error = store
        .read(&path)
        .expect_err("test store reports a bounded busy error");

    assert_eq!(error.kind(), StateStoreErrorKind::Busy);
    assert_eq!(error.message(), "state store is busy");

    let receipt = StateWriteReceipt::new(StateWriteDisposition::Created, 42);
    assert_eq!(receipt.disposition(), StateWriteDisposition::Created);
    assert_eq!(receipt.bytes_written(), 42);
}
