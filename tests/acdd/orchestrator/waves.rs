use clap::Parser;
use contextunity_acdd::cli::Cli;
use serde_json::Value;

mod blueprints;
mod cancellation;
mod command_proofs;
mod configuration;
mod dag_state;
mod escalation;
mod handoff;
mod harness;
mod isolation;
mod plans;
mod recovery;
mod root_fixture;
mod root_gates;
mod scheduling;
mod snapshots;
mod workflows;

fn run_cli(root: &std::path::Path, arguments: &[&str]) -> anyhow::Result<Value> {
    let cli = Cli::try_parse_from(
        [
            "contextunity-acdd",
            "--root",
            root.to_str().unwrap(),
            "--format",
            "json",
        ]
        .into_iter()
        .chain(arguments.iter().copied()),
    )?;
    let mut output = Vec::new();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(cli.run_with_writer(&mut output))?;
    Ok(serde_json::from_slice(&output)?)
}
