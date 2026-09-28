#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("Cargo target reclaim requires Linux");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::time::{SystemTime, UNIX_EPOCH};

    use clap::{Parser, ValueEnum};
    use glaeda::cargo_target_holder_observation::observe_cargo_target_holders;
    use glaeda::cargo_target_observation::observe_cargo_target;
    use glaeda::cargo_target_reclaim::{
        CargoTargetReclaimDecision, CargoTargetReclaimOutcome, CargoTargetReclaimPolicy,
        plan_cargo_target_reclaim, reclaim_cargo_target,
    };
    use serde::Serialize;

    const REPORT_SCHEMA_VERSION: u8 = 1;

    /// Default idle window: a week.
    ///
    /// The policy floor is an hour, which is only enough to keep a live build safe. A target
    /// touched yesterday is state someone is plausibly still iterating on, and rebuilding it costs
    /// them minutes. A week of silence is a much better signal that nobody wants it, so the
    /// default is tuned for not surprising anyone rather than for reclaiming the most bytes.
    const DEFAULT_IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;

    #[derive(Debug, Parser)]
    #[command(
        name = "glaeda-cargo-target-reclaim",
        about = "Reclaim one checkout-local Cargo target that is idle and reconstructible"
    )]
    struct Cli {
        /// Explicit canonical absolute Git checkout root.
        #[arg(long)]
        checkout: PathBuf,

        /// Delete the target. Without it this reports the decision and changes nothing.
        #[arg(long)]
        apply: bool,

        /// Seconds a target must be untouched before it may be reclaimed.
        #[arg(long, default_value_t = DEFAULT_IDLE_SECONDS)]
        minimum_idle_seconds: i64,

        /// Select human or JSON output.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
    }

    #[derive(Debug, Clone, Copy, ValueEnum)]
    enum OutputFormat {
        Human,
        Json,
    }

    #[derive(Debug, Serialize)]
    struct PlanReport {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        compensation: &'static str,
        mutation_performed: bool,
        decision: CargoTargetReclaimDecision,
    }

    #[derive(Debug, Serialize)]
    struct ApplyReport {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        compensation: &'static str,
        #[serde(flatten)]
        receipt: glaeda::cargo_target_reclaim::CargoTargetReclaimReceipt,
    }

    const AUTHORITY: &str = "reconstructible_from_checkout";
    // Deletion cannot be rolled back. Naming the compensation accurately is the point: the tree
    // comes back by being rebuilt, and that costs time rather than truth.
    const COMPENSATION: &str = "cold_reconstruction_by_cargo_rebuild";

    pub fn main() -> ExitCode {
        let cli = Cli::parse();
        let policy = match CargoTargetReclaimPolicy::new(cli.minimum_idle_seconds) {
            Ok(policy) => policy,
            Err(error) => return fail(&cli.output, error.code(), error.problem()),
        };
        let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
            return fail(
                &cli.output,
                "clock_unavailable",
                "the system clock is before the epoch",
            );
        };
        let Ok(now_seconds) = i64::try_from(now.as_secs()) else {
            return fail(
                &cli.output,
                "clock_unavailable",
                "the system clock does not fit i64",
            );
        };

        if cli.apply {
            return match reclaim_cargo_target(&cli.checkout, policy, now_seconds) {
                Ok(receipt) => {
                    let reclaimed = matches!(
                        receipt.outcome(),
                        CargoTargetReclaimOutcome::Reclaimed { .. }
                    );
                    emit_apply(&cli.output, receipt);
                    if reclaimed {
                        ExitCode::SUCCESS
                    } else {
                        // Refused and incomplete are both ordinary, but a caller sweeping many
                        // checkouts should be able to tell them from a completed reclaim.
                        ExitCode::from(3)
                    }
                }
                Err(error) => fail(&cli.output, error.code(), error.problem()),
            };
        }

        let observation = match observe_cargo_target(&cli.checkout) {
            Ok(observation) => observation,
            Err(error) => return fail(&cli.output, error.code(), error.problem()),
        };
        let holders = match observe_cargo_target_holders(&cli.checkout) {
            Ok(holders) => holders,
            Err(error) => return fail(&cli.output, error.code(), error.problem()),
        };
        match plan_cargo_target_reclaim(&observation, &holders, policy, now_seconds) {
            Ok(plan) => {
                let eligible = plan.decision().is_eligible();
                emit_plan(&cli.output, plan.decision().clone());
                if eligible {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(3)
                }
            }
            Err(error) => fail(&cli.output, error.code(), error.problem()),
        }
    }

    fn emit_plan(output: &OutputFormat, decision: CargoTargetReclaimDecision) {
        let report = PlanReport {
            document_type: "glaeda-cargo-target-reclaim-plan",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: AUTHORITY,
            compensation: COMPENSATION,
            mutation_performed: false,
            decision,
        };
        match output {
            OutputFormat::Json => print_json(&report),
            OutputFormat::Human => match &report.decision {
                CargoTargetReclaimDecision::Eligible {
                    allocated_bytes,
                    entry_count,
                    idle_seconds,
                    released_bytes_are_upper_bound,
                    ..
                } => {
                    println!(
                        "eligible: {allocated_bytes} bytes across {entry_count} entries, idle {idle_seconds}s{}",
                        if *released_bytes_are_upper_bound {
                            " (bytes are an upper bound: external hardlinks)"
                        } else {
                            ""
                        }
                    );
                    println!("run again with --apply to reclaim it");
                }
                CargoTargetReclaimDecision::Refused { vetoes } => {
                    println!("refused: {}", join_vetoes(vetoes));
                }
            },
        }
    }

    fn emit_apply(
        output: &OutputFormat,
        receipt: glaeda::cargo_target_reclaim::CargoTargetReclaimReceipt,
    ) {
        let resumed = receipt.resumed_entries_removed();
        let report = ApplyReport {
            document_type: "glaeda-cargo-target-reclaim-receipt",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: AUTHORITY,
            compensation: COMPENSATION,
            receipt,
        };
        match output {
            OutputFormat::Json => print_json(&report),
            OutputFormat::Human => {
                if resumed > 0 {
                    println!("resumed an interrupted pass: {resumed} entries removed");
                }
                match report.receipt.outcome() {
                    CargoTargetReclaimOutcome::Reclaimed {
                        entries_removed,
                        released_bytes,
                        released_bytes_are_upper_bound,
                    } => println!(
                        "reclaimed: {released_bytes} bytes across {entries_removed} entries{}",
                        if *released_bytes_are_upper_bound {
                            " (upper bound: external hardlinks)"
                        } else {
                            ""
                        }
                    ),
                    CargoTargetReclaimOutcome::Incomplete { entries_removed } => println!(
                        "incomplete: {entries_removed} entries removed; run again to finish"
                    ),
                    CargoTargetReclaimOutcome::Refused { vetoes } => {
                        println!("refused: {}", join_vetoes(vetoes));
                    }
                }
            }
        }
    }

    fn join_vetoes(vetoes: &[glaeda::cargo_target_reclaim::CargoTargetReclaimVeto]) -> String {
        vetoes
            .iter()
            .map(|veto| veto.code())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn print_json<T: Serialize>(report: &T) {
        match serde_json::to_string_pretty(report) {
            Ok(text) => println!("{text}"),
            Err(_) => eprintln!("report could not be serialized"),
        }
    }

    fn fail(output: &OutputFormat, code: &str, problem: &str) -> ExitCode {
        match output {
            OutputFormat::Json => println!(
                "{{\"document_type\":\"glaeda-cargo-target-reclaim-error\",\"code\":\"{code}\",\"problem\":\"{problem}\"}}"
            ),
            OutputFormat::Human => eprintln!("{code}: {problem}"),
        }
        ExitCode::from(2)
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}
