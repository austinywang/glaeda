//! Pure persistent content-store and project materialization planning for Blender snapshots.
//!
//! This module separates cheap reusable content from accelerator lifetime. It derives immutable
//! logical object keys from SHA-256 content identity and maps one exact Blender snapshot onto those
//! objects. It grants zero filesystem, network, provider, storage-publication, lease, billing,
//! execution, deletion, or accelerator authority.

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

use super::blender_snapshot::{
    BlenderContentObject, BlenderProjectFileClass, BlenderProjectSnapshot, BlenderRelativePath,
    BlenderRemoteInventory, BlenderSnapshotError, BlenderTransferPlan,
};

pub const BLENDER_CONTENT_STORE_PLAN_SCHEMA_VERSION: u8 = 1;

const OBJECT_KEY_PREFIX: &str = "objects/v1/sha256/";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BlenderContentObjectKey(String);

impl BlenderContentObjectKey {
    #[must_use]
    pub fn from_digest(digest: &Sha256Digest) -> Self {
        let hex = digest
            .as_str()
            .strip_prefix(SHA256_PREFIX)
            .expect("validated SHA-256 digest must use canonical prefix");
        debug_assert_eq!(hex.len(), 64);
        let value = format!("{OBJECT_KEY_PREFIX}{}/{}", &hex[..2], &hex[2..]);
        Self(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlenderContentStoreDisposition {
    Present,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderContentStoreObjectPlan {
    digest: Sha256Digest,
    bytes: u64,
    object_key: BlenderContentObjectKey,
    disposition: BlenderContentStoreDisposition,
}

impl BlenderContentStoreObjectPlan {
    #[must_use]
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub fn object_key(&self) -> &BlenderContentObjectKey {
        &self.object_key
    }

    #[must_use]
    pub const fn disposition(&self) -> BlenderContentStoreDisposition {
        self.disposition
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderMaterializationEntry {
    relative_path: BlenderRelativePath,
    class: BlenderProjectFileClass,
    digest: Sha256Digest,
    bytes: u64,
    object_key: BlenderContentObjectKey,
}

impl BlenderMaterializationEntry {
    #[must_use]
    pub fn relative_path(&self) -> &BlenderRelativePath {
        &self.relative_path
    }

    #[must_use]
    pub const fn class(&self) -> BlenderProjectFileClass {
        self.class
    }

    #[must_use]
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub fn object_key(&self) -> &BlenderContentObjectKey {
        &self.object_key
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderContentStorePlan {
    schema_version: u8,
    snapshot_digest: Sha256Digest,
    objects: Vec<BlenderContentStoreObjectPlan>,
    materialization_entries: Vec<BlenderMaterializationEntry>,
    required_bytes: u64,
    present_bytes: u64,
    missing_bytes: u64,
    plan_digest: Sha256Digest,
}

impl BlenderContentStorePlan {
    pub fn new(
        snapshot: &BlenderProjectSnapshot,
        remote: &BlenderRemoteInventory,
    ) -> Result<Self, BlenderContentStorePlanError> {
        let transfer = BlenderTransferPlan::new(snapshot, remote).map_err(snapshot_error)?;
        let present = transfer
            .present_objects()
            .iter()
            .map(BlenderContentObject::digest)
            .cloned()
            .collect::<BTreeSet<_>>();

        let objects = transfer
            .required_objects()
            .iter()
            .map(|object| BlenderContentStoreObjectPlan {
                digest: object.digest().clone(),
                bytes: object.bytes(),
                object_key: BlenderContentObjectKey::from_digest(object.digest()),
                disposition: if present.contains(object.digest()) {
                    BlenderContentStoreDisposition::Present
                } else {
                    BlenderContentStoreDisposition::Missing
                },
            })
            .collect::<Vec<_>>();

        let materialization_entries = snapshot
            .files()
            .iter()
            .map(|file| BlenderMaterializationEntry {
                relative_path: file.relative_path().clone(),
                class: file.class(),
                digest: file.digest().clone(),
                bytes: file.bytes(),
                object_key: BlenderContentObjectKey::from_digest(file.digest()),
            })
            .collect::<Vec<_>>();

        let plan_digest = canonical_plan_digest(
            snapshot.snapshot_digest(),
            &objects,
            &materialization_entries,
            transfer.required_bytes(),
            transfer.present_bytes(),
            transfer.missing_bytes(),
        )?;

        Ok(Self {
            schema_version: BLENDER_CONTENT_STORE_PLAN_SCHEMA_VERSION,
            snapshot_digest: snapshot.snapshot_digest().clone(),
            objects,
            materialization_entries,
            required_bytes: transfer.required_bytes(),
            present_bytes: transfer.present_bytes(),
            missing_bytes: transfer.missing_bytes(),
            plan_digest,
        })
    }

    #[must_use]
    pub fn snapshot_digest(&self) -> &Sha256Digest {
        &self.snapshot_digest
    }

    #[must_use]
    pub fn objects(&self) -> &[BlenderContentStoreObjectPlan] {
        &self.objects
    }

    #[must_use]
    pub fn materialization_entries(&self) -> &[BlenderMaterializationEntry] {
        &self.materialization_entries
    }

    #[must_use]
    pub const fn required_bytes(&self) -> u64 {
        self.required_bytes
    }

    #[must_use]
    pub const fn present_bytes(&self) -> u64 {
        self.present_bytes
    }

    #[must_use]
    pub const fn missing_bytes(&self) -> u64 {
        self.missing_bytes
    }

    #[must_use]
    pub fn plan_digest(&self) -> &Sha256Digest {
        &self.plan_digest
    }
}

#[derive(Serialize)]
struct CanonicalPlanDocument<'a> {
    schema_version: u8,
    snapshot_digest: &'a str,
    objects: Vec<CanonicalObject<'a>>,
    materialization_entries: Vec<CanonicalEntry<'a>>,
    required_bytes: u64,
    present_bytes: u64,
    missing_bytes: u64,
}

#[derive(Serialize)]
struct CanonicalObject<'a> {
    digest: &'a str,
    bytes: u64,
    object_key: &'a str,
    disposition: BlenderContentStoreDisposition,
}

#[derive(Serialize)]
struct CanonicalEntry<'a> {
    relative_path: &'a str,
    class: BlenderProjectFileClass,
    digest: &'a str,
    bytes: u64,
    object_key: &'a str,
}

fn canonical_plan_digest(
    snapshot_digest: &Sha256Digest,
    objects: &[BlenderContentStoreObjectPlan],
    entries: &[BlenderMaterializationEntry],
    required_bytes: u64,
    present_bytes: u64,
    missing_bytes: u64,
) -> Result<Sha256Digest, BlenderContentStorePlanError> {
    let document = CanonicalPlanDocument {
        schema_version: BLENDER_CONTENT_STORE_PLAN_SCHEMA_VERSION,
        snapshot_digest: snapshot_digest.as_str(),
        objects: objects
            .iter()
            .map(|object| CanonicalObject {
                digest: object.digest.as_str(),
                bytes: object.bytes,
                object_key: object.object_key.as_str(),
                disposition: object.disposition,
            })
            .collect(),
        materialization_entries: entries
            .iter()
            .map(|entry| CanonicalEntry {
                relative_path: entry.relative_path.as_str(),
                class: entry.class,
                digest: entry.digest.as_str(),
                bytes: entry.bytes,
                object_key: entry.object_key.as_str(),
            })
            .collect(),
        required_bytes,
        present_bytes,
        missing_bytes,
    };
    let bytes = serde_json::to_vec(&document).map_err(|_| {
        BlenderContentStorePlanError::new(
            "blender_content_store_plan_serialization_failed",
            "Blender content-store plan canonicalization failed",
        )
    })?;
    Ok(sha256_digest(&bytes))
}

fn sha256_digest(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Sha256Digest::parse(&value).expect("SHA-256 encoder must produce a canonical digest")
}

fn snapshot_error(error: BlenderSnapshotError) -> BlenderContentStorePlanError {
    BlenderContentStorePlanError::new(
        error.code(),
        "Blender content-store plan failed snapshot/transfer validation",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlenderContentStorePlanError {
    code: &'static str,
    message: &'static str,
}

impl BlenderContentStorePlanError {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for BlenderContentStorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for BlenderContentStorePlanError {}

#[cfg(test)]
mod tests {
    use super::super::blender_snapshot::{
        BlenderProjectFile, BlenderProjectFileClass, BlenderRemoteInventory, BlenderRuntimeId,
    };
    use super::*;

    fn digest(value: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", value.to_string().repeat(64))).unwrap()
    }

    fn file(
        path: &str,
        class: BlenderProjectFileClass,
        digest_char: char,
        bytes: u64,
    ) -> BlenderProjectFile {
        BlenderProjectFile::new(
            BlenderRelativePath::parse(path).unwrap(),
            class,
            digest(digest_char),
            bytes,
        )
        .unwrap()
    }

    fn scene(digest_char: char, bytes: u64) -> BlenderProjectFile {
        file(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            digest_char,
            bytes,
        )
    }

    fn asset(path: &str, digest_char: char, bytes: u64) -> BlenderProjectFile {
        file(path, BlenderProjectFileClass::Asset, digest_char, bytes)
    }

    fn snapshot(files: Vec<BlenderProjectFile>) -> BlenderProjectSnapshot {
        BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-5.2.0").unwrap(),
            BlenderRelativePath::parse("scenes/main.blend").unwrap(),
            files,
        )
        .unwrap()
    }

    fn remote(entries: &[(char, u64)]) -> BlenderRemoteInventory {
        BlenderRemoteInventory::new(
            entries
                .iter()
                .map(|(value, bytes)| BlenderContentObject::new(digest(*value), *bytes).unwrap())
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn object_key_is_canonical_and_contains_no_external_identity() {
        let key = BlenderContentObjectKey::from_digest(&digest('a'));
        assert_eq!(
            key.as_str(),
            format!("objects/v1/sha256/aa/{}", "a".repeat(62))
        );
        for forbidden in ["/home/", "/Users/", "https://", "runpod", "vast"] {
            assert!(!key.as_str().contains(forbidden));
        }
    }

    #[test]
    fn empty_store_reports_unique_objects_and_all_materialization_paths() {
        let project = snapshot(vec![
            scene('a', 100),
            asset("assets/wood-a.exr", 'b', 200),
            asset("assets/wood-b.exr", 'b', 200),
        ]);
        let plan = BlenderContentStorePlan::new(&project, &remote(&[])).unwrap();

        assert_eq!(plan.objects().len(), 2);
        assert_eq!(plan.materialization_entries().len(), 3);
        assert_eq!(plan.required_bytes(), 300);
        assert_eq!(plan.present_bytes(), 0);
        assert_eq!(plan.missing_bytes(), 300);
        assert!(
            plan.objects()
                .iter()
                .all(|object| object.disposition() == BlenderContentStoreDisposition::Missing)
        );

        let wood_entries = plan
            .materialization_entries()
            .iter()
            .filter(|entry| entry.digest() == &digest('b'))
            .collect::<Vec<_>>();
        assert_eq!(wood_entries.len(), 2);
        assert_eq!(wood_entries[0].object_key(), wood_entries[1].object_key());
    }

    #[test]
    fn warm_store_reuses_every_object_without_transfer() {
        let project = snapshot(vec![scene('a', 100), asset("assets/wood.exr", 'b', 200)]);
        let plan =
            BlenderContentStorePlan::new(&project, &remote(&[('a', 100), ('b', 200)])).unwrap();

        assert_eq!(plan.required_bytes(), 300);
        assert_eq!(plan.present_bytes(), 300);
        assert_eq!(plan.missing_bytes(), 0);
        assert!(
            plan.objects()
                .iter()
                .all(|object| object.disposition() == BlenderContentStoreDisposition::Present)
        );
    }

    #[test]
    fn one_scene_edit_marks_only_new_scene_object_missing() {
        let project = snapshot(vec![scene('c', 120), asset("assets/wood.exr", 'b', 200)]);
        let plan =
            BlenderContentStorePlan::new(&project, &remote(&[('a', 100), ('b', 200)])).unwrap();

        assert_eq!(plan.present_bytes(), 200);
        assert_eq!(plan.missing_bytes(), 120);
        let missing = plan
            .objects()
            .iter()
            .filter(|object| object.disposition() == BlenderContentStoreDisposition::Missing)
            .collect::<Vec<_>>();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].digest(), &digest('c'));
    }

    #[test]
    fn plan_digest_and_entries_are_independent_of_snapshot_input_order() {
        let scene = scene('a', 100);
        let asset = asset("assets/wood.exr", 'b', 200);
        let first = BlenderContentStorePlan::new(
            &snapshot(vec![scene.clone(), asset.clone()]),
            &remote(&[('b', 200)]),
        )
        .unwrap();
        let second =
            BlenderContentStorePlan::new(&snapshot(vec![asset, scene]), &remote(&[('b', 200)]))
                .unwrap();

        assert_eq!(first.plan_digest(), second.plan_digest());
        assert_eq!(
            first.materialization_entries(),
            second.materialization_entries()
        );
        assert_eq!(first.objects(), second.objects());
    }

    #[test]
    fn store_plan_transfer_totals_match_existing_transfer_contract() {
        let project = snapshot(vec![scene('a', 100), asset("assets/wood.exr", 'b', 200)]);
        let inventory = remote(&[('b', 200)]);
        let transfer = BlenderTransferPlan::new(&project, &inventory).unwrap();
        let store = BlenderContentStorePlan::new(&project, &inventory).unwrap();

        assert_eq!(store.required_bytes(), transfer.required_bytes());
        assert_eq!(store.present_bytes(), transfer.present_bytes());
        assert_eq!(store.missing_bytes(), transfer.missing_bytes());
    }

    #[test]
    fn public_json_contains_only_content_and_portable_project_identity() {
        let project = snapshot(vec![scene('a', 100), asset("assets/wood.exr", 'b', 200)]);
        let plan = BlenderContentStorePlan::new(&project, &remote(&[])).unwrap();
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("objects/v1/sha256/"));
        assert!(json.contains("scenes/main.blend"));
        for forbidden in ["/home/", "/Users/", "https://", "runpod", "vast.ai"] {
            assert!(!json.contains(forbidden));
        }
    }
}
