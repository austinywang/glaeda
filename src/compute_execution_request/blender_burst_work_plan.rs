//! Pure composition of a Blender snapshot, persistent content inventory, and accelerator intent.
//!
//! This report is the read-only pre-provider view for burst Blender work. It explains exact local
//! project identity, remote content heat, materialization identity, and accelerator requirements.
//! It grants zero file, network, provider, billing, storage, lease, render, or execution authority.

use std::fmt::Write as _;

use serde::Serialize;

use super::accelerator_burst::{
    AcceleratorApi, AcceleratorIntent, AcceleratorRequirement, AcceleratorVendor,
};
use super::blender_content_store::{
    BlenderContentStoreDisposition, BlenderContentStorePlan, BlenderContentStorePlanError,
};
use super::blender_snapshot::{BlenderProjectSnapshot, BlenderRemoteInventory};

pub const BLENDER_BURST_WORK_PLAN_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlenderBurstWorkPlan {
    schema_version: u8,
    snapshot_digest: String,
    blender_runtime: String,
    main_scene: String,
    logical_project_files: usize,
    unique_content_objects: usize,
    present_content_objects: usize,
    missing_content_objects: usize,
    required_bytes: u64,
    present_bytes: u64,
    missing_bytes: u64,
    materialization_plan_digest: String,
    accelerator: AcceleratorRequirement,
    effect_class: &'static str,
}

impl BlenderBurstWorkPlan {
    pub fn new(
        snapshot: &BlenderProjectSnapshot,
        remote: &BlenderRemoteInventory,
        accelerator: AcceleratorRequirement,
    ) -> Result<Self, BlenderBurstWorkPlanError> {
        let store = BlenderContentStorePlan::new(snapshot, remote).map_err(store_error)?;
        let present_content_objects = store
            .objects()
            .iter()
            .filter(|object| object.disposition() == BlenderContentStoreDisposition::Present)
            .count();
        let missing_content_objects = store.objects().len() - present_content_objects;

        Ok(Self {
            schema_version: BLENDER_BURST_WORK_PLAN_SCHEMA_VERSION,
            snapshot_digest: snapshot.snapshot_digest().as_str().to_owned(),
            blender_runtime: snapshot.runtime_id().as_str().to_owned(),
            main_scene: snapshot.main_scene().as_str().to_owned(),
            logical_project_files: snapshot.files().len(),
            unique_content_objects: store.objects().len(),
            present_content_objects,
            missing_content_objects,
            required_bytes: store.required_bytes(),
            present_bytes: store.present_bytes(),
            missing_bytes: store.missing_bytes(),
            materialization_plan_digest: store.plan_digest().as_str().to_owned(),
            accelerator,
            effect_class: "read_only_plan",
        })
    }

    #[must_use]
    pub fn snapshot_digest(&self) -> &str {
        &self.snapshot_digest
    }

    #[must_use]
    pub fn blender_runtime(&self) -> &str {
        &self.blender_runtime
    }

    #[must_use]
    pub fn main_scene(&self) -> &str {
        &self.main_scene
    }

    #[must_use]
    pub const fn logical_project_files(&self) -> usize {
        self.logical_project_files
    }

    #[must_use]
    pub const fn unique_content_objects(&self) -> usize {
        self.unique_content_objects
    }

    #[must_use]
    pub const fn present_content_objects(&self) -> usize {
        self.present_content_objects
    }

    #[must_use]
    pub const fn missing_content_objects(&self) -> usize {
        self.missing_content_objects
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
    pub fn materialization_plan_digest(&self) -> &str {
        &self.materialization_plan_digest
    }

    #[must_use]
    pub const fn accelerator(&self) -> &AcceleratorRequirement {
        &self.accelerator
    }
}

#[must_use]
pub fn render_blender_burst_work_plan_human(plan: &BlenderBurstWorkPlan) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "Blender burst plan");
    let _ = writeln!(output, "snapshot: {}", plan.snapshot_digest());
    let _ = writeln!(output, "runtime: {}", plan.blender_runtime());
    let _ = writeln!(output, "main scene: {}", plan.main_scene());
    let _ = writeln!(
        output,
        "project: {} logical files / {} unique content objects",
        plan.logical_project_files(),
        plan.unique_content_objects()
    );
    let _ = writeln!(
        output,
        "remote content: {} present / {} missing objects",
        plan.present_content_objects(),
        plan.missing_content_objects()
    );
    let _ = writeln!(
        output,
        "bytes: {} present / {} missing / {} total",
        plan.present_bytes(),
        plan.missing_bytes(),
        plan.required_bytes()
    );
    let _ = writeln!(
        output,
        "materialization: {}",
        plan.materialization_plan_digest()
    );
    let _ = writeln!(
        output,
        "accelerator: {}/{} >= {} bytes{}",
        accelerator_vendor_name(plan.accelerator().vendor()),
        accelerator_api_name(plan.accelerator().api()),
        plan.accelerator().minimum_memory_bytes(),
        accelerator_interaction_suffix(plan.accelerator())
    );
    let _ = writeln!(output, "effects: none");
    output
}

const fn accelerator_vendor_name(vendor: AcceleratorVendor) -> &'static str {
    match vendor {
        AcceleratorVendor::Nvidia => "nvidia",
    }
}

