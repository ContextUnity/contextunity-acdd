#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "languages/mod.rs"]
mod languages;
pub(crate) use languages::support;

#[path = "framework_manifests/mod.rs"]
mod manifests;

#[test]
fn helper_modules_are_not_advertised_as_language_profiles() {
    use acdd_index::languages;

    for (id, feature) in [
        ("support", "lang-support"),
        ("registries", "lang-registries"),
    ] {
        assert!(
            !languages::AVAILABLE_LANGUAGE_FEATURES.contains(&feature),
            "helper module {id} was advertised as an available language feature"
        );
        let error = match languages::require(id) {
            Ok(profile) => panic!(
                "helper module {id} unexpectedly resolved to profile {}",
                profile.id()
            ),
            Err(error) => error.to_string(),
        };
        assert!(
            error.starts_with("unknown AST language"),
            "helper module {id} should be reported as unknown, got: {error}"
        );
    }
}
