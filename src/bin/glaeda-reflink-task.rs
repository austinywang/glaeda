#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::process::ExitCode;

    use clap::{Parser, ValueEnum};
    use glaeda::linux_reflink_task_materialization::{
        ReflinkTaskFanoutRequest, ReflinkTaskMaterializationMode,
        ReflinkTaskMaterializationRequest, materialize_reflink_task,
        materialize_reflink_task_fanout, render_reflink_task_fanout_human,
        render_reflink_task_materialization_human,
    };

    #[derive(Debug, Parser)]
    #[command(
        name = "glaeda-reflink-task",
        about = "Research exact same-HEAD Linux reflink task materialization"
    )]
    struct Cli {
        #[arg(long, default_value = "/usr/bin/git")]
        git: PathBuf,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        target: Vec<PathBuf>,
        #[arg(
            long,
            help = "emit the fan-out contract even when exactly one target is requested"
        )]
        fanout: bool,
        #[arg(long)]
        commit: String,
        #[arg(long, value_enum)]
        mode: Mode,
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
    }

    #[derive(Debug, Clone, Copy, ValueEnum)]
    enum Mode {
        Ordinary,
        Reflink,
    }

    #[derive(Debug, Clone, Copy, ValueEnum)]
    enum OutputFormat {
        Human,
        Json,
    }

    pub fn main() -> ExitCode {
        let cli = Cli::parse();
        let mode = match cli.mode {
            Mode::Ordinary => ReflinkTaskMaterializationMode::Ordinary,
            Mode::Reflink => ReflinkTaskMaterializationMode::ReflinkWithFallback,
        };
        if cli.fanout || cli.target.len() > 1 {
            return run_fanout(cli, mode);
        }
        let target = match cli.target.into_iter().next() {
            Some(target) => target,
            None => {
                eprintln!("glaeda-reflink-task error: reflink_task_request_invalid");
                return ExitCode::from(2);
            }
        };
        let request = match ReflinkTaskMaterializationRequest::new(
            cli.git, cli.source, target, cli.commit, mode,
        ) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("glaeda-reflink-task error: {}", error.code());
                return ExitCode::from(2);
            }
        };
        let report = match materialize_reflink_task(&request) {
            Ok(report) => report,
            Err(error) => {
                eprintln!("glaeda-reflink-task error: {}", error.code());
                return ExitCode::from(2);
            }
        };
        match cli.output {
            OutputFormat::Human => print!("{}", render_reflink_task_materialization_human(&report)),
            OutputFormat::Json => match serde_json::to_writer_pretty(std::io::stdout(), &report) {
                Ok(()) => println!(),
                Err(_) => return ExitCode::from(2),
            },
        }
        ExitCode::SUCCESS
    }

    fn run_fanout(cli: Cli, mode: ReflinkTaskMaterializationMode) -> ExitCode {
        let request = match ReflinkTaskFanoutRequest::new(
            cli.git, cli.source, cli.target, cli.commit, mode,
        ) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("glaeda-reflink-task error: {}", error.code());
                return ExitCode::from(2);
            }
        };
        let report = match materialize_reflink_task_fanout(&request) {
            Ok(report) => report,
            Err(error) => {
                eprintln!("glaeda-reflink-task error: {}", error.code());
                return ExitCode::from(2);
            }
        };
        match cli.output {
            OutputFormat::Human => print!("{}", render_reflink_task_fanout_human(&report)),
            OutputFormat::Json => match serde_json::to_writer_pretty(std::io::stdout(), &report) {
                Ok(()) => println!(),
                Err(_) => return ExitCode::from(2),
            },
        }
        ExitCode::SUCCESS
    }

    #[cfg(test)]
    mod tests {
        use super::Cli;
        use clap::Parser;

        fn parse(arguments: &[&str]) -> Cli {
            Cli::try_parse_from(arguments).expect("fixture CLI should parse")
        }

        #[test]
        fn explicit_fanout_preserves_the_fanout_contract_for_one_target() {
            let cli = parse(&[
                "glaeda-reflink-task",
                "--source",
                "/source",
                "--target",
                "/task",
                "--fanout",
                "--commit",
                "0123456789012345678901234567890123456789",
                "--mode",
                "ordinary",
            ]);

            assert!(cli.fanout);
            assert_eq!(cli.target.len(), 1);
        }

        #[test]
        fn legacy_single_target_selection_remains_distinct() {
            let cli = parse(&[
                "glaeda-reflink-task",
                "--source",
                "/source",
                "--target",
                "/task",
                "--commit",
                "0123456789012345678901234567890123456789",
                "--mode",
                "ordinary",
            ]);

            assert!(!cli.fanout);
            assert_eq!(cli.target.len(), 1);
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("glaeda-reflink-task is available only on Linux");
    std::process::ExitCode::from(2)
}
