use crate::format::OutputFormat;
use anyhow::{bail, Result};
use std::path::Path;

fn write_line(output: &mut dyn std::io::Write, line: &str) -> Result<()> {
    match writeln!(output, "{line}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn check(
    root: &Path,
    format: OutputFormat,
    output: &mut dyn std::io::Write,
) -> Result<()> {
    let result = check_links(root);
    match (format, result) {
        (OutputFormat::Json, Ok(count)) => {
            write_line(
                output,
                &serde_json::json!({"status": "ok", "files_checked": count}).to_string(),
            )?;
            Ok(())
        }
        (OutputFormat::Json, Err(error)) => {
            write_line(
                output,
                &serde_json::json!({"status": "error", "errors": format!("{error:#}").lines().collect::<Vec<_>>()}).to_string(),
            )?;
            bail!("documentation check failed")
        }
        (_, Ok(_)) => {
            write_line(output, "All Markdown links and anchors resolve.")?;
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
