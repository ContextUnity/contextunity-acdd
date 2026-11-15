use super::*;

const PRESERVED_NOTE: &str = r#"{"milestone_passports":{"forge-mcp/other/m-close":{"retained":true}},"external_note":"preserved"}"#;

mod basics;
mod foreign_archive;
mod opt_out;
mod retained_cancellation;

#[test]
fn control_plane_activation_matches_acdd_paths() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let adapter_text = std::fs::read_to_string(repo_root.join("acdd.yaml"))
        .expect("acdd.yaml must be activated in repository root");
    let adapter: serde_yaml::Value = serde_yaml::from_str(&adapter_text).unwrap();
    assert_eq!(adapter["acdd_profile"].as_str(), Some(".acdd/profile.yaml"));
    assert!(
        adapter["ignore"]
            .as_sequence()
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(".acdd"))),
        "acdd.yaml ignore list must exclude .acdd runtime state"
    );

    let profile = contextunity_acdd::core::tasks::profile::load_for_workspace(repo_root)
        .expect(".acdd/profile.yaml must load cleanly");
    assert_eq!(
        profile
            .scope_args_by_pattern
            .get("src/main.rs")
            .map(String::as_str),
        Some("--bin acdd")
    );
    for (pattern, expected_arg) in [
        ("crates/acdd-core/**/*.rs", "--package acdd-core --lib"),
        ("crates/acdd-index/**/*.rs", "--package acdd-index --lib"),
        ("crates/acdd-tasks/**/*.rs", "--package acdd-tasks --lib"),
    ] {
        assert_eq!(
            profile
                .scope_args_by_pattern
                .get(pattern)
                .map(String::as_str),
            Some(expected_arg),
            "expected updated package rule for {pattern}"
        );
    }

    let gitignore = std::fs::read_to_string(repo_root.join(".gitignore")).unwrap();
    assert!(gitignore.contains("!/.acdd/profile.yaml"));
    assert!(gitignore.contains(".acdd/*.sqlite*"));
}
