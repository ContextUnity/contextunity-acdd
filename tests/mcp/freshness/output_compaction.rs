use super::*;

#[test]
fn overview_markdown_reports_workspace_inventory() {
    let mut client = McpTestHarness::indexed(&[("src/lib.rs", "pub fn sample() {}\n")]).unwrap();
    let response = client.text("code_map_overview", json!({})).unwrap();
    assert!(response.contains("Workspace"));
    client.finish().unwrap();
}
