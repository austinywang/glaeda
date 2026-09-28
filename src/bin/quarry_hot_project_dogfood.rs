use std::error::Error;
use std::fmt;

use clap::{Args, Parser, Subcommand, ValueEnum};
use glaeda::artifact::{CommitId, GitTreeId, Sha256Digest};
use glaeda::hot_execution_performance::{
    HotBuildState, HotDependencyState, HotExecutionHeat, HotExecutionMilestones, HotExecutionMode,
    HotExecutionPerformanceIdentity, HotExecutionPerformanceReceipt,
    HotExecutionResourceObservation, HotExecutionResultClass, HotExecutionStorageObservation,
    HotIndexServiceState, HotRepositoryState, HotSandboxState,
};
use serde::Serialize;

const WORKLOAD_ID: &str = "quarry-agent-brief-edit-test-v1";
const PROJECT_ID: &str = "quarry";
const BACKEND_ID: &str = "lima-vz";
const CONTROL_ID: &str = "trusted-mac-current";
const HOT_ID: &str = "project-disk-ext4-overlay";

#[derive(Debug, Parser)]
#[command(about = "Plan and record the first Quarry resident-project dogfood experiment")]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Emit the frozen experiment contract. Performs no host or guest mutation.
    Plan(PlanArgs),
    /// Wrap already-owned benchmark observations in a bounded receipt.
    Sample(Box<SampleArgs>),
}

#[derive(Debug, Args)]
struct PlanArgs {
    #[arg(long)]
    quarry_commit: String,
    #[arg(long)]
    quarry_tree: String,
    #[arg(long)]
    smolrunner_commit: String,
}

