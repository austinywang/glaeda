#[cfg(not(unix))]
compile_error!("glaeda-repo-query requires a Unix host");

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use glaeda::artifact::{CommitId, GitTreeId};
use glaeda::process::ProcessExecutor;
use glaeda::project_catalog::ProjectIdentity;
use glaeda::resident_repo_query::{
    DEFAULT_PATCH_BYTES, MAX_BLOB_BYTES, MAX_GREP_MATCHES, MAX_HISTORY_COMMITS,
    ResidentRepoQueryError, ResidentRepoQueryObserver, ResidentRepoQueryRequest,
};
use serde::Serialize;

const GIT_PROGRAM: &str = "/usr/bin/git";

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-repo-query",
    about = "Return one bounded exact-OID review bundle from a resident Git checkout"
)]
struct Cli {
    /// Explicit canonical absolute checkout root.
    #[arg(long)]
    checkout: PathBuf,

    /// Canonical github.com/owner/repository identity expected at origin.
    #[arg(long)]
    project: String,

    /// Complete lowercase base commit object ID.
    #[arg(long)]
    base: String,

    /// Complete lowercase candidate commit object ID.
    #[arg(long)]
    head: String,

    /// Complete lowercase tree object ID expected for the candidate commit.
    #[arg(long)]
    tree: String,

    /// Include the complete patch only when it fits this byte ceiling.
    #[arg(long, default_value_t = DEFAULT_PATCH_BYTES)]
    max_patch_bytes: usize,

    /// One literal string to search in the exact candidate tree.
    #[arg(long)]
    grep_literal: Option<String>,

    /// Optional literal repository-relative scope for the exact-tree grep; repeatable.
    #[arg(long = "grep-path", requires = "grep_literal")]
    grep_paths: Vec<String>,

    /// Maximum aggregate exact-tree grep matches returned.
    #[arg(long, default_value_t = MAX_GREP_MATCHES)]
    max_grep_matches: usize,

    /// Literal exact-candidate-tree blob path to read; repeatable.
    #[arg(long = "blob")]
    blob_paths: Vec<String>,

    /// Maximum bytes returned per requested blob.
    #[arg(long, default_value_t = MAX_BLOB_BYTES)]
    max_blob_bytes: usize,

    /// Literal path whose exact-candidate history should be returned; repeatable.
    #[arg(long = "history")]
    history_paths: Vec<String>,

    /// Maximum commits returned per requested history path.
    #[arg(long, default_value_t = MAX_HISTORY_COMMITS)]
    max_history_commits: usize,

    /// Complete exact Git object ID whose existence, type, and size should be returned; repeatable.
    #[arg(long = "object")]
    object_oids: Vec<String>,

    /// Select compact JSON or human output.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    output: OutputFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Human,
}

#[derive(Debug, Serialize)]
struct ErrorReport<'a> {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    error: &'a ResidentRepoQueryError,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(report) => match cli.output {
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(json) => println!("{json}"),
                Err(_) => return encoding_error(),
            },
            OutputFormat::Human => print!("{}", report.render_human()),
        },
        Err(error) => return emit_error(&error),
    }
    ExitCode::SUCCESS
}

fn run(
    cli: &Cli,
) -> Result<glaeda::resident_repo_query::ResidentRepoQueryReport, ResidentRepoQueryError> {
    let project = ProjectIdentity::parse(&cli.project).map_err(|_| cli_input_error())?;
    let base = CommitId::parse(&cli.base).map_err(|_| cli_input_error())?;
    let head = CommitId::parse(&cli.head).map_err(|_| cli_input_error())?;
    let tree = GitTreeId::parse(&cli.tree).map_err(|_| cli_input_error())?;
    let mut request =
        ResidentRepoQueryRequest::new(project, base, head, tree, cli.max_patch_bytes)?;
    if let Some(literal) = &cli.grep_literal {
        request = request.with_exact_tree_grep(
            literal.clone(),
            cli.grep_paths.clone(),
            cli.max_grep_matches,
        )?;
    }
    if !cli.blob_paths.is_empty() {
        request = request.with_blob_reads(cli.blob_paths.clone(), cli.max_blob_bytes)?;
    }
    if !cli.history_paths.is_empty() {
        request = request.with_path_history(cli.history_paths.clone(), cli.max_history_commits)?;
    }
    if !cli.object_oids.is_empty() {
        request = request.with_object_info(cli.object_oids.clone())?;
    }
    let observer = ResidentRepoQueryObserver::new(GIT_PROGRAM)?;
    observer.observe(&cli.checkout, &request, &ProcessExecutor)
}

fn emit_error(error: &ResidentRepoQueryError) -> ExitCode {
    let report = ErrorReport {
        document_type: "glaeda-resident-repo-query-error",
        schema_version: 1,
        authority: "observation_only",
        error,
    };
    match serde_json::to_string(&report) {
        Ok(json) => eprintln!("{json}"),
        Err(_) => eprintln!("repo query error could not be encoded"),
    }
    ExitCode::from(2)
}

fn encoding_error() -> ExitCode {
    eprintln!("repo query result could not be encoded");
    ExitCode::from(2)
}

const fn cli_input_error() -> ResidentRepoQueryError {
    ResidentRepoQueryError {
        code: "invalid_cli_input",
        problem: "repo query CLI input is invalid",
    }
}
