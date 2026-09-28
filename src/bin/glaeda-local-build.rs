use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use glaeda::artifact::{CommitId, GitTreeId, Sha256Digest};
use glaeda::local_install_build_command::LocalInstallBuildCommandContext;
use glaeda::local_install_build_execution::{
    LocalInstallBuildExecutionOutcome, execute_local_install_build,
};
use glaeda::local_install_plan::{
    LocalInstallBuildPlan, LocalInstallGenerationIdentity, LocalInstallPlatform,
    LocalInstallSourceIdentity, LocalInstallToolchainIdentity,
};
use glaeda::process::ProcessExecutor;
use glaeda::project_checkout_observation::ProjectCheckoutObserver;

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-local-build",
    about = "Build one exact Glaeda source identity into path-private artifact evidence"
)]
struct Arguments {
    #[arg(long)]
    source_root: PathBuf,
    #[arg(long)]
    build_root: PathBuf,
    #[arg(long)]
    commit: String,
    #[arg(long)]
    tree: String,
    #[arg(long)]
    cargo_lock_digest: String,
    #[arg(long)]
    toolchain: String,
    #[arg(long)]
    cargo: PathBuf,
    #[arg(long)]
    rustc: PathBuf,
    #[arg(long)]
    rustdoc: PathBuf,
    #[arg(long, default_value_t = 4)]
    jobs: u8,
    #[arg(long, default_value_t = 1)]
    target_generation: u64,
    #[arg(long, requires = "predecessor_digest")]
    predecessor_number: Option<u64>,
    #[arg(long, requires = "predecessor_number")]
    predecessor_digest: Option<String>,
}

fn main() -> ExitCode {
    match run(Arguments::parse()) {
        Ok((document, succeeded)) => {
            println!("{document}");
            if succeeded {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
        }
        Err(problem) => {
            eprintln!("glaeda-local-build: {problem}");
            ExitCode::from(2)
        }
    }
}

fn run(arguments: Arguments) -> Result<(String, bool), &'static str> {
    let source = LocalInstallSourceIdentity::new(
        CommitId::parse(&arguments.commit).map_err(|_| "commit identity is invalid")?,
        GitTreeId::parse(&arguments.tree).map_err(|_| "tree identity is invalid")?,
        Sha256Digest::parse(&arguments.cargo_lock_digest)
            .map_err(|_| "Cargo.lock digest is invalid")?,
        LocalInstallToolchainIdentity::parse(&arguments.toolchain)
            .map_err(|_| "toolchain identity is invalid")?,
    )
    .map_err(|_| "source identity could not be encoded")?;
    let expected_predecessor = match (arguments.predecessor_number, arguments.predecessor_digest) {
        (Some(number), Some(digest)) => Some(LocalInstallGenerationIdentity {
            number,
            digest: Sha256Digest::parse(&digest).map_err(|_| "predecessor digest is invalid")?,
        }),
        (None, None) => None,
        _ => return Err("predecessor identity is incomplete"),
    };
    let plan = LocalInstallBuildPlan {
        target_generation: arguments.target_generation,
        expected_predecessor,
        source,
    };
    let context = LocalInstallBuildCommandContext::new(
        arguments.source_root,
        arguments.build_root,
        arguments.cargo,
        arguments.rustc,
        arguments.rustdoc,
    )
    .map_err(|_| "private build context is invalid")?;
    let observer = ProjectCheckoutObserver::new("/usr/bin/git")
        .map_err(|_| "fixed Git observer is unavailable")?;
    let platform = if cfg!(target_os = "macos") {
        LocalInstallPlatform::Macos
    } else if cfg!(target_os = "linux") {
        LocalInstallPlatform::Linux
    } else {
        return Err("host platform is unsupported");
    };
    let receipt = execute_local_install_build(
        &plan,
        platform,
        &context,
        arguments.jobs,
        &observer,
        &ProcessExecutor,
    )
    .map_err(|_| "build execution input is invalid")?;
    let succeeded = receipt.outcome() == LocalInstallBuildExecutionOutcome::Succeeded;
    let document =
        serde_json::to_string_pretty(&receipt).map_err(|_| "build receipt could not be encoded")?;
    Ok((document, succeeded))
}
