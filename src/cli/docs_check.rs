use super::OutputFormat;
use anyhow::{bail, Result};
use std::path::Path;

pub(super) fn check(
    root: &Path,
    format: OutputFormat,
    output: &mut dyn std::io::Write,
) -> Result<()> {
    let result = check_links(root);
    match (format, result) {
        (OutputFormat::Json, Ok(count)) => {
            writeln!(
                output,
                "{}",
                serde_json::json!({"status": "ok", "files_checked": count})
            )
            .unwrap_or_else(|error| panic!("failed printing to stdout: {error}"));
            Ok(())
        }
        (OutputFormat::Json, Err(error)) => {
            writeln!(
                output,
                "{}",
                serde_json::json!({"status": "error", "errors": format!("{error:#}").lines().collect::<Vec<_>>()})
            )
            .unwrap_or_else(|error| panic!("failed printing to stdout: {error}"));
            bail!("documentation check failed")
        }
        (_, Ok(_)) => {
            writeln!(output, "All Markdown links and anchors resolve.")
                .unwrap_or_else(|error| panic!("failed printing to stdout: {error}"));
            Ok(())
        }
        (_, Err(error)) => Err(error),
    }
}

fn check_links(root: &Path) -> Result<usize> {
    let (files_checked, errors) = crate::engine::docs_check::collect_link_errors(root, None)?;
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }
    Ok(files_checked)
}
