use super::*;

#[test]
fn exact_search_retains_the_matching_symbol() {
    let conn = search_and_paging::search_workspace().open();
    let result = symbols::search_with_options(
        &conn,
        "admit",
        &symbols::SearchOptions {
            kind: Some("method"),
            path: Some("pkg"),
            include_docs: false,
            exact: true,
            classification: None,
            page: &options(10),
        },
    )
    .unwrap();
    assert_eq!(result["nodes"]["items"][0]["name"], "admit");
}
