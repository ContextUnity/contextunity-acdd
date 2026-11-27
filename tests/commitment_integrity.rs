#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "commitment_integrity/cold_sealing.rs"]
mod cold_sealing;
#[path = "commitment_integrity/disk_index.rs"]
mod disk_index;
#[path = "commitment_integrity/parallel_sealing.rs"]
mod parallel_sealing;
#[path = "commitment_integrity/spec_boundaries.rs"]
mod spec_boundaries;
#[path = "commitment_integrity/state_boundaries.rs"]
mod state_boundaries;

#[test]
fn workspace_proxy_stubs_and_duplicate_build_script_are_removed() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !manifest_dir.join("build.rs").exists(),
        "duplicate root build.rs must remain deleted"
    );
    assert!(
        !manifest_dir.join("src/core").exists(),
        "src/core proxy stub directory must remain deleted"
    );
    assert!(
        !manifest_dir.join("src/db").exists(),
        "src/db proxy stub directory must remain deleted"
    );
    let engine_mod = std::fs::read_to_string(manifest_dir.join("src/engine/mod.rs"))
        .expect("read src/engine/mod.rs");
    for line in engine_mod.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        assert!(
            !trimmed.starts_with("pub use ") && !trimmed.contains(" pub use "),
            "src/engine/mod.rs must not contain pub use re-exports: {trimmed}"
        );
    }
}
