#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("Cargo target holder observation requires Linux");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::process::ExitCode;

    use clap::{Parser, ValueEnum};
    use glaeda::cargo_target_holder_observation::{
        CargoTargetHolderObservation, CargoTargetHolderState, observe_cargo_target_holders,
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
        name = "glaeda-cargo-target-holders",
        about = "Observe positive Linux references to one checkout-local Cargo target"
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
    struct CargoTargetHolderReport {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        zero_means: &'static str,
        mutation_performed: bool,
        checkout: ProjectCheckoutObservation,
        holders: CargoTargetHolderObservation,
    }

    impl CargoTargetHolderReport {
        fn new(
            checkout: ProjectCheckoutObservation,
            holders: CargoTargetHolderObservation,
        ) -> Self {
            Self {
                document_type: "glaeda-cargo-target-holder-observation",
                schema_version: REPORT_SCHEMA_VERSION,
                authority: "positive_observation_only",
                zero_means: "none_observed_not_absence",
                mutation_performed: false,
                checkout,
                holders,
            }
        }

        fn render_human(&self) -> String {
            match self.holders.state() {
                CargoTargetHolderState::Absent => concat!(
                    "Cargo target holders: state=absent authority=positive_observation_only\n",
                    "zero_means=none_observed_not_absence mutation_performed=false\n"
                )
                .to_owned(),
                CargoTargetHolderState::Present {
                    target_id,
                    disposition,
                    counts,
                    coverage,
                } => format!(
                    concat!(
                        "Cargo target holders: state=present disposition={:?} ",
                        "authority=positive_observation_only\n",
                        "target_id={} holder_processes={} open_fd_references={} ",
                        "mount_references={}\n",
                        "processes_started={} processes_incomplete={} process_table_rescan_equal={} ",
                        "universal_absence_proven=false\n",
                        "zero_means=none_observed_not_absence mutation_performed=false\n"
                    ),
                    disposition,
                    target_id.as_str(),
                    counts.holder_processes(),
                    counts.open_fd_references(),
                    counts.mount_references(),
                    coverage.process_entries_started(),
                    coverage.process_entries_incomplete(),
                    coverage.process_table_rescan_equal(),
                ),
            }
        }
    }

    #[derive(Debug, Serialize)]
    struct CargoTargetHolderErrorReport<'a> {
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
        let holders = match observe_cargo_target_holders(&cli.checkout) {
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
                "cargo_target_holder_checkout_changed",
                "checkout changed during Cargo target holder observation",
            );
        }
        emit_report(
            cli.output,
            CargoTargetHolderReport::new(checkout_after, holders),
        )
    }

    fn emit_report(output: OutputFormat, report: CargoTargetHolderReport) -> ExitCode {
        match output {
            OutputFormat::Human => print!("{}", report.render_human()),
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(json) => println!("{json}"),
                Err(_) => {
                    eprintln!("Cargo target holder observation could not be encoded");
                    return ExitCode::from(2);
                }
            },
        }
        ExitCode::SUCCESS
    }

    fn emit_error(output: OutputFormat, code: &str, problem: &str) -> ExitCode {
        let report = CargoTargetHolderErrorReport {
            document_type: "glaeda-cargo-target-holder-observation-error",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: "positive_observation_only",
            code,
            problem,
            mutation_performed: false,
        };
        match output {
            OutputFormat::Human => {
                eprintln!("Cargo target holder observation unavailable: {problem}")
            }
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(json) => eprintln!("{json}"),
                Err(_) => eprintln!("Cargo target holder observation error could not be encoded"),
            },
        }
        ExitCode::from(2)
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}
