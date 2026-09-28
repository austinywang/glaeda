//! Strict provider-neutral decoder for one bounded Blender remote content inventory.
//!
//! The document only states which immutable content digests and byte lengths are already present.
//! It carries no provider, path, credential, storage-publication, upload, lease, or execution
//! authority.

use std::fmt;

use serde::Deserialize;

use crate::artifact::Sha256Digest;

use super::blender_snapshot::{BlenderContentObject, BlenderRemoteInventory, BlenderSnapshotError};

pub const BLENDER_REMOTE_INVENTORY_SCHEMA_VERSION: u8 = 1;
pub const MAX_BLENDER_REMOTE_INVENTORY_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlenderRemoteInventoryDocument {
    schema_version: u8,
    objects: Vec<BlenderRemoteInventoryDocumentObject>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlenderRemoteInventoryDocumentObject {
    digest: String,
    bytes: u64,
}

/// Decode one provider-neutral inventory document and re-run the typed inventory validators.
///
/// # Errors
///
/// Refuses malformed JSON, unknown fields, unsupported schema versions, invalid content digests,
/// oversized content objects, excessive object counts, or conflicting sizes for one digest.
pub fn decode_blender_remote_inventory_document(
    bytes: &[u8],
) -> Result<BlenderRemoteInventory, BlenderRemoteInventoryDocumentError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BLENDER_REMOTE_INVENTORY_DOCUMENT_BYTES
    {
        return Err(BlenderRemoteInventoryDocumentError::new(
            "blender_remote_inventory_document_too_large",
            "Blender remote inventory document exceeds the bounded maximum",
        ));
    }
    let document: BlenderRemoteInventoryDocument = serde_json::from_slice(bytes).map_err(|_| {
        BlenderRemoteInventoryDocumentError::new(
            "invalid_blender_remote_inventory_document",
            "Blender remote inventory document is malformed or contains unsupported fields",
        )
    })?;
    if document.schema_version != BLENDER_REMOTE_INVENTORY_SCHEMA_VERSION {
        return Err(BlenderRemoteInventoryDocumentError::new(
            "unsupported_blender_remote_inventory_schema",
            "Blender remote inventory schema version is unsupported",
        ));
    }

    let mut objects = Vec::with_capacity(document.objects.len());
    for object in document.objects {
        let digest = Sha256Digest::parse(&object.digest).map_err(|_| {
            BlenderRemoteInventoryDocumentError::new(
                "invalid_blender_remote_content_digest",
                "Blender remote inventory content digest is invalid",
            )
        })?;
        objects.push(BlenderContentObject::new(digest, object.bytes).map_err(validation_error)?);
    }
    BlenderRemoteInventory::new(objects).map_err(validation_error)
}

fn validation_error(error: BlenderSnapshotError) -> BlenderRemoteInventoryDocumentError {
    BlenderRemoteInventoryDocumentError::new(
        error.code(),
        "Blender remote inventory failed typed validation",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlenderRemoteInventoryDocumentError {
    code: &'static str,
    message: &'static str,
}

impl BlenderRemoteInventoryDocumentError {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for BlenderRemoteInventoryDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for BlenderRemoteInventoryDocumentError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_populated_inventories_decode() {
        let empty =
            decode_blender_remote_inventory_document(br#"{"schema_version":1,"objects":[]}"#)
                .unwrap();
        assert!(empty.objects().is_empty());

        let populated = decode_blender_remote_inventory_document(
            br#"{
              "schema_version": 1,
              "objects": [{
                "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "bytes": 123
              }]
            }"#,
        )
        .unwrap();
        assert_eq!(populated.objects().len(), 1);
        assert_eq!(populated.objects()[0].bytes(), 123);
    }

    #[test]
    fn unknown_fields_and_wrong_schema_refuse() {
        let unknown = br#"{"schema_version":1,"objects":[],"provider":"runpod"}"#;
        assert_eq!(
            decode_blender_remote_inventory_document(unknown)
                .unwrap_err()
                .code(),
            "invalid_blender_remote_inventory_document"
        );

        let wrong_schema = br#"{"schema_version":2,"objects":[]}"#;
        assert_eq!(
            decode_blender_remote_inventory_document(wrong_schema)
                .unwrap_err()
                .code(),
            "unsupported_blender_remote_inventory_schema"
        );
    }

    #[test]
    fn invalid_digest_and_conflicting_size_refuse() {
        let invalid = br#"{
          "schema_version": 1,
          "objects": [{"digest":"nope","bytes":1}]
        }"#;
        assert_eq!(
            decode_blender_remote_inventory_document(invalid)
                .unwrap_err()
                .code(),
            "invalid_blender_remote_content_digest"
        );

        let conflict = br#"{
          "schema_version": 1,
          "objects": [
            {
              "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "bytes": 1
            },
            {
              "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "bytes": 2
            }
          ]
        }"#;
        assert_eq!(
            decode_blender_remote_inventory_document(conflict)
                .unwrap_err()
                .code(),
            "inconsistent_blender_digest_size"
        );
    }

    #[test]
    fn oversized_document_refuses_before_json_decode() {
        let oversized = vec![b' '; (MAX_BLENDER_REMOTE_INVENTORY_DOCUMENT_BYTES + 1) as usize];
        assert_eq!(
            decode_blender_remote_inventory_document(&oversized)
                .unwrap_err()
                .code(),
            "blender_remote_inventory_document_too_large"
        );
    }
}
