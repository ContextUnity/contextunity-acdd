use crate::cli;
use crate::common::Workspace;

#[test]
fn real_cli_preserves_captured_output_parser_errors_and_process_exit_status() {
    let workspace = Workspace::with_files(&[
        ("acdd.yaml", "preset: minimal\n"),
        ("notes.txt", "hello\n"),
    ]);
    for args in [
        vec!["--format", "json", "read", "notes.txt"],
        vec!["read", "../notes.txt"],
        vec!["--format", "invalid", "read", "notes.txt"],
    ] {
        let captured = cli::run_cli(&workspace, &args);
        let process = crate::common::mcp_client::run_cli(&workspace, &args);
        assert_eq!(captured.status.code(), process.status.code(), "{args:?}");
        assert_eq!(captured.stdout, process.stdout, "{args:?}");
        assert_eq!(captured.stderr, process.stderr, "{args:?}");
    }
}
