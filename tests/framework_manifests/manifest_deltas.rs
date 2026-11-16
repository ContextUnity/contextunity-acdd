use super::*;

const CONSUMER: &str = "import novel_library as client\ndef run():\n    return client.fetch()\n";
const EXTRA_REQUIREMENTS: &str = "novel_library>=2\n";

#[cfg(feature = "lang-python")]
#[test]
fn manifest_reclassification_matrix() {
    let workspace = FrameworkHarness::published(&[
        ("consumer.py", "def run(): return 1\n"),
        ("unchanged.py", CONSUMER),
    ]);
    let cold_workspace = FrameworkHarness::persisted(&[]);
    for (name, listed, present, declared) in [
        (
            "manifest_addition_reclassifies_existing_import_and_matches_cold_graph",
            true,
            true,
            true,
        ),
        (
            "manifest_removal_restores_unlisted_import_origin",
            true,
            false,
            false,
        ),
        (
            "unlisted_manifest_change_rebuilds_unmodified_import_owners",
            false,
            true,
            true,
        ),
    ] {
        let manifest = "requirements-extra.txt";
        if present {
            workspace.write(manifest, EXTRA_REQUIREMENTS);
        } else {
            workspace.remove(manifest);
        }
        if listed {
            workspace.delta(&[manifest]);
        } else {
            workspace.write("consumer.py", "def run(): return 2\n");
            workspace.delta(&["consumer.py"]);
        }
        let conn = workspace.open();
        let incremental = coverage_details(&conn);
        assert!(
            incremental.iter().any(
                |(expression, status, evidence)| expression == "novel_library"
                    && status == "external"
                    && evidence.contains("manifest") == declared
            ),
            "{name}: {incremental:?}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path("unchanged.py")).unwrap(),
            CONSUMER,
            "{name}"
        );
        cold_workspace.write(
            "consumer.py",
            if listed {
                "def run(): return 1\n"
            } else {
                "def run(): return 2\n"
            },
        );
        cold_workspace.write("unchanged.py", CONSUMER);
        if present {
            cold_workspace.write(manifest, EXTRA_REQUIREMENTS);
        } else {
            cold_workspace.remove(manifest);
        }
        cold_workspace.build();
        assert_eq!(
            incremental,
            coverage_details(&cold_workspace.open()),
            "{name}: cold/delta parity"
        );
        commitments::verify(&conn).unwrap();
    }
}
