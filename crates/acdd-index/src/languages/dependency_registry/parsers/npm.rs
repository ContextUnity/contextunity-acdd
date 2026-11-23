use super::super::parser::{Ecosystem, ManifestParser, ParsedEntrypoint, ParsedManifest};

/// Parser for JavaScript/TypeScript manifests and lockfiles.
pub(crate) struct NpmParser;

/// Shared npm/JS/TS manifest parser instance.
pub(crate) static NPM_PARSER: NpmParser = NpmParser;

impl ManifestParser for NpmParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::JavaScript
    }

    fn filenames(&self) -> &'static [&'static str] {
        &[
            "package.json",
            "package-lock.json",
            "pnpm-lock.yaml",
            "pnpm-workspace.yaml",
            "yarn.lock",
            "tsconfig*.json",
        ]
    }

    fn parse(&self, filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::JavaScript);
        match filename {
            "pnpm-workspace.yaml" => {
                parsed.skip_dependency_recording = true;
            }
            name if name.starts_with("tsconfig") && name.ends_with(".json") => {
                parsed.skip_dependency_recording = true;
            }
            "package.json" => parse_package_json(content, &mut parsed),
            "package-lock.json" => parsed.dependencies = parse_package_lock(content),
            "pnpm-lock.yaml" => parsed.dependencies = parse_pnpm_lock(content),
            "yarn.lock" => parsed.dependencies = parse_yarn_lock(content),
            _ => {}
        }
        parsed
    }
}

/// Extracts dependency package names from JavaScript/TypeScript manifests and lockfiles.
pub(crate) fn extract_dependencies(filename: &str, content: &str) -> Vec<String> {
    match filename {
        "package.json" => serde_json::from_str::<serde_json::Value>(content)
            .map(|root| {
                let mut deps = Vec::new();
                collect_package_json_dependencies(&root, &mut deps);
                deps
            })
            .unwrap_or_default(),
        "package-lock.json" => parse_package_lock(content),
        "pnpm-lock.yaml" => parse_pnpm_lock(content),
        "yarn.lock" => parse_yarn_lock(content),
        _ => Vec::new(),
    }
}

