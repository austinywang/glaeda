#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("Cargo target observation requires Linux");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::process::ExitCode;

    use clap::{Parser, ValueEnum};
    use glaeda::cargo_target_observation::{
        CargoTargetObservation, CargoTargetState, observe_cargo_target,
    };
    use glaeda::process::ProcessExecutor;
    use glaeda::project_checkout_observation::{
        ProjectCheckoutObservation, ProjectCheckoutObserver,
    };
    use serde::Serialize;

    const REPORT_SCHEMA_VERSION: u8 = 1;
    const GIT_PROGRAM: &str = "/usr/bin/git";

    #[derive(Debug, Parser)]
    #[command(
        name = "glaeda-cargo-target-observe",
        about = "Observe one checkout-local Cargo target without mutation"
    )]
    struct Cli {
        /// Explicit canonical absolute Git checkout root.
        #[arg(long)]
        checkout: PathBuf,

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
    struct CargoTargetReport {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        activity_evidence: &'static str,
        successful_use_evidence: &'static str,
        rebuild_cost_evidence: &'static str,
        retention_value: &'static str,
        mutation_performed: bool,
        checkout: ProjectCheckoutObservation,
        target: CargoTargetObservation,
    }

    impl CargoTargetReport {
        fn new(checkout: ProjectCheckoutObservation, target: CargoTargetObservation) -> Self {
            Self {
                document_type: "glaeda-cargo-target-observation",
                schema_version: REPORT_SCHEMA_VERSION,
                authority: "observation_only",
                activity_evidence: "unknown",
                successful_use_evidence: "unknown",
                rebuild_cost_evidence: "unknown",
                retention_value: "unknown",
                mutation_performed: false,
                checkout,
                target,
            }
        }

        fn render_human(&self) -> String {
            match self.target.state() {
                CargoTargetState::Absent => format!(
                    "Cargo target: state=absent authority={} mutation_performed=false\n",
                    self.authority
                ),
                CargoTargetState::Present {
                    target_id,
                    entry_count,
                    allocated_bytes,
                    hardlink_coverage,
                    ..
                } => format!(
                    concat!(
                        "Cargo target: state=present authority={} mutation_performed=false\n",
                        "target_id={} entries={} allocated_bytes={} hardlink_coverage={:?}\n",
                        "activity=unknown successful_use=unknown rebuild_cost=unknown retention_value=unknown\n"
                    ),
                    self.authority,
                    target_id.as_str(),
                    entry_count,
                    allocated_bytes,
                    hardlink_coverage,
                ),
            }
        }
    }

    #[derive(Debug, Serialize)]
    struct CargoTargetErrorReport<'a> {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        code: &'a str,
        problem: &'a str,
        mutation_performed: bool,
    }

    pub fn main() -> ExitCode {
        let cli = Cli::parse();
        let observer = match ProjectCheckoutObserver::new(GIT_PROGRAM) {
            Ok(observer) => observer,
            Err(error) => return emit_error(cli.output, error.code, error.problem),
        };
        let checkout_before = match observer.observe(&cli.checkout, &ProcessExecutor) {
            Ok(observation) => observation,
            Err(error) => return emit_error(cli.output, error.code, error.problem),
        };
        let target = match observe_cargo_target(&cli.checkout) {
            Ok(observation) => observation,
            Err(error) => return emit_error(cli.output, error.code(), error.problem()),
        };
        let checkout_after = match observer.observe(&cli.checkout, &ProcessExecutor) {
            Ok(observation) => observation,
            Err(error) => return emit_error(cli.output, error.code, error.problem),
        };
        if checkout_before != checkout_after {
            return emit_error(
                cli.output,
                "cargo_target_checkout_changed",
                "checkout changed during Cargo target observation",
            );
        }
        emit_report(cli.output, CargoTargetReport::new(checkout_after, target))
    }

    fn emit_report(output: OutputFormat, report: CargoTargetReport) -> ExitCode {
        match output {
            OutputFormat::Human => print!("{}", report.render_human()),
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(json) => println!("{json}"),
                Err(_) => {
                    eprintln!("Cargo target observation could not be encoded");
                    return ExitCode::from(2);
                }
            },
        }
        ExitCode::SUCCESS
    }

    fn emit_error(output: OutputFormat, code: &str, problem: &str) -> ExitCode {
        let report = CargoTargetErrorReport {
            document_type: "glaeda-cargo-target-observation-error",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: "observation_only",
            code,
            problem,
            mutation_performed: false,
        };
        match output {
            OutputFormat::Human => eprintln!("Cargo target observation unavailable: {problem}"),
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(json) => eprintln!("{json}"),
                Err(_) => eprintln!("Cargo target observation error could not be encoded"),
            },
        }
        ExitCode::from(2)
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}
