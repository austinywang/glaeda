#[cfg(not(unix))]
compile_error!("glaeda-project-observe requires a Unix host");

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use glaeda::process::ProcessExecutor;
use glaeda::project_checkout_observation::{
    ProjectBranchState, ProjectCheckoutObservation, ProjectCheckoutObservationError,
    ProjectCheckoutObserver,
};
use serde::Serialize;

const REPORT_SCHEMA_VERSION: u8 = 1;
const GIT_PROGRAM: &str = "/usr/bin/git";

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-project-observe",
    about = "Observe one exact developer checkout without mutation or network access"
)]
struct Cli {
    /// Explicit canonical absolute checkout root.
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
struct ProjectObservationReport {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    observation: ProjectCheckoutObservation,
}

impl ProjectObservationReport {
    fn new(observation: ProjectCheckoutObservation) -> Self {
        Self {
            document_type: "glaeda-project-observation",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: "observation_only",
            observation,
        }
    }

    fn render_human(&self) -> String {
        let project = self
            .observation
            .primary_project()
            .map_or("unknown", |project| project.as_str());
        let source = if self.observation.source_ambiguous() {
            "ambiguous"
        } else if self.observation.primary_project().is_some() {
            "canonical_github"
        } else {
            "unknown"
        };
        let branch = match self.observation.branch() {
            ProjectBranchState::Attached { name } => name.as_str(),
            ProjectBranchState::Detached => "detached",
        };
        let ahead = self
            .observation
            .local_commits_ahead()
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string());

        format!(
            concat!(
                "project observation: authority={} source={} project={}\n",
                "commit: {}\n",
                "tree: {}\n",
                "branch: {}\n",
                "working state: tracked_changes={} untracked_entries={} ",
                "upstream_configured={} local_commits_ahead={}\n",
                "topology: linked_worktrees={} submodules={} owner_matches_parent={}\n",
                "materialization: {}\n"
            ),
            self.authority,
            source,
            project,
            self.observation.commit().as_str(),
            self.observation.tree().as_str(),
            branch,
            self.observation.tracked_changes_present(),
            self.observation.untracked_entry_count(),
            self.observation.upstream_configured(),
            ahead,
            self.observation.linked_worktree_count(),
            self.observation.submodules_present(),
            self.observation.owner_matches_parent(),
            self.observation.materialization_id().as_str(),
        )
    }
}

#[derive(Debug, Serialize)]
struct ProjectObservationErrorReport<'a> {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    error: &'a ProjectCheckoutObservationError,
}

impl<'a> ProjectObservationErrorReport<'a> {
    fn new(error: &'a ProjectCheckoutObservationError) -> Self {
        Self {
            document_type: "glaeda-project-observation-error",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: "observation_only",
            error,
        }
    }

    fn render_human(&self) -> String {
        format!(
            "project observation unavailable: code={} problem={}\n",
            self.error.code, self.error.problem
        )
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let observer = match ProjectCheckoutObserver::new(GIT_PROGRAM) {
        Ok(observer) => observer,
        Err(error) => return emit_error(cli.output, &error),
    };
    match observer.observe(&cli.checkout, &ProcessExecutor) {
        Ok(observation) => emit_report(cli.output, ProjectObservationReport::new(observation)),
        Err(error) => emit_error(cli.output, &error),
    }
}

fn emit_report(output: OutputFormat, report: ProjectObservationReport) -> ExitCode {
    match output {
        OutputFormat::Human => print!("{}", report.render_human()),
        OutputFormat::Json => match serde_json::to_string(&report) {
            Ok(json) => println!("{json}"),
            Err(_) => {
                eprintln!("project observation could not be encoded");
                return ExitCode::from(2);
            }
        },
    }
    ExitCode::SUCCESS
}

fn emit_error(output: OutputFormat, error: &ProjectCheckoutObservationError) -> ExitCode {
    let report = ProjectObservationErrorReport::new(error);
    match output {
        OutputFormat::Human => eprint!("{}", report.render_human()),
        OutputFormat::Json => match serde_json::to_string(&report) {
            Ok(json) => eprintln!("{json}"),
            Err(_) => eprintln!("project observation error could not be encoded"),
        },
    }
    ExitCode::from(2)
}
