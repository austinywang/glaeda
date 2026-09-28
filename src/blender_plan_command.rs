use std::fmt;
use std::io::Read as _;
use std::path::Path;

use glaeda::compute_execution_request::accelerator_burst::{
    AcceleratorApi, AcceleratorIntent, AcceleratorRequirement, AcceleratorVendor,
};
use glaeda::compute_execution_request::blender_burst_work_plan::{
    BlenderBurstWorkPlan, BlenderBurstWorkPlanError,
};
use glaeda::compute_execution_request::blender_remote_inventory_document::{
    BlenderRemoteInventoryDocumentError, MAX_BLENDER_REMOTE_INVENTORY_DOCUMENT_BYTES,
    decode_blender_remote_inventory_document,
};
use glaeda::compute_execution_request::blender_snapshot_document::{
    BlenderSnapshotDocumentError, decode_blender_snapshot_document,
};

const GIB: u64 = 1024 * 1024 * 1024;
pub const MAX_BLENDER_SNAPSHOT_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;

pub fn build_blender_plan(
    snapshot_path: &Path,
    inventory_path: &Path,
    minimum_vram_gib: u64,
    intent: AcceleratorIntent,
    maximum_rtt_ms: Option<u32>,
) -> Result<BlenderBurstWorkPlan, BlenderPlanCommandError> {
    let snapshot_bytes = read_bounded_document(
        snapshot_path,
        MAX_BLENDER_SNAPSHOT_DOCUMENT_BYTES,
        "blender_snapshot_read_failed",
        "Blender snapshot document could not be read within its bounded maximum",
    )?;
    let snapshot = decode_blender_snapshot_document(&snapshot_bytes).map_err(snapshot_error)?;

    let inventory_bytes = read_bounded_document(
        inventory_path,
        MAX_BLENDER_REMOTE_INVENTORY_DOCUMENT_BYTES,
        "blender_remote_inventory_read_failed",
        "Blender remote inventory document could not be read within its bounded maximum",
    )?;
    let inventory =
        decode_blender_remote_inventory_document(&inventory_bytes).map_err(inventory_error)?;

    let minimum_memory_bytes = minimum_vram_gib.checked_mul(GIB).ok_or_else(|| {
        BlenderPlanCommandError::new(
            "blender_vram_overflow",
            "minimum VRAM GiB value overflows the bounded byte representation",
        )
    })?;
    let accelerator = AcceleratorRequirement::new(
        AcceleratorVendor::Nvidia,
        AcceleratorApi::Cuda,
        minimum_memory_bytes,
        intent,
        maximum_rtt_ms,
    )
    .map_err(|error| {
        BlenderPlanCommandError::new(error.code(), "accelerator requirement is invalid")
    })?;

    BlenderBurstWorkPlan::new(&snapshot, &inventory, accelerator).map_err(plan_error)
}

fn read_bounded_document(
    path: &Path,
    maximum_bytes: u64,
    code: &'static str,
    message: &'static str,
) -> Result<Vec<u8>, BlenderPlanCommandError> {
    let file =
        std::fs::File::open(path).map_err(|_| BlenderPlanCommandError::new(code, message))?;
    let metadata = file
        .metadata()
        .map_err(|_| BlenderPlanCommandError::new(code, message))?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(BlenderPlanCommandError::new(code, message));
    }
    let capacity = usize::try_from(metadata.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BlenderPlanCommandError::new(code, message))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(BlenderPlanCommandError::new(code, message));
    }
    Ok(bytes)
}

fn snapshot_error(error: BlenderSnapshotDocumentError) -> BlenderPlanCommandError {
    BlenderPlanCommandError::new(error.code(), "Blender snapshot document is invalid")
}

fn inventory_error(error: BlenderRemoteInventoryDocumentError) -> BlenderPlanCommandError {
    BlenderPlanCommandError::new(error.code(), "Blender remote inventory document is invalid")
}

fn plan_error(error: BlenderBurstWorkPlanError) -> BlenderPlanCommandError {
    BlenderPlanCommandError::new(error.code(), "Blender burst-work plan could not be built")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlenderPlanCommandError {
    code: &'static str,
    message: &'static str,
}

impl BlenderPlanCommandError {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for BlenderPlanCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for BlenderPlanCommandError {}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_document(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "glaeda-blender-plan-{}-{nonce}-{name}.json",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn snapshot_document() -> Vec<u8> {
        br#"{
          "schema_version": 1,
          "runtime_id": "blender-5.2.0",
          "main_scene": "scenes/main.blend",
          "files": [
            {
              "relative_path": "scenes/main.blend",
              "class": "scene",
              "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "bytes": 100
            },
            {
              "relative_path": "assets/wood.exr",
              "class": "asset",
              "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
              "bytes": 200
            }
          ]
        }"#
        .to_vec()
    }

    #[test]
    fn one_scene_delta_builds_read_only_plan() {
        let snapshot = temporary_document("snapshot", &snapshot_document());
        let inventory = temporary_document(
            "inventory",
            br#"{
              "schema_version": 1,
              "objects": [{
                "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "bytes": 200
              }]
            }"#,
        );
        let plan =
            build_blender_plan(&snapshot, &inventory, 24, AcceleratorIntent::Batch, None).unwrap();
        std::fs::remove_file(snapshot).unwrap();
        std::fs::remove_file(inventory).unwrap();

        assert_eq!(plan.logical_project_files(), 2);
        assert_eq!(plan.present_bytes(), 200);
        assert_eq!(plan.missing_bytes(), 100);
        assert_eq!(plan.accelerator().minimum_memory_bytes(), 24 * GIB);
    }

    #[test]
    fn interactive_plan_requires_rtt_ceiling() {
        let snapshot = temporary_document("snapshot", &snapshot_document());
        let inventory = temporary_document("inventory", br#"{"schema_version":1,"objects":[]}"#);
        let error = build_blender_plan(
            &snapshot,
            &inventory,
            24,
            AcceleratorIntent::Interactive,
            None,
        )
        .unwrap_err();
        std::fs::remove_file(snapshot).unwrap();
        std::fs::remove_file(inventory).unwrap();
        assert_eq!(error.code(), "interactive_rtt_required");
    }
}
