use super::*;

#[test]
fn scoped_selector_resolves_matching_symbol() {
    let conn = search_and_paging::search_workspace().open();
    let result = reader::inspect_paged_with_path(
        &conn,
        "admit",
        Some("pkg/service.py"),
        false,
        &options(10),
    )
    .unwrap();
    assert_eq!(result["node"]["name"], "admit");
}
