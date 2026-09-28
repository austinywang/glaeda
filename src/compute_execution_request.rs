//! Pure workload-neutral pre-admission request plus product-neutral capacity admission.
//!
//! The request carrier remains non-authoritative. Capacity ownership appears only through the
//! explicit `capacity_admission` boundary after the existing fail-closed arithmetic accepts it.

mod request;

pub use request::{COMPUTE_EXECUTION_REQUEST_SCHEMA_VERSION, ComputeExecutionRequest};

pub mod accelerator_burst;
pub mod blender_burst_work_plan;
pub mod blender_content_store;
pub mod blender_remote_inventory_document;
pub mod blender_snapshot;
pub mod blender_snapshot_document;
pub mod capacity_admission;
