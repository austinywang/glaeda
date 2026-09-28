#[path = "../blender_plan_command.rs"]
mod blender_plan_command;

use std::path::PathBuf;
use std::process::ExitCode;

use blender_plan_command::build_blender_plan;
use clap::{Parser, Subcommand, ValueEnum};
use glaeda::compute_execution_request::accelerator_burst::AcceleratorIntent;
use glaeda::compute_execution_request::blender_burst_work_plan::render_blender_burst_work_plan_human;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-blender",
    version,
    about = "Plan read-only Blender burst work without provider effects"
)]
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
    /// Compose a portable Blender snapshot, remote content inventory, and accelerator requirement.
    Plan {
        /// Private portable snapshot JSON emitted by the Blender exporter.
        #[arg(long)]
        snapshot: PathBuf,
        /// Provider-neutral remote content inventory JSON.
        #[arg(long)]
        inventory: PathBuf,
        /// Minimum NVIDIA VRAM in GiB.
        #[arg(long)]
        minimum_vram_gib: u64,
        /// Batch or interactive accelerator intent.
        #[arg(long, value_enum)]
        intent: BlenderIntent,
        /// Maximum acceptable RTT in milliseconds; required only for interactive intent.
        #[arg(long)]
        maximum_rtt_ms: Option<u32>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BlenderIntent {
    Batch,
    Interactive,
}

impl From<BlenderIntent> for AcceleratorIntent {
    fn from(value: BlenderIntent) -> Self {
        match value {
            BlenderIntent::Batch => Self::Batch,
            BlenderIntent::Interactive => Self::Interactive,
        }
    }
}

#[derive(Debug, Serialize)]
struct RuntimeErrorReport<'a> {
    schema_version: u8,
    kind: &'a str,
    message: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Plan {
            snapshot,
            inventory,
            minimum_vram_gib,
            intent,
            maximum_rtt_ms,
        } => {
            let plan = match build_blender_plan(
                &snapshot,
                &inventory,
                minimum_vram_gib,
                intent.into(),
                maximum_rtt_ms,
            ) {
                Ok(plan) => plan,
                Err(error) => {
                    return emit_error(cli.output, error.code(), error.to_string());
                }
            };
            match cli.output {
                OutputFormat::Human => print!("{}", render_blender_burst_work_plan_human(&plan)),
                OutputFormat::Json => match serde_json::to_string_pretty(&plan) {
                    Ok(json) => println!("{json}"),
                    Err(_) => {
                        return emit_error(
                            cli.output,
                            "blender_plan_serialization_failed",
                            "Blender burst-work plan could not be serialized".to_owned(),
                        );
                    }
                },
            }
            ExitCode::SUCCESS
        }
    }
}

fn emit_error(output: OutputFormat, kind: &str, message: String) -> ExitCode {
    match output {
        OutputFormat::Human => eprintln!("{message}"),
        OutputFormat::Json => {
            let report = RuntimeErrorReport {
                schema_version: 1,
                kind,
                message,
            };
            match serde_json::to_string_pretty(&report) {
                Ok(json) => println!("{json}"),
                Err(_) => eprintln!("Blender plan error"),
            }
        }
    }
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_cli_requires_explicit_snapshot_inventory_and_accelerator_intent() {
        let cli = Cli::try_parse_from([
            "glaeda-blender",
            "--output",
            "json",
            "plan",
            "--snapshot",
            "snapshot.json",
            "--inventory",
            "inventory.json",
            "--minimum-vram-gib",
            "24",
            "--intent",
            "interactive",
            "--maximum-rtt-ms",
            "80",
        ])
        .unwrap();
        let Command::Plan {
            snapshot,
            inventory,
            minimum_vram_gib,
            intent,
            maximum_rtt_ms,
        } = cli.command;
        assert_eq!(snapshot, PathBuf::from("snapshot.json"));
        assert_eq!(inventory, PathBuf::from("inventory.json"));
        assert_eq!(minimum_vram_gib, 24);
        assert!(matches!(intent, BlenderIntent::Interactive));
        assert_eq!(maximum_rtt_ms, Some(80));
    }
}
