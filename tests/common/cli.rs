use super::Workspace;
use std::{
    ffi::OsString,
    process::{Command, Output},
};

/// Executes the compiled `acdd` binary against `workspace`.
pub fn run_cli(workspace: &Workspace, args: &[&str]) -> Output {
    super::isolate_acdd_env(
        Command::new(env!("CARGO_BIN_EXE_acdd"))
            .arg("--root")
            .arg(workspace.root())
            .env(
                "ACDD_EXTRACTION_CACHE_DIR",
                workspace.path(".acdd/extraction-cache"),
            )
            .args(args),
    )
    .output()
    .expect("could not run acdd")
}

pub fn run_cli_json(workspace: &Workspace, args: &[&str]) -> Output {
    let mut json_args = vec!["--format", "json"];
    json_args.extend_from_slice(args);
    run_cli(workspace, &json_args)
}

pub fn run_cli_args(args: &[OsString]) -> Output {
    super::isolate_acdd_env(Command::new(env!("CARGO_BIN_EXE_acdd")).args(args))
        .output()
        .expect("could not run acdd")
}
