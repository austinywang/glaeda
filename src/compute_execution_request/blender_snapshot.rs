//! Pure Blender project execution snapshots and content-addressed transfer planning.
//!
//! The snapshot is portable execution identity only. It contains project-relative logical paths,
//! content digests, file classes, and byte lengths. It grants zero host-path, provider-path,
//! credential, upload, execution, lease, billing, or mutation authority.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

pub const BLENDER_EXECUTION_SNAPSHOT_SCHEMA_VERSION: u8 = 1;

const MAX_PROJECT_FILES: usize = 4096;
const MAX_REMOTE_OBJECTS: usize = 8192;
const MAX_RELATIVE_PATH_BYTES: usize = 512;
const MAX_RUNTIME_ID_BYTES: usize = 96;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024 * 1024 * 1024;
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BlenderRuntimeId(String);

impl BlenderRuntimeId {
    pub fn parse(value: &str) -> Result<Self, BlenderSnapshotError> {
        let bytes = value.as_bytes();
        let valid_edges = bytes
            .first()
            .zip(bytes.last())
            .is_some_and(|(first, last)| {
                first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
            });
        let valid_body = bytes
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if value.is_empty() || value.len() > MAX_RUNTIME_ID_BYTES || !valid_edges || !valid_body {
            return Err(BlenderSnapshotError::new(
                "runtime_id",
                "invalid_blender_runtime_id",
                "Blender runtime id must be bounded ASCII using letters, digits, '.', '_', or '-' with alphanumeric edges",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BlenderRelativePath(String);

impl BlenderRelativePath {
    pub fn parse(value: &str) -> Result<Self, BlenderSnapshotError> {
        if value.is_empty()
            || value.len() > MAX_RELATIVE_PATH_BYTES
            || value.starts_with('/')
            || value.starts_with('~')
            || value.contains('\\')
            || value.contains(':')
            || value.chars().any(|character| {
                character.is_control() || matches!(character, '*' | '?' | '<' | '>' | '|' | '"')
            })
            || value
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            return Err(BlenderSnapshotError::new(
                "relative_path",
                "invalid_blender_relative_path",
                "Blender project paths must be bounded portable relative paths without host or traversal syntax",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlenderProjectFileClass {
    Scene,
    LinkedLibrary,
    Asset,
    BakedCache,
    Script,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderProjectFile {
    relative_path: BlenderRelativePath,
    class: BlenderProjectFileClass,
    digest: Sha256Digest,
    bytes: u64,
}

impl BlenderProjectFile {
    pub fn new(
        relative_path: BlenderRelativePath,
        class: BlenderProjectFileClass,
        digest: Sha256Digest,
        bytes: u64,
    ) -> Result<Self, BlenderSnapshotError> {
        validate_object_size(bytes)?;
        Ok(Self {
            relative_path,
            class,
            digest,
            bytes,
        })
    }

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderProjectSnapshot {
    schema_version: u8,
    runtime_id: BlenderRuntimeId,
    main_scene: BlenderRelativePath,
    files: Vec<BlenderProjectFile>,
    snapshot_digest: Sha256Digest,
}

impl BlenderProjectSnapshot {
    pub fn new(
        runtime_id: BlenderRuntimeId,
        main_scene: BlenderRelativePath,
        mut files: Vec<BlenderProjectFile>,
    ) -> Result<Self, BlenderSnapshotError> {
        if files.is_empty() || files.len() > MAX_PROJECT_FILES {
            return Err(BlenderSnapshotError::new(
                "files",
                "invalid_blender_file_count",
                "Blender snapshot must contain one to 4096 project files",
            ));
        }

        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        if files
            .windows(2)
            .any(|pair| pair[0].relative_path == pair[1].relative_path)
        {
            return Err(BlenderSnapshotError::new(
                "files",
                "duplicate_blender_relative_path",
                "Blender snapshot cannot contain duplicate logical paths",
            ));
        }

        let main = files
            .iter()
            .find(|file| file.relative_path == main_scene)
            .ok_or_else(|| {
                BlenderSnapshotError::new(
                    "main_scene",
                    "blender_main_scene_missing",
                    "Blender main scene must exist in the project manifest",
                )
            })?;
        if main.class != BlenderProjectFileClass::Scene {
            return Err(BlenderSnapshotError::new(
                "main_scene",
                "blender_main_scene_wrong_class",
                "Blender main scene must be classed as a scene",
            ));
        }
        if !main_scene.as_str().to_ascii_lowercase().ends_with(".blend") {
            return Err(BlenderSnapshotError::new(
                "main_scene",
                "blender_main_scene_wrong_extension",
                "Blender main scene must use a .blend logical path",
            ));
        }

        validate_digest_sizes(files.iter().map(|file| (&file.digest, file.bytes)))?;
        let snapshot_digest = canonical_snapshot_digest(&runtime_id, &main_scene, &files)?;
        Ok(Self {
            schema_version: BLENDER_EXECUTION_SNAPSHOT_SCHEMA_VERSION,
            runtime_id,
            main_scene,
            files,
            snapshot_digest,
        })
    }

    #[must_use]
    pub fn runtime_id(&self) -> &BlenderRuntimeId {
        &self.runtime_id
    }

    #[must_use]
    pub fn main_scene(&self) -> &BlenderRelativePath {
        &self.main_scene
    }

    #[must_use]
    pub fn files(&self) -> &[BlenderProjectFile] {
        &self.files
    }

    #[must_use]
    pub fn snapshot_digest(&self) -> &Sha256Digest {
        &self.snapshot_digest
    }
}

#[derive(Serialize)]
struct CanonicalSnapshotDocument<'a> {
    schema_version: u8,
    runtime_id: &'a str,
    main_scene: &'a str,
    files: Vec<CanonicalSnapshotFile<'a>>,
}

#[derive(Serialize)]
struct CanonicalSnapshotFile<'a> {
    relative_path: &'a str,
    class: BlenderProjectFileClass,
    digest: &'a str,
    bytes: u64,
}

fn canonical_snapshot_digest(
    runtime_id: &BlenderRuntimeId,
    main_scene: &BlenderRelativePath,
    files: &[BlenderProjectFile],
) -> Result<Sha256Digest, BlenderSnapshotError> {
    let document = CanonicalSnapshotDocument {
        schema_version: BLENDER_EXECUTION_SNAPSHOT_SCHEMA_VERSION,
        runtime_id: runtime_id.as_str(),
        main_scene: main_scene.as_str(),
        files: files
            .iter()
            .map(|file| CanonicalSnapshotFile {
                relative_path: file.relative_path.as_str(),
                class: file.class,
                digest: file.digest.as_str(),
                bytes: file.bytes,
            })
            .collect(),
    };
    let encoded = serde_json::to_vec(&document).map_err(|_| {
        BlenderSnapshotError::new(
            "snapshot",
            "blender_snapshot_serialization_failed",
            "Blender snapshot canonicalization failed",
        )
    })?;
    Ok(sha256_digest(&encoded))
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct BlenderContentObject {
    digest: Sha256Digest,
    bytes: u64,
}

impl BlenderContentObject {
    pub fn new(digest: Sha256Digest, bytes: u64) -> Result<Self, BlenderSnapshotError> {
        validate_object_size(bytes)?;
        Ok(Self { digest, bytes })
    }

    #[must_use]
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderRemoteInventory {
    schema_version: u8,
    objects: Vec<BlenderContentObject>,
}

impl BlenderRemoteInventory {
    pub fn new(objects: Vec<BlenderContentObject>) -> Result<Self, BlenderSnapshotError> {
        if objects.len() > MAX_REMOTE_OBJECTS {
            return Err(BlenderSnapshotError::new(
                "objects",
                "too_many_blender_remote_objects",
                "Blender remote inventory exceeds the bounded object count",
            ));
        }

        let mut canonical = BTreeMap::<Sha256Digest, u64>::new();
        for BlenderContentObject { digest, bytes } in objects {
            insert_digest_size(&mut canonical, digest, bytes, "objects")?;
        }
        let objects = canonical
            .into_iter()
            .map(|(digest, bytes)| BlenderContentObject { digest, bytes })
            .collect();
        Ok(Self {
            schema_version: BLENDER_EXECUTION_SNAPSHOT_SCHEMA_VERSION,
            objects,
        })
    }

    #[must_use]
    pub fn objects(&self) -> &[BlenderContentObject] {
        &self.objects
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderTransferPlan {
    schema_version: u8,
    snapshot_digest: Sha256Digest,
    required_objects: Vec<BlenderContentObject>,
    present_objects: Vec<BlenderContentObject>,
    missing_objects: Vec<BlenderContentObject>,
    required_bytes: u64,
    present_bytes: u64,
    missing_bytes: u64,
}

impl BlenderTransferPlan {
    pub fn new(
        snapshot: &BlenderProjectSnapshot,
        remote: &BlenderRemoteInventory,
    ) -> Result<Self, BlenderSnapshotError> {
        let mut required = BTreeMap::<Sha256Digest, u64>::new();
        for file in snapshot.files() {
            insert_digest_size(&mut required, file.digest().clone(), file.bytes(), "files")?;
        }
        let remote = remote
            .objects()
            .iter()
            .map(|object| (object.digest().clone(), object.bytes()))
            .collect::<BTreeMap<_, _>>();

        let mut required_objects = Vec::with_capacity(required.len());
        let mut present_objects = Vec::new();
        let mut missing_objects = Vec::new();
        let mut required_bytes = 0_u64;
        let mut present_bytes = 0_u64;
        let mut missing_bytes = 0_u64;

        for (digest, bytes) in required {
            required_bytes = checked_total(required_bytes, bytes)?;
            let object = BlenderContentObject {
                digest: digest.clone(),
                bytes,
            };
            required_objects.push(object.clone());
            match remote.get(&digest) {
                Some(remote_bytes) if *remote_bytes != bytes => {
                    return Err(BlenderSnapshotError::new(
                        "remote_inventory",
                        "inconsistent_blender_remote_size",
                        "remote Blender inventory conflicts with the required content byte length",
                    ));
                }
                Some(_) => {
                    present_bytes = checked_total(present_bytes, bytes)?;
                    present_objects.push(object);
                }
                None => {
                    missing_bytes = checked_total(missing_bytes, bytes)?;
                    missing_objects.push(object);
                }
            }
        }

        Ok(Self {
            schema_version: BLENDER_EXECUTION_SNAPSHOT_SCHEMA_VERSION,
            snapshot_digest: snapshot.snapshot_digest().clone(),
            required_objects,
            present_objects,
            missing_objects,
            required_bytes,
            present_bytes,
            missing_bytes,
        })
    }

    #[must_use]
    pub fn snapshot_digest(&self) -> &Sha256Digest {
        &self.snapshot_digest
    }

    #[must_use]
    pub fn required_objects(&self) -> &[BlenderContentObject] {
        &self.required_objects
    }

    #[must_use]
    pub fn present_objects(&self) -> &[BlenderContentObject] {
        &self.present_objects
    }

    #[must_use]
    pub fn missing_objects(&self) -> &[BlenderContentObject] {
        &self.missing_objects
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
}

fn validate_object_size(bytes: u64) -> Result<(), BlenderSnapshotError> {
    if bytes > MAX_FILE_BYTES {
        return Err(BlenderSnapshotError::new(
            "bytes",
            "blender_content_object_too_large",
            "Blender content object exceeds the bounded byte limit",
        ));
    }
    Ok(())
}

fn insert_digest_size(
    sizes: &mut BTreeMap<Sha256Digest, u64>,
    digest: Sha256Digest,
    bytes: u64,
    field: &'static str,
) -> Result<(), BlenderSnapshotError> {
    match sizes.get(&digest) {
        Some(existing) if *existing != bytes => Err(BlenderSnapshotError::new(
            field,
            "inconsistent_blender_digest_size",
            "the same Blender content digest cannot have conflicting byte lengths",
        )),
        Some(_) => Ok(()),
        None => {
            sizes.insert(digest, bytes);
            Ok(())
        }
    }
}

fn validate_digest_sizes<'a>(
    objects: impl Iterator<Item = (&'a Sha256Digest, u64)>,
) -> Result<(), BlenderSnapshotError> {
    let mut sizes = BTreeMap::<Sha256Digest, u64>::new();
    for (digest, bytes) in objects {
        insert_digest_size(&mut sizes, digest.clone(), bytes, "files")?;
    }
    Ok(())
}

fn checked_total(current: u64, value: u64) -> Result<u64, BlenderSnapshotError> {
    current.checked_add(value).ok_or_else(|| {
        BlenderSnapshotError::new(
            "bytes",
            "blender_byte_total_overflow",
            "Blender byte total overflowed the bounded integer representation",
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BlenderSnapshotError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl BlenderSnapshotError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for BlenderSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for BlenderSnapshotError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn path(value: &str) -> BlenderRelativePath {
        BlenderRelativePath::parse(value).unwrap()
    }

    fn file(
        relative_path: &str,
        class: BlenderProjectFileClass,
        digest_char: char,
        bytes: u64,
    ) -> BlenderProjectFile {
        BlenderProjectFile::new(path(relative_path), class, digest(digest_char), bytes).unwrap()
    }

    fn scene(digest_char: char, bytes: u64) -> BlenderProjectFile {
        file(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            digest_char,
            bytes,
        )
    }

    fn snapshot(files: Vec<BlenderProjectFile>) -> BlenderProjectSnapshot {
        BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-4.5.3").unwrap(),
            path("scenes/main.blend"),
            files,
        )
        .unwrap()
    }

    #[test]
    fn portable_paths_refuse_host_and_escape_shapes() {
        for invalid in [
            "",
            "/tmp/main.blend",
            "../main.blend",
            "scenes/../main.blend",
            "scenes//main.blend",
            "C:/main.blend",
            "https://example.invalid/main.blend",
            "~/main.blend",
            "scenes\\main.blend",
        ] {
            assert_eq!(
                BlenderRelativePath::parse(invalid).unwrap_err().code(),
                "invalid_blender_relative_path"
            );
        }
        assert_eq!(
            BlenderRelativePath::parse("资产/角色/main.blend")
                .unwrap()
                .as_str(),
            "资产/角色/main.blend"
        );
    }

    #[test]
    fn snapshot_digest_is_independent_of_input_order() {
        let scene = scene('a', 100);
        let texture = file("assets/wood.exr", BlenderProjectFileClass::Asset, 'b', 200);
        let first = snapshot(vec![scene.clone(), texture.clone()]);
        let second = snapshot(vec![texture, scene]);
        assert_eq!(first.snapshot_digest(), second.snapshot_digest());
        assert_eq!(first.files()[0].relative_path().as_str(), "assets/wood.exr");
    }

    #[test]
    fn main_scene_must_be_present_and_classed_as_scene() {
        let wrong = BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-4.5.3").unwrap(),
            path("scenes/main.blend"),
            vec![file(
                "scenes/main.blend",
                BlenderProjectFileClass::LinkedLibrary,
                'a',
                100,
            )],
        )
        .unwrap_err();
        assert_eq!(wrong.code(), "blender_main_scene_wrong_class");

        let missing = BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-4.5.3").unwrap(),
            path("scenes/main.blend"),
            vec![file(
                "scenes/other.blend",
                BlenderProjectFileClass::Scene,
                'a',
                100,
            )],
        )
        .unwrap_err();
        assert_eq!(missing.code(), "blender_main_scene_missing");
    }

    #[test]
    fn duplicate_paths_and_inconsistent_digest_sizes_refuse() {
        let duplicate_path = BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-4.5.3").unwrap(),
            path("scenes/main.blend"),
            vec![scene('a', 100), scene('b', 100)],
        )
        .unwrap_err();
        assert_eq!(duplicate_path.code(), "duplicate_blender_relative_path");

        let inconsistent = BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-4.5.3").unwrap(),
            path("scenes/main.blend"),
            vec![
                scene('a', 100),
                file("assets/copy.bin", BlenderProjectFileClass::Asset, 'a', 101),
            ],
        )
        .unwrap_err();
        assert_eq!(inconsistent.code(), "inconsistent_blender_digest_size");
    }

    #[test]
    fn empty_remote_inventory_requires_each_unique_content_object_once() {
        let project = snapshot(vec![
            scene('a', 100),
            file(
                "assets/wood-a.exr",
                BlenderProjectFileClass::Asset,
                'b',
                200,
            ),
            file(
                "assets/wood-b.exr",
                BlenderProjectFileClass::Asset,
                'b',
                200,
            ),
        ]);
        let plan =
            BlenderTransferPlan::new(&project, &BlenderRemoteInventory::new(vec![]).unwrap())
                .unwrap();
        assert_eq!(plan.required_objects().len(), 2);
        assert_eq!(plan.missing_objects().len(), 2);
        assert_eq!(plan.present_objects().len(), 0);
        assert_eq!(plan.required_bytes(), 300);
        assert_eq!(plan.missing_bytes(), 300);
    }

    #[test]
    fn fully_warm_inventory_requires_zero_transfer() {
        let project = snapshot(vec![
            scene('a', 100),
            file("assets/wood.exr", BlenderProjectFileClass::Asset, 'b', 200),
        ]);
        let remote = BlenderRemoteInventory::new(vec![
            BlenderContentObject::new(digest('a'), 100).unwrap(),
            BlenderContentObject::new(digest('b'), 200).unwrap(),
        ])
        .unwrap();
        let plan = BlenderTransferPlan::new(&project, &remote).unwrap();
        assert!(plan.missing_objects().is_empty());
        assert_eq!(plan.present_objects().len(), 2);
        assert_eq!(plan.present_bytes(), 300);
        assert_eq!(plan.missing_bytes(), 0);
    }

    #[test]
    fn one_scene_edit_transfers_only_the_new_scene_object() {
        let old = snapshot(vec![
            scene('a', 100),
            file("assets/wood.exr", BlenderProjectFileClass::Asset, 'b', 200),
        ]);
        let remote = BlenderRemoteInventory::new(
            old.files()
                .iter()
                .map(|file| BlenderContentObject::new(file.digest().clone(), file.bytes()).unwrap())
                .collect(),
        )
        .unwrap();
        let edited = snapshot(vec![
            scene('c', 120),
            file("assets/wood.exr", BlenderProjectFileClass::Asset, 'b', 200),
        ]);
        let plan = BlenderTransferPlan::new(&edited, &remote).unwrap();
        assert_eq!(plan.missing_objects().len(), 1);
        assert_eq!(
            plan.missing_objects()[0].digest().as_str(),
            digest('c').as_str()
        );
        assert_eq!(plan.missing_bytes(), 120);
        assert_eq!(plan.present_bytes(), 200);
    }

    #[test]
    fn remote_inventory_refuses_conflicting_size_for_same_digest() {
        let error = BlenderRemoteInventory::new(vec![
            BlenderContentObject::new(digest('a'), 100).unwrap(),
            BlenderContentObject::new(digest('a'), 101).unwrap(),
        ])
        .unwrap_err();
        assert_eq!(error.code(), "inconsistent_blender_digest_size");
    }

    #[test]
    fn public_json_contains_only_portable_project_identity() {
        let project = snapshot(vec![
            scene('a', 100),
            file("assets/wood.exr", BlenderProjectFileClass::Asset, 'b', 200),
        ]);
        let json = serde_json::to_string(&project).unwrap();
        assert!(json.contains("scenes/main.blend"));
        for forbidden in [
            "/Users/", "/home/", "C:\\\\", "https://", "runpod", "vast.ai",
        ] {
            assert!(!json.contains(forbidden));
        }
    }
}