fn is_local_reference(value: &str) -> bool {
    ["workspace:", "link:", "file:", "portal:"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn is_local_json_entry(value: &serde_json::Value) -> bool {
    value.get("link").and_then(serde_json::Value::as_bool) == Some(true)
        || value.as_str().is_some_and(is_local_reference)
        || ["version", "specifier", "resolved"].iter().any(|field| {
            value
                .get(*field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(is_local_reference)
        })
}

const PACKAGE_JSON_DEPENDENCY_SECTIONS: [&str; 3] =
    ["dependencies", "devDependencies", "peerDependencies"];

const LOCK_DEPENDENCY_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

fn collect_json_map<'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a serde_json::Value)>,
    out: &mut Vec<String>,
) {
    out.extend(
        entries
            .into_iter()
            .filter(|(_, descriptor)| !is_local_json_entry(descriptor))
            .map(|(name, _)| name.clone()),
    );
}

fn collect_json_sections(
    value: &serde_json::Value,
    sections: &[&str],
    out: &mut Vec<String>,
) {
    for section in sections {
        if let Some(entries) = value.get(*section).and_then(serde_json::Value::as_object) {
            collect_json_map(entries, out);
        }
    }
}

fn collect_package_json_dependencies(root: &serde_json::Value, out: &mut Vec<String>) {
    collect_json_sections(root, &PACKAGE_JSON_DEPENDENCY_SECTIONS, out);
}

fn parse_package_json(content: &str, parsed: &mut ParsedManifest) {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(content) else {
        return;
    };
    if let Some(name) = root.get("name").and_then(serde_json::Value::as_str) {
        parsed.own_packages.push(name.to_owned());
    }
    if let Some(target) = root.get("main").and_then(serde_json::Value::as_str) {
        parsed.entrypoints.push(ParsedEntrypoint {
            name: "main".to_owned(),
            target: target.to_owned(),
            python: false,
        });
    }
    match root.get("bin") {
        Some(serde_json::Value::String(target)) => parsed.entrypoints.push(ParsedEntrypoint {
            name: "bin:".to_owned(),
            target: target.clone(),
            python: false,
        }),
        Some(serde_json::Value::Object(bins)) => {
            for (name, value) in bins {
                if let Some(target) = value.as_str() {
                    parsed.entrypoints.push(ParsedEntrypoint {
                        name: format!("bin:{name}"),
                        target: target.to_owned(),
                        python: false,
                    });
                }
            }
        }
        _ => {}
    }
    collect_package_json_dependencies(&root, &mut parsed.dependencies);
}

fn parse_package_lock(content: &str) -> Vec<String> {
    let Ok(lock) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    collect_json_sections(&lock, &LOCK_DEPENDENCY_SECTIONS, &mut deps);
    if let Some(entries) = lock
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
    {
        let mut pending: Vec<&serde_json::Value> = entries.values().collect();
        while let Some(entry) = pending.pop() {
            if is_local_json_entry(entry) {
                continue;
            }
            collect_json_sections(entry, &LOCK_DEPENDENCY_SECTIONS, &mut deps);
            if let Some(nested) = entry
                .get("dependencies")
                .and_then(serde_json::Value::as_object)
            {
                pending.extend(nested.values());
            }
        }
    }
    if let Some(packages) = lock.get("packages").and_then(serde_json::Value::as_object) {
        for (path, package) in packages {
            if is_local_json_entry(package) {
                continue;
            }
            if let Some((_, name)) = path.rsplit_once("node_modules/") {
                if !name.is_empty() {
                    deps.push(name.to_owned());
                }
            }
            collect_json_sections(package, &LOCK_DEPENDENCY_SECTIONS, &mut deps);
        }
    }
    deps
}

fn is_local_yaml_entry(value: &serde_yaml::Value) -> bool {
    value.as_str().is_some_and(is_local_reference)
        || ["specifier", "version"].iter().any(|field| {
            value
                .get(*field)
                .and_then(serde_yaml::Value::as_str)
                .is_some_and(is_local_reference)
        })
}

fn pnpm_locator_name(locator: &str) -> Option<&str> {
    let locator = locator.trim_start_matches('/');
    let split = if let Some(scoped) = locator.strip_prefix('@') {
        let pkg_start = scoped.find('/').map(|idx| idx + 2)?;
        locator[pkg_start..]
            .find(['@', '/'])
            .map(|idx| idx + pkg_start)
    } else {
        locator.find(['@', '/'])
    }?;
    let tail = &locator[split + 1..];
    (split > 0 && !tail.is_empty() && !is_local_reference(tail)).then_some(&locator[..split])
}

fn collect_yaml_sections(mapping: &serde_yaml::Value, out: &mut Vec<String>) {
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(entries) = mapping.get(section).and_then(serde_yaml::Value::as_mapping) {
            out.extend(
                entries
                    .iter()
                    .filter(|(_, descriptor)| !is_local_yaml_entry(descriptor))
                    .filter_map(|(name, _)| name.as_str())
                    .map(str::to_owned),
            );
        }
    }
}

fn parse_pnpm_lock(content: &str) -> Vec<String> {
    let Ok(lock) = serde_yaml::from_str::<serde_yaml::Value>(content) else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    collect_yaml_sections(&lock, &mut deps);
    if let Some(importers) = lock
        .get("importers")
        .and_then(serde_yaml::Value::as_mapping)
    {
        for importer in importers.values() {
            collect_yaml_sections(importer, &mut deps);
        }
    }
    for section in ["packages", "snapshots"] {
        if let Some(packages) = lock.get(section).and_then(serde_yaml::Value::as_mapping) {
            deps.extend(
                packages
                    .keys()
                    .filter_map(serde_yaml::Value::as_str)
                    .filter_map(pnpm_locator_name)
                    .map(str::to_owned),
            );
        }
    }
    deps
}

fn yarn_selector_name(selector: &str) -> Option<&str> {
    let selector = selector.trim().trim_matches(['\'', '"']);
    let split = if let Some(scoped) = selector.strip_prefix('@') {
        scoped.find('@').map(|idx| idx + 1)
    } else {
        selector.find('@')
    }?;
    let tail = &selector[split + 1..];
    (split > 0 && !tail.is_empty() && !is_local_reference(tail)).then_some(&selector[..split])
}

fn parse_yarn_lock(content: &str) -> Vec<String> {
    let mut deps = Vec::new();
    for line in content.lines() {
        if line.is_empty() || line.starts_with([' ', '\t', '#']) || !line.ends_with(':') {
            continue;
        }
        for selector in line.trim_end_matches(':').split(',') {
            if let Some(name) = yarn_selector_name(selector) {
                deps.push(name.to_owned());
            }
        }
    }
    deps
}
