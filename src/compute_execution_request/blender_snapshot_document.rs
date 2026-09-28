//! Strict decoder from the local Blender exporter document into the typed snapshot contract.
//!
//! The exporter document is an intermediate transport between Blender's Python runtime and the
//! provider-neutral Rust planner. Decoding grants no file, network, provider, lease, execution, or
//! billing authority.

use std::fmt;

use serde::Deserialize;

use crate::artifact::Sha256Digest;

use super::blender_snapshot::{
    BLENDER_EXECUTION_SNAPSHOT_SCHEMA_VERSION, BlenderProjectFile, BlenderProjectFileClass,
    BlenderProjectSnapshot, BlenderRelativePath, BlenderRuntimeId, BlenderSnapshotError,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlenderSnapshotDocument {
    schema_version: u8,
    runtime_id: String,
    main_scene: String,
    files: Vec<BlenderSnapshotDocumentFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlenderSnapshotDocumentFile {
    relative_path: String,
    class: BlenderSnapshotDocumentFileClass,
    digest: String,
    bytes: u64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BlenderSnapshotDocumentFileClass {
    Scene,
    LinkedLibrary,
    Asset,
    BakedCache,
    Script,
}

impl From<BlenderSnapshotDocumentFileClass> for BlenderProjectFileClass {
    fn from(value: BlenderSnapshotDocumentFileClass) -> Self {
        match value {
            BlenderSnapshotDocumentFileClass::Scene => Self::Scene,
            BlenderSnapshotDocumentFileClass::LinkedLibrary => Self::LinkedLibrary,
            BlenderSnapshotDocumentFileClass::Asset => Self::Asset,
            BlenderSnapshotDocumentFileClass::BakedCache => Self::BakedCache,
            BlenderSnapshotDocumentFileClass::Script => Self::Script,
        }
    }
}

/// Decode one bounded exporter document and re-run the typed snapshot validators.
///
/// # Errors
///
/// Refuses malformed JSON, unknown fields, unsupported schema versions, invalid runtime/path/digest
/// identities, oversized objects, duplicate logical paths, or invalid main-scene semantics.
pub fn decode_blender_snapshot_document(
    bytes: &[u8],
) -> Result<BlenderProjectSnapshot, BlenderSnapshotDocumentError> {
    let document: BlenderSnapshotDocument = serde_json::from_slice(bytes).map_err(|_| {
        BlenderSnapshotDocumentError::new(
            "invalid_blender_snapshot_document",
            "Blender snapshot exporter document is malformed or contains unsupported fields",
        )
    })?;
    if document.schema_version != BLENDER_EXECUTION_SNAPSHOT_SCHEMA_VERSION {
        return Err(BlenderSnapshotDocumentError::new(
            "unsupported_blender_snapshot_schema",
            "Blender snapshot exporter schema version is unsupported",
        ));
    }

    let runtime_id = BlenderRuntimeId::parse(&document.runtime_id).map_err(validation_error)?;
    let main_scene = BlenderRelativePath::parse(&document.main_scene).map_err(validation_error)?;
    let mut files = Vec::with_capacity(document.files.len());
    for file in document.files {
        let relative_path =
            BlenderRelativePath::parse(&file.relative_path).map_err(validation_error)?;
        let digest = Sha256Digest::parse(&file.digest).map_err(|_| {
            BlenderSnapshotDocumentError::new(
                "invalid_blender_content_digest",
                "Blender snapshot content digest is invalid",
            )
        })?;
        files.push(
            BlenderProjectFile::new(relative_path, file.class.into(), digest, file.bytes)
                .map_err(validation_error)?,
        );
    }

    BlenderProjectSnapshot::new(runtime_id, main_scene, files).map_err(validation_error)
}

fn validation_error(error: BlenderSnapshotError) -> BlenderSnapshotDocumentError {
    BlenderSnapshotDocumentError::new(
        error.code(),
        "Blender snapshot exporter document failed typed snapshot validation",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlenderSnapshotDocumentError {
    code: &'static str,
    message: &'static str,
}

impl BlenderSnapshotDocumentError {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for BlenderSnapshotDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for BlenderSnapshotDocumentError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_document() -> Vec<u8> {
        br#"{
          "schema_version": 1,
          "runtime_id": "blender-5.2.0",
          "main_scene": "scenes/main.blend",
          "files": [
            {
              "relative_path": "textures/wood.exr",
              "class": "asset",
              "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
              "bytes": 200
            },
            {
              "relative_path": "scenes/main.blend",
              "class": "scene",
              "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "bytes": 100
            }
          ]
        }"#
        .to_vec()
    }

    #[test]
    fn exporter_shape_decodes_into_canonical_typed_snapshot() {
        let snapshot = decode_blender_snapshot_document(&valid_document()).unwrap();
        assert_eq!(snapshot.runtime_id().as_str(), "blender-5.2.0");
        assert_eq!(snapshot.main_scene().as_str(), "scenes/main.blend");
        assert_eq!(snapshot.files().len(), 2);
        assert_eq!(
            snapshot.files()[0].relative_path().as_str(),
            "scenes/main.blend"
        );
        assert_eq!(
            snapshot.files()[1].relative_path().as_str(),
            "textures/wood.exr"
        );
        assert!(snapshot.snapshot_digest().as_str().starts_with("sha256:"));
    }

    #[test]
    fn unknown_fields_and_wrong_schema_refuse() {
        let unknown = br#"{
          "schema_version": 1,
          "runtime_id": "blender-5.2.0",
          "main_scene": "scenes/main.blend",
          "files": [],
          "provider": "runpod"
        }"#;
        assert_eq!(
            decode_blender_snapshot_document(unknown)
                .unwrap_err()
                .code(),
            "invalid_blender_snapshot_document"
        );

        let wrong_schema = br#"{
          "schema_version": 2,
          "runtime_id": "blender-5.2.0",
          "main_scene": "scenes/main.blend",
          "files": []
        }"#;
        assert_eq!(
            decode_blender_snapshot_document(wrong_schema)
                .unwrap_err()
                .code(),
            "unsupported_blender_snapshot_schema"
        );
    }

    #[test]
    fn invalid_digest_and_path_preserve_bounded_refusal_class() {
        let invalid_digest = br#"{
          "schema_version": 1,
          "runtime_id": "blender-5.2.0",
          "main_scene": "scenes/main.blend",
          "files": [{
            "relative_path": "scenes/main.blend",
            "class": "scene",
            "digest": "nope",
            "bytes": 100
          }]
        }"#;
        assert_eq!(
            decode_blender_snapshot_document(invalid_digest)
                .unwrap_err()
                .code(),
            "invalid_blender_content_digest"
        );

        let invalid_path = br#"{
          "schema_version": 1,
          "runtime_id": "blender-5.2.0",
          "main_scene": "../main.blend",
          "files": [{
            "relative_path": "../main.blend",
            "class": "scene",
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bytes": 100
          }]
        }"#;
        assert_eq!(
            decode_blender_snapshot_document(invalid_path)
                .unwrap_err()
                .code(),
            "invalid_blender_relative_path"
        );
    }
}
