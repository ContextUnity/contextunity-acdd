use super::*;

impl FrameworkHarness {
    pub(in super::super) fn discover_with_custom_adapter(
        owner: &Workspace,
        workspace_files: &[(&str, &str)],
        enabled_files: &[(&str, &str)],
        disabled_files: &[(&str, &str)],
    ) -> DependencyRegistry {
        for (root, files) in [
            ("primary", workspace_files),
            ("enabled", enabled_files),
            ("disabled", disabled_files),
        ] {
            let directory = owner.path(root);
            if directory.exists() {
                std::fs::remove_dir_all(&directory).unwrap();
            }
            std::fs::create_dir_all(&directory).unwrap();
            for (path, source) in files {
                owner.write(format!("{root}/{path}"), source);
            }
        }
        let workspace = owner.path("primary");
        let enabled = owner.path("enabled");
        let disabled = owner.path("disabled");
        owner.write(
            "primary/custom-adapter.yaml",
            &format!(
                "roots: [.]\nexcluded_directory_names: [excluded]\nlinked_workspaces:\n  - name: enabled\n    path: '{}'\n    roots: [.]\n  - name: disabled\n    path: '{}'\n    enabled: false\n    roots: [.]\n",
                enabled.display(),
                disabled.display()
            ),
        );
        let adapter = acdd_index::scanner::load_adapter(
            &workspace,
            Some(&workspace.join("custom-adapter.yaml")),
        )
        .unwrap();
        DependencyRegistry::collect_with_scan_config(&workspace, Some(&adapter))
    }
}