#[derive(Debug, Args)]
struct SampleArgs {
    #[arg(long)]
    quarry_commit: String,
    #[arg(long)]
    quarry_tree: String,
    #[arg(long)]
    smolrunner_commit: String,
    #[arg(long, value_enum)]
    arm: Arm,
    #[arg(long, value_enum)]
    sample_class: SampleClass,
    #[arg(long)]
    ordinal: u16,
    #[arg(long)]
    fanout: Option<u8>,
    #[arg(long, value_enum)]
    execution_mode: ExecutionMode,
    #[arg(long)]
    host_class: String,
    #[arg(long)]
    resource_profile: String,
    #[arg(long)]
    total_elapsed_millis: u64,
    #[arg(long)]
    sandbox_ready_millis: Option<u64>,
    #[arg(long)]
    repository_ready_millis: Option<u64>,
    #[arg(long)]
    dependency_ready_millis: Option<u64>,
    #[arg(long)]
    first_useful_command_millis: u64,
    #[arg(long)]
    edit_complete_millis: Option<u64>,
    #[arg(long)]
    focused_pytest_result_millis: Option<u64>,
    #[arg(long)]
    final_relevant_result_millis: Option<u64>,
    #[arg(long)]
    residency_transition_millis: Option<u64>,
    #[arg(long)]
    agent_brief_digest: Option<String>,
    #[arg(long)]
    guest_logical_bytes: Option<u64>,
    #[arg(long)]
    guest_filesystem_used_bytes: Option<u64>,
    #[arg(long)]
    host_backing_logical_bytes: Option<u64>,
    #[arg(long)]
    host_backing_allocated_bytes: Option<u64>,
    #[arg(long)]
    task_filesystem_used_delta_bytes: Option<u64>,
    #[arg(long)]
    peak_guest_memory_bytes: Option<u64>,
    #[arg(long)]
    host_memory_delta_bytes: Option<i64>,
    #[arg(long)]
    cpu_time_millis: Option<u64>,
    #[arg(long, value_enum, default_value_t = ResultClass::Succeeded)]
    result: ResultClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Arm {
    Control,
    HotProject,
}

impl Arm {
    const fn candidate_id(self) -> &'static str {
        match self {
            Self::Control => CONTROL_ID,
            Self::HotProject => HOT_ID,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum SampleClass {
    Singleton,
    Sequential,
    Fanout,
    Restart,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExecutionMode {
    ColdDisposable,
    PreparedDisposable,
    ResidentAfterIdle,
    ResidentImmediate,
    ResidentTaskLoop,
}

impl From<ExecutionMode> for HotExecutionMode {
    fn from(value: ExecutionMode) -> Self {
        match value {
            ExecutionMode::ColdDisposable => Self::ColdDisposable,
            ExecutionMode::PreparedDisposable => Self::PreparedDisposable,
            ExecutionMode::ResidentAfterIdle => Self::ResidentAfterIdle,
            ExecutionMode::ResidentImmediate => Self::ResidentImmediate,
            ExecutionMode::ResidentTaskLoop => Self::ResidentTaskLoop,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ResultClass {
    Succeeded,
    Failed,
    Canceled,
    ResetRequired,
    Unknown,
}

impl From<ResultClass> for HotExecutionResultClass {
    fn from(value: ResultClass) -> Self {
        match value {
            ResultClass::Succeeded => Self::Succeeded,
            ResultClass::Failed => Self::Failed,
            ResultClass::Canceled => Self::Canceled,
            ResultClass::ResetRequired => Self::ResetRequired,
            ResultClass::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Serialize)]
struct Plan {
    schema_version: u8,
    authority: &'static str,
    physical_mutation: &'static str,
    quarry_commit: CommitId,
    quarry_tree: GitTreeId,
    smolrunner_commit: CommitId,
    workload_id: &'static str,
    control_candidate_id: &'static str,
    hot_candidate_id: &'static str,
    persistent_disk_state: [&'static str; 7],
    deliberately_outside_disk: [&'static str; 5],
    orientation_command: &'static str,
    edit_target: &'static str,
    focused_test_command: &'static str,
    cold_oracle_command: &'static str,
    sequential_tasks: u8,
    fanout_widths: [u8; 3],
    restart_cycles: u8,
    acceptance: Acceptance,
}

#[derive(Debug, Serialize)]
struct Acceptance {
    first_command_p50_max_control_basis_points: u16,
    first_command_p90_max_control_basis_points: u16,
    edit_to_pytest_p50_max_control_basis_points: u16,
    fanout_min_speedup_milli: u16,
    max_break_even_tasks: u8,
    exact_equivalence_required: bool,
    unexplained_second_cycle_growth_allowed: bool,
    cold_oracle_required: bool,
}

#[derive(Debug, Serialize)]
struct EditObservation {
    edit_complete_millis: u64,
    focused_pytest_result_millis: u64,
    edit_to_focused_pytest_millis: u64,
}

#[derive(Debug, Serialize)]
struct Sample {
    schema_version: u8,
    authority: &'static str,
    physical_mutation: &'static str,
    arm: Arm,
    sample_class: SampleClass,
    ordinal: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    fanout: Option<u8>,
    quarry_commit: CommitId,
    quarry_tree: GitTreeId,
    smolrunner_commit: CommitId,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_brief_digest: Option<Sha256Digest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit: Option<EditObservation>,
    performance: HotExecutionPerformanceReceipt,
}

#[derive(Debug)]
struct DogfoodError(&'static str);

impl fmt::Display for DogfoodError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for DogfoodError {}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Plan(args) => render(&build_plan(args)?, cli.output)?,
        Command::Sample(args) => {
            let sample = build_sample(*args)?;
            match cli.output {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&sample)?),
                OutputFormat::Human => {
                    if let Some(edit) = &sample.edit {
                        println!(
                            "edit -> focused pytest: {} ms",
                            edit.edit_to_focused_pytest_millis
                        );
                    }
                    print!("{}", sample.performance.render_human());
                }
            }
        }
    }
    Ok(())
}

fn render(plan: &Plan, output: OutputFormat) -> Result<(), Box<dyn Error>> {
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(plan)?),
        OutputFormat::Human => {
            println!("Quarry hot-project dogfood v{}", plan.schema_version);
            println!("Quarry commit: {}", plan.quarry_commit.as_str());
            println!("Quarry tree: {}", plan.quarry_tree.as_str());
            println!("SmolRunner commit: {}", plan.smolrunner_commit.as_str());
            println!("fanout: 1/8/32; restart cycles: {}", plan.restart_cycles);
            println!("physical mutation: {}", plan.physical_mutation);
        }
    }
    Ok(())
}

fn build_plan(args: PlanArgs) -> Result<Plan, Box<dyn Error>> {
    Ok(Plan {
        schema_version: 1,
        authority: "observation_only",
        physical_mutation: "sealed_until_separately_authorized",
        quarry_commit: CommitId::parse(&args.quarry_commit)?,
        quarry_tree: GitTreeId::parse(&args.quarry_tree)?,
        smolrunner_commit: CommitId::parse(&args.smolrunner_commit)?,
        workload_id: WORKLOAD_ID,
        control_candidate_id: CONTROL_ID,
        hot_candidate_id: HOT_ID,
        persistent_disk_state: [
            "immutable_git_object_pool",
            "resident_clean_source_anchor",
            "python_dependency_environment",
            "dependency_cache",
            "exact_parented_derived_indexes",
            "task_private_git_metadata",
            "task_overlay_upper_work",
        ],
        deliberately_outside_disk: [
            "disk_lease_attachment_and_correlation_authority",
            "source_and_verification_authority",
            "credentials_and_jit_material",
            "performance_receipts",
            "canonical_cold_reconstruction_inputs",
        ],
        orientation_command: "python -m quarry.agent_brief --format json",
        edit_target: "src/quarry/agent_brief.py",
        focused_test_command: "python -m pytest -q tests/test_agent_brief.py --disable-warnings",
        cold_oracle_command: "python scripts/run_local_tests.py exact-head --base origin/main",
        sequential_tasks: 10,
        fanout_widths: [1, 8, 32],
        restart_cycles: 3,
        acceptance: Acceptance {
            first_command_p50_max_control_basis_points: 5_000,
            first_command_p90_max_control_basis_points: 6_500,
            edit_to_pytest_p50_max_control_basis_points: 11_000,
            fanout_min_speedup_milli: 2_000,
            max_break_even_tasks: 10,
            exact_equivalence_required: true,
            unexplained_second_cycle_growth_allowed: false,
            cold_oracle_required: true,
        },
    })
}

fn build_sample(args: SampleArgs) -> Result<Sample, Box<dyn Error>> {
    validate_coordinates(args.arm, args.sample_class, args.ordinal, args.fanout)?;
    let mode: HotExecutionMode = args.execution_mode.into();
    validate_mode(args.arm, args.sample_class, mode)?;
    let quarry_commit = CommitId::parse(&args.quarry_commit)?;
    let edit = edit_observation(args.edit_complete_millis, args.focused_pytest_result_millis)?;
    if edit
        .as_ref()
        .is_some_and(|value| value.edit_complete_millis < args.first_useful_command_millis)
    {
        return Err(Box::new(DogfoodError("dogfood_edit_before_first_command")));
    }
    let milestones = HotExecutionMilestones::new(
        args.sandbox_ready_millis,
        args.repository_ready_millis,
        args.dependency_ready_millis,
        Some(args.first_useful_command_millis),
        args.focused_pytest_result_millis,
        args.final_relevant_result_millis,
        args.residency_transition_millis,
    )?;
    let storage = storage_observation(&args)?;
    let resources = resource_observation(&args)?;
    let source_id = format!("git:{}", quarry_commit.as_str());
    let performance = HotExecutionPerformanceReceipt::new(
        HotExecutionPerformanceIdentity::new(
            WORKLOAD_ID,
            PROJECT_ID,
            &source_id,
            args.arm.candidate_id(),
            BACKEND_ID,
            &args.host_class,
            &args.resource_profile,
        )?,
        mode,
        args.total_elapsed_millis,
        milestones,
        heat(mode),
        storage,
        resources,
        args.result.into(),
    )?;

    Ok(Sample {
        schema_version: 1,
        authority: "observation_only",
        physical_mutation: "sealed_until_separately_authorized",
        arm: args.arm,
        sample_class: args.sample_class,
        ordinal: args.ordinal,
        fanout: args.fanout,
        quarry_commit,
        quarry_tree: GitTreeId::parse(&args.quarry_tree)?,
        smolrunner_commit: CommitId::parse(&args.smolrunner_commit)?,
        agent_brief_digest: args
            .agent_brief_digest
            .as_deref()
            .map(Sha256Digest::parse)
            .transpose()?,
        edit,
        performance,
    })
}

fn edit_observation(
    edit_millis: Option<u64>,
    pytest_millis: Option<u64>,
) -> Result<Option<EditObservation>, DogfoodError> {
    match (edit_millis, pytest_millis) {
        (None, None) => Ok(None),
        (Some(edit), Some(result)) if result >= edit => Ok(Some(EditObservation {
            edit_complete_millis: edit,
            focused_pytest_result_millis: result,
            edit_to_focused_pytest_millis: result - edit,
        })),
        (Some(_), Some(_)) => Err(DogfoodError("dogfood_result_before_edit")),
        _ => Err(DogfoodError("dogfood_incomplete_edit_measurement")),
    }
}

fn storage_observation(
    args: &SampleArgs,
) -> Result<Option<HotExecutionStorageObservation>, Box<dyn Error>> {
    if args.guest_logical_bytes.is_none()
        && args.guest_filesystem_used_bytes.is_none()
        && args.host_backing_logical_bytes.is_none()
        && args.host_backing_allocated_bytes.is_none()
        && args.task_filesystem_used_delta_bytes.is_none()
    {
        return Ok(None);
    }
    Ok(Some(HotExecutionStorageObservation::new(
        args.guest_logical_bytes,
        args.guest_filesystem_used_bytes,
        args.host_backing_logical_bytes,
        args.host_backing_allocated_bytes,
        args.task_filesystem_used_delta_bytes,
    )?))
}

fn resource_observation(
    args: &SampleArgs,
) -> Result<Option<HotExecutionResourceObservation>, Box<dyn Error>> {
    if args.peak_guest_memory_bytes.is_none()
        && args.host_memory_delta_bytes.is_none()
        && args.cpu_time_millis.is_none()
    {
        return Ok(None);
    }
    Ok(Some(HotExecutionResourceObservation::new(
        args.peak_guest_memory_bytes,
        args.host_memory_delta_bytes,
        args.cpu_time_millis,
    )?))
}

fn validate_coordinates(
    arm: Arm,
    class: SampleClass,
    ordinal: u16,
    fanout: Option<u8>,
) -> Result<(), DogfoodError> {
    if ordinal == 0 {
        return Err(DogfoodError("dogfood_ordinal_must_be_positive"));
    }
    if matches!(class, SampleClass::Restart | SampleClass::Fallback) && arm != Arm::HotProject {
        return Err(DogfoodError(
            "dogfood_restart_and_fallback_belong_to_hot_project_arm",
        ));
    }
    match class {
        SampleClass::Fanout if !matches!(fanout, Some(1 | 8 | 32)) => {
            Err(DogfoodError("dogfood_fanout_must_be_1_8_or_32"))
        }
        SampleClass::Sequential if ordinal > 10 => {
            Err(DogfoodError("dogfood_sequential_ordinal_exceeds_10"))
        }
        SampleClass::Restart if ordinal > 3 => {
            Err(DogfoodError("dogfood_restart_ordinal_exceeds_3"))
        }
        SampleClass::Fanout => Ok(()),
        _ if fanout.is_some() => Err(DogfoodError("dogfood_unexpected_fanout")),
        _ => Ok(()),
    }
}

fn validate_mode(arm: Arm, class: SampleClass, mode: HotExecutionMode) -> Result<(), DogfoodError> {
    let cold_or_prepared = matches!(
        mode,
        HotExecutionMode::ColdDisposable | HotExecutionMode::PreparedDisposable
    );
    let resident = matches!(
        mode,
        HotExecutionMode::ResidentAfterIdle
            | HotExecutionMode::ResidentImmediate
            | HotExecutionMode::ResidentTaskLoop
    );
    let valid = match (arm, class) {
        (Arm::Control, _) => cold_or_prepared,
        (Arm::HotProject, SampleClass::Fallback) => cold_or_prepared,
        (Arm::HotProject, SampleClass::Restart) => mode == HotExecutionMode::ResidentAfterIdle,
        (Arm::HotProject, _) => resident,
    };
    valid
        .then_some(())
        .ok_or(DogfoodError("dogfood_arm_sample_mode_mismatch"))
}

fn heat(mode: HotExecutionMode) -> HotExecutionHeat {
    match mode {
        HotExecutionMode::ColdDisposable => HotExecutionHeat::new(
            HotSandboxState::Cold,
            HotRepositoryState::Cold,
            HotDependencyState::Cold,
            HotBuildState::Cold,
            HotIndexServiceState::Unavailable,
        ),
        HotExecutionMode::PreparedDisposable => HotExecutionHeat::new(
            HotSandboxState::Prepared,
            HotRepositoryState::CheckoutHit,
            HotDependencyState::EnvironmentHit,
            HotBuildState::Cold,
            HotIndexServiceState::Unavailable,
        ),
        HotExecutionMode::ResidentAfterIdle => HotExecutionHeat::new(
            HotSandboxState::Resumed,
            HotRepositoryState::TaskFork,
            HotDependencyState::EnvironmentHit,
            HotBuildState::Cold,
            HotIndexServiceState::Unavailable,
        ),
        HotExecutionMode::ResidentImmediate | HotExecutionMode::ResidentTaskLoop => {
            HotExecutionHeat::new(
                HotSandboxState::ResidentHit,
                HotRepositoryState::TaskFork,
                HotDependencyState::EnvironmentHit,
                HotBuildState::Cold,
                HotIndexServiceState::Unavailable,
            )
        }
        _ => unreachable!("Quarry dogfood CLI excludes other execution modes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const TREE: &str = "89abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn plan_keeps_physical_work_sealed_and_freezes_required_matrix() {
        let plan = build_plan(PlanArgs {
            quarry_commit: COMMIT.to_owned(),
            quarry_tree: TREE.to_owned(),
            smolrunner_commit: COMMIT.to_owned(),
        })
        .expect("plan is valid");
        assert_eq!(plan.physical_mutation, "sealed_until_separately_authorized");
        assert_eq!(plan.fanout_widths, [1, 8, 32]);
        assert_eq!(plan.restart_cycles, 3);
        assert_eq!(
            plan.cold_oracle_command,
            "python scripts/run_local_tests.py exact-head --base origin/main"
        );
        assert!(
            plan.deliberately_outside_disk
                .contains(&"disk_lease_attachment_and_correlation_authority")
        );
    }

    #[test]
    fn edit_delta_is_direct_and_reversed_timing_is_refused() {
        let edit = edit_observation(Some(100), Some(2_500))
            .expect("valid edit observation")
            .expect("edit observation exists");
        assert_eq!(edit.edit_to_focused_pytest_millis, 2_400);
        assert!(edit_observation(Some(100), Some(99)).is_err());
        assert!(edit_observation(Some(100), None).is_err());
    }

    #[test]
    fn experiment_coordinates_and_actual_modes_are_bounded() {
        assert!(validate_coordinates(Arm::Control, SampleClass::Fanout, 1, Some(1)).is_ok());
        assert!(validate_coordinates(Arm::Control, SampleClass::Fanout, 1, Some(8)).is_ok());
        assert!(validate_coordinates(Arm::Control, SampleClass::Fanout, 1, Some(32)).is_ok());
        assert!(validate_coordinates(Arm::Control, SampleClass::Fanout, 1, Some(4)).is_err());
        assert!(validate_coordinates(Arm::Control, SampleClass::Sequential, 11, None).is_err());
        assert!(validate_coordinates(Arm::HotProject, SampleClass::Restart, 4, None).is_err());
        assert!(validate_coordinates(Arm::Control, SampleClass::Restart, 1, None).is_err());
        assert!(validate_coordinates(Arm::Control, SampleClass::Fallback, 1, None).is_err());

        assert!(
            validate_mode(
                Arm::Control,
                SampleClass::Singleton,
                HotExecutionMode::PreparedDisposable
            )
            .is_ok()
        );
        assert!(
            validate_mode(
                Arm::HotProject,
                SampleClass::Singleton,
                HotExecutionMode::ResidentImmediate
            )
            .is_ok()
        );
        assert!(
            validate_mode(
                Arm::HotProject,
                SampleClass::Restart,
                HotExecutionMode::ResidentAfterIdle
            )
            .is_ok()
        );
        assert!(
            validate_mode(
                Arm::HotProject,
                SampleClass::Fallback,
                HotExecutionMode::ColdDisposable
            )
            .is_ok()
        );
        assert!(
            validate_mode(
                Arm::HotProject,
                SampleClass::Fallback,
                HotExecutionMode::ResidentTaskLoop
            )
            .is_err()
        );
        assert_eq!(
            heat(HotExecutionMode::ColdDisposable).sandbox(),
            HotSandboxState::Cold
        );
    }
}
