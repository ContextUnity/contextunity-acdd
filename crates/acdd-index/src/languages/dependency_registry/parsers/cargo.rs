use super::super::parser::{Ecosystem, ManifestParser, ParsedManifest, RustManifestInfo};

/// Parser for `Cargo.toml` manifests.
pub struct CargoParser;

/// Shared `Cargo.toml` manifest parser instance.
pub static CARGO_PARSER: CargoParser = CargoParser;

impl ManifestParser for CargoParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Rust
    }

    fn filenames(&self) -> &'static [&'static str] {
        &["Cargo.toml"]
    }

    fn parse(&self, _filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::Rust);
        let Ok(root) = toml::from_str::<toml::Table>(content) else {
            return parsed;
        };

        let mut package_name = None::<String>;
        let mut valid_package_name = false;
        if let Some(pkg) = root.get("package").and_then(toml::Value::as_table) {
            if let Some(name) = pkg.get("name").and_then(toml::Value::as_str) {
                valid_package_name = !name.is_empty()
                    && name
                        .chars()
                        .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-'));
                if valid_package_name {
                    parsed.own_packages.push(name.to_owned());
                    package_name = Some(name.to_owned());
                }
            }
        }

        let autolib = root
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|pkg| pkg.get("autolib"));
        let autolib_valid = match autolib {
            None => true,
            Some(val) => val.is_bool(),
        };
        let has_lib_table = root.get("lib").is_some_and(toml::Value::is_table);
        let lib_table = root.get("lib").and_then(toml::Value::as_table);
        let lib_name = lib_table.and_then(|lib| lib.get("name").and_then(toml::Value::as_str));
        let invalid_lib_name = lib_table.is_some_and(|lib| {
            lib.get("name").is_some_and(|val| {
                val.as_str().is_none_or(|name| {
                    name.is_empty()
                        || !name
                            .chars()
                            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-'))
                })
            })
        });
        let lib_path = lib_table.and_then(|lib| lib.get("path").and_then(toml::Value::as_str));
        let invalid_lib_path = lib_table.is_some_and(|lib| {
            lib.get("path")
                .is_some_and(|val| val.as_str().is_none_or(|path| !matches!(path, "src/lib.rs" | "./src/lib.rs")))
        });

        let autolib_allows = match autolib.and_then(toml::Value::as_bool) {
            None | Some(true) => true,
            Some(false) => has_lib_table,
        };
        let base_valid = autolib_valid
            && autolib_allows
            && !invalid_lib_name
            && !invalid_lib_path
            && valid_package_name;
        let resolved_crate = (base_valid
            && lib_path.is_none_or(|path| matches!(path, "src/lib.rs" | "./src/lib.rs")))
        .then(|| {
            lib_name
                .map(str::to_owned)
                .or_else(|| package_name.clone())
                .map(|name| name.replace('-', "_"))
        })
        .flatten();

        let package_library = (base_valid && lib_path.is_none_or(|path| path == "src/lib.rs"))
            .then(|| {
                lib_name
                    .map(str::to_owned)
                    .or(package_name)
                    .map(|name| name.replace('-', "_"))
            })
            .flatten();

        parsed.rust_info = Some(RustManifestInfo {
            package_library,
            root_crate: resolved_crate,
        });

        let mut deps = Vec::new();
        collect_dep_sections(&root, &mut deps);
        if let Some(workspace) = root.get("workspace").and_then(toml::Value::as_table) {
            if let Some(ws_deps) = workspace
                .get("dependencies")
                .and_then(toml::Value::as_table)
            {
                deps.extend(ws_deps.keys().map(|name| name.replace('-', "_")));
            }
        }
        if let Some(targets) = root.get("target").and_then(toml::Value::as_table) {
            for target_tbl in targets.values().filter_map(toml::Value::as_table) {
                collect_dep_sections(target_tbl, &mut deps);
            }
        }
        deps.retain(|name| !name.is_empty());
        deps.sort_unstable();
        deps.dedup();
        parsed.dependencies = deps;
        parsed
    }
}

fn collect_dep_sections(table: &toml::Table, out: &mut Vec<String>) {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(entries) = table.get(section).and_then(toml::Value::as_table) {
            out.extend(entries.keys().map(|name| name.replace('-', "_")));
        }
    }
}