const fn accelerator_api_name(api: AcceleratorApi) -> &'static str {
    match api {
        AcceleratorApi::Cuda => "cuda",
    }
}

fn accelerator_interaction_suffix(requirement: &AcceleratorRequirement) -> String {
    match requirement.intent() {
        AcceleratorIntent::Batch => " · batch".to_owned(),
        AcceleratorIntent::Interactive => format!(
            " · interactive · max RTT {} ms",
            requirement
                .maximum_rtt_ms()
                .expect("interactive requirements always carry an RTT ceiling")
        ),
    }
}

fn store_error(error: BlenderContentStorePlanError) -> BlenderBurstWorkPlanError {
    BlenderBurstWorkPlanError {
        code: error.code(),
        message: "Blender burst plan could not compose the content-store plan",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlenderBurstWorkPlanError {
    code: &'static str,
    message: &'static str,
}

impl BlenderBurstWorkPlanError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for BlenderBurstWorkPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for BlenderBurstWorkPlanError {}

#[cfg(test)]
mod tests {
    use crate::artifact::Sha256Digest;

    use super::super::blender_snapshot::{
        BlenderContentObject, BlenderProjectFile, BlenderProjectFileClass, BlenderRelativePath,
        BlenderRemoteInventory, BlenderRuntimeId,
    };
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn digest(value: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", value.to_string().repeat(64))).unwrap()
    }

    fn snapshot(scene_digest: char) -> BlenderProjectSnapshot {
        BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-5.2.0").unwrap(),
            BlenderRelativePath::parse("scenes/main.blend").unwrap(),
            vec![
                BlenderProjectFile::new(
                    BlenderRelativePath::parse("scenes/main.blend").unwrap(),
                    BlenderProjectFileClass::Scene,
                    digest(scene_digest),
                    100,
                )
                .unwrap(),
                BlenderProjectFile::new(
                    BlenderRelativePath::parse("assets/wood.exr").unwrap(),
                    BlenderProjectFileClass::Asset,
                    digest('b'),
                    200,
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    fn requirement(intent: AcceleratorIntent) -> AcceleratorRequirement {
        AcceleratorRequirement::new(
            AcceleratorVendor::Nvidia,
            AcceleratorApi::Cuda,
            24 * GIB,
            intent,
            (intent == AcceleratorIntent::Interactive).then_some(80),
        )
        .unwrap()
    }

    #[test]
    fn empty_remote_inventory_reports_all_content_missing() {
        let plan = BlenderBurstWorkPlan::new(
            &snapshot('a'),
            &BlenderRemoteInventory::new(vec![]).unwrap(),
            requirement(AcceleratorIntent::Batch),
        )
        .unwrap();
        assert_eq!(plan.logical_project_files(), 2);
        assert_eq!(plan.unique_content_objects(), 2);
        assert_eq!(plan.present_content_objects(), 0);
        assert_eq!(plan.missing_content_objects(), 2);
        assert_eq!(plan.present_bytes(), 0);
        assert_eq!(plan.missing_bytes(), 300);
    }

    #[test]
    fn warm_remote_inventory_reports_zero_missing_bytes() {
        let plan = BlenderBurstWorkPlan::new(
            &snapshot('a'),
            &BlenderRemoteInventory::new(vec![
                BlenderContentObject::new(digest('a'), 100).unwrap(),
                BlenderContentObject::new(digest('b'), 200).unwrap(),
            ])
            .unwrap(),
            requirement(AcceleratorIntent::Interactive),
        )
        .unwrap();
        assert_eq!(plan.present_content_objects(), 2);
        assert_eq!(plan.missing_content_objects(), 0);
        assert_eq!(plan.present_bytes(), 300);
        assert_eq!(plan.missing_bytes(), 0);
        let human = render_blender_burst_work_plan_human(&plan);
        assert!(human.contains("interactive · max RTT 80 ms"));
        assert!(human.contains("effects: none"));
    }

    #[test]
    fn one_scene_edit_reports_only_new_scene_content_missing() {
        let plan = BlenderBurstWorkPlan::new(
            &snapshot('c'),
            &BlenderRemoteInventory::new(vec![
                BlenderContentObject::new(digest('a'), 100).unwrap(),
                BlenderContentObject::new(digest('b'), 200).unwrap(),
            ])
            .unwrap(),
            requirement(AcceleratorIntent::Batch),
        )
        .unwrap();
        assert_eq!(plan.present_content_objects(), 1);
        assert_eq!(plan.missing_content_objects(), 1);
        assert_eq!(plan.present_bytes(), 200);
        assert_eq!(plan.missing_bytes(), 100);
    }

    #[test]
    fn public_json_contains_no_provider_or_host_path() {
        let plan = BlenderBurstWorkPlan::new(
            &snapshot('a'),
            &BlenderRemoteInventory::new(vec![]).unwrap(),
            requirement(AcceleratorIntent::Batch),
        )
        .unwrap();
        let json = serde_json::to_string(&plan).unwrap();
        for forbidden in ["/home/", "/Users/", "runpod", "vast.ai", "https://"] {
            assert!(!json.contains(forbidden));
        }
        assert!(json.contains("read_only_plan"));
    }
}
