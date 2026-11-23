use super::super::parser::{Ecosystem, ManifestParser, ParsedEntrypoint, ParsedManifest};

/// Parser for Python manifests (`pyproject.toml`, `requirements*.txt`, `setup.cfg`, `Pipfile`).
pub(crate) struct PythonParser;

/// Shared Python manifest parser instance.
pub(crate) static PYTHON_PARSER: PythonParser = PythonParser;

impl ManifestParser for PythonParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Python
    }

    fn filenames(&self) -> &'static [&'static str] {
        &[
            "pyproject.toml",
            "requirements*.txt",
            "setup.cfg",
            "Pipfile",
        ]
    }

    fn parse(&self, filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::Python);
        let base = filename.rsplit('/').next().unwrap_or(filename);
        if base.starts_with("requirements") && base.ends_with(".txt") {
            collect_requirement_lines(content, &mut parsed.dependencies);
        } else if base == "setup.cfg" {
            parse_setup_cfg(content, &mut parsed.dependencies);
        } else if base == "pyproject.toml" {
            parse_pyproject_toml(content, &mut parsed);
        } else if base == "Pipfile" {
            parse_pipfile(content, &mut parsed.dependencies);
        }
        parsed.finish()
    }
}

/// Extracts sorted, deduplicated normalized dependency names from Python manifests.
pub(crate) fn extract_dependencies(filename: &str, content: &str) -> Vec<String> {
    PYTHON_PARSER.parse(filename, content).dependencies
}

/// Normalizes a PEP 508 requirement string into a canonical underscore-separated package name.
pub(crate) fn normalize_package_name(requirement: &str) -> Option<String> {
    let requirement = requirement.trim();
    let requirement = if requirement.starts_with("-e ") || requirement.starts_with("--editable ") {
        requirement.split("#egg=").nth(1)?
    } else {
        requirement
    };
    if !requirement.as_bytes().first()?.is_ascii_alphanumeric() {
        return None;
    }
    let name = requirement
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .next()?;
    let tail = &requirement[name.len()..];
    if tail.chars().next().is_some_and(|ch| {
        !ch.is_whitespace() && !matches!(ch, '[' | '<' | '>' | '=' | '!' | '~' | ';' | '@')
    }) || [".whl", ".tar.gz", ".zip"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return None;
    }
    Some(name.to_ascii_lowercase().replace(['-', '.'], "_"))
}

fn collect_requirement_lines(source: &str, out: &mut Vec<String>) {
    out.extend(source.lines().filter_map(normalize_package_name));
}

fn collect_toml_string_array(value: Option<&toml::Value>, out: &mut Vec<String>) {
    let Some(items) = value.and_then(toml::Value::as_array) else {
        return;
    };
    for item in items {
        if let Some(pkg) = item.as_str().and_then(normalize_package_name) {
            out.push(pkg);
        }
    }
}

fn collect_toml_table_keys(table: Option<&toml::Table>, out: &mut Vec<String>) {
    let Some(entries) = table else {
        return;
    };
    for key in entries.keys() {
        if key != "python" {
            if let Some(pkg) = normalize_package_name(key) {
                out.push(pkg);
            }
        }
    }
}

fn parse_pyproject_toml(source: &str, parsed: &mut ParsedManifest) {
    let Ok(root) = toml::from_str::<toml::Table>(source) else {
        return;
    };
    if let Some(project) = root.get("project").and_then(toml::Value::as_table) {
        if let Some(name) = project.get("name").and_then(toml::Value::as_str) {
            parsed.own_packages.push(name.to_owned());
        }
        if let Some(scripts) = project.get("scripts").and_then(toml::Value::as_table) {
            for (name, target) in scripts {
                if let Some(target) = target.as_str() {
                    parsed.entrypoints.push(ParsedEntrypoint {
                        name: name.clone(),
                        target: target.to_owned(),
                        python: true,
                    });
                }
            }
        }
        collect_toml_string_array(project.get("dependencies"), &mut parsed.dependencies);
        if let Some(optional) = project
            .get("optional-dependencies")
            .and_then(toml::Value::as_table)
        {
            for group in optional.values() {
                collect_toml_string_array(Some(group), &mut parsed.dependencies);
            }
        }
    }
    if let Some(groups) = root
        .get("dependency-groups")
        .and_then(toml::Value::as_table)
    {
        for group in groups.values() {
            collect_toml_string_array(Some(group), &mut parsed.dependencies);
        }
    }
    if let Some(build_system) = root.get("build-system").and_then(toml::Value::as_table) {
        collect_toml_string_array(build_system.get("requires"), &mut parsed.dependencies);
    }
    if let Some(poetry) = root
        .get("tool")
        .and_then(toml::Value::as_table)
        .and_then(|tool| tool.get("poetry"))
        .and_then(toml::Value::as_table)
    {
        if let Some(name) = poetry.get("name").and_then(toml::Value::as_str) {
            if !parsed.own_packages.iter().any(|existing| existing == name) {
                parsed.own_packages.push(name.to_owned());
            }
        }
        collect_toml_table_keys(
            poetry.get("dependencies").and_then(toml::Value::as_table),
            &mut parsed.dependencies,
        );
        collect_toml_table_keys(
            poetry
                .get("dev-dependencies")
                .and_then(toml::Value::as_table),
            &mut parsed.dependencies,
        );
        if let Some(groups) = poetry.get("group").and_then(toml::Value::as_table) {
            for group in groups.values().filter_map(toml::Value::as_table) {
                collect_toml_table_keys(
                    group.get("dependencies").and_then(toml::Value::as_table),
                    &mut parsed.dependencies,
                );
            }
        }
    }
}

fn parse_pipfile(source: &str, out: &mut Vec<String>) {
    let Ok(root) = toml::from_str::<toml::Table>(source) else {
        return;
    };
    collect_toml_table_keys(root.get("packages").and_then(toml::Value::as_table), out);
    collect_toml_table_keys(
        root.get("dev-packages").and_then(toml::Value::as_table),
        out,
    );
}

fn parse_setup_cfg(source: &str, out: &mut Vec<String>) {
    let mut section = "";
    let mut active = false;
    for raw in source.lines() {
        let line = raw.trim();
        if line.starts_with(['#', ';']) || line.is_empty() {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name;
            active = false;
        } else if raw.starts_with(char::is_whitespace) && active {
            collect_requirement_lines(line, out);
        } else if let Some((key, value)) = line.split_once('=') {
            active = section == "options.extras_require"
                || (section == "options"
                    && matches!(
                        key.trim(),
                        "install_requires" | "setup_requires" | "tests_require"
                    ));
            if active {
                collect_requirement_lines(value, out);
            }
        } else {
            active = false;
        }
    }
}
