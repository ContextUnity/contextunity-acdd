use std::io::{self, Write};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match contextunity_acdd::cli::run_from_env().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
