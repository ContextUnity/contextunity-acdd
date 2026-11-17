//! Fail-fast validation for `acdd.yaml`, `.acdd/profile.yaml`, `DomainProvider` capabilities, and spec documents.

use crate::state_bridge::build_domain_providers;
use acdd_core::config::{load_adapter, Adapter};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ports::CompositeDomainProvider;
use acdd_state::profile::{
    resolve_profile_from_str, resolve_profile_ref, validate_profile, ProfileValidationOptions,
    ResolvedGateProfile,
};
use acdd_state::spec::{parse_spec, SpecDocument, SpecStatus};
use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize)]
struct Diagnostic {
    status: &'static str,
    code: String,
    path: String,
    line: usize,
    column: usize,
    message: String,
}

/// Validate one document, or `acdd.yaml`, the active workspace profile, `DomainProvider`s, and all open specs.
pub fn run(root: &Path, relative: Option<&Path>) -> Result<Value> {
    let mut diagnostics = Vec::new();
    let mut warnings = Vec::new();

    let adapter = match load_adapter(root, None) {
        Ok(adapter) => adapter,
        Err(error) => {
            diagnostics.push(fail(
                ErrorCode::ProfileSchemaInvalid.as_str(),
                "acdd.yaml",
                1,
                1,
                format!("invalid acdd.yaml: {error}"),
            ));
            return Ok(report("workspace", diagnostics, warnings, 0));
        }
    };

    let providers = match build_domain_providers(root, &adapter) {
        Ok(providers) => providers,
        Err(error) => {
            diagnostics.push(fail_state_err("acdd.yaml", &error, 1, 1));
            return Ok(report("workspace", diagnostics, warnings, 0));
        }
    };

    if let Some(relative) = relative {
        let Some(relative_text) = relative.to_str() else {
            diagnostics.push(fail(
                "VALIDATION_PATH_INVALID",
                "<path>",
                1,
                1,
                "validation path must be valid UTF-8".to_owned(),
            ));
            return Ok(report("document", diagnostics, warnings, 1));
        };
        let path = match super::paths::verify_no_symlink_traversal(
            root,
            Path::new(relative_text),
            "VALIDATION_PATH_INVALID",
        ) {
            Ok(p) => p,
            Err(error) => {
                diagnostics.push(fail_from_anyhow(relative_text, &error, 1, 1));
                return Ok(report("document", diagnostics, warnings, 1));
            }
        };
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                diagnostics.push(fail(
                    "VALIDATION_IO",
                    relative_text,
                    1,
                    1,
                    format!("unable to read validation input: {error}"),
                ));
                return Ok(report("document", diagnostics, warnings, 1));
            }
        };

        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if file_name == "acdd.yaml" {
            match load_adapter(root, Some(&path)) {
                Ok(loaded) => match build_domain_providers(root, &loaded) {
                    Ok(_) => diagnostics.push(pass(
                        relative_text,
                        "CONFIG_VALID",
                        format!(
                            "validated acdd.yaml ({} spec collections, {} linked workspaces)",
                            loaded.specs.len(),
                            loaded.linked_workspaces.len()
                        ),
                    )),
                    Err(err) => diagnostics.push(fail_state_err(relative_text, &err, 1, 1)),
                },
                Err(err) => diagnostics.push(fail(
                    ErrorCode::ProfileSchemaInvalid.as_str(),
                    relative_text,
                    1,
                    1,
                    format!("invalid acdd.yaml: {err}"),
                )),
            }
            return Ok(report("document", diagnostics, warnings, 1));
        }

        match path_kind(&path) {
            Some(DocumentKind::Profile) => {
                let require_bound = relative_text == ".acdd/profile.yaml";
                validate_profile_content(
                    root,
                    relative_text,
                    &content,
                    &adapter,
                    &providers,
                    require_bound,
                    &mut diagnostics,
                );
            }
            Some(DocumentKind::Spec) => {
                let default_profile = resolve_default_workspace_profile(root, &adapter);
                if let Some(spec) = validate_spec_doc(
                    root,
                    relative_text,
                    &content,
                    &adapter,
                    &providers,
                    default_profile.as_ref(),
                    &mut diagnostics,
                    &mut warnings,
                ) {
                    validate_spec_dag(relative_text, &spec, &mut diagnostics);
                }
            }
            None => diagnostics.push(fail(
                "VALIDATION_KIND_UNSUPPORTED",
                relative_text,
                1,
                1,
                "expected a .md, .yaml, .yml, .yaml.example, or .yml.example document".to_owned(),
            )),
        }
        return Ok(report("document", diagnostics, warnings, 1));
    }

    if root.join("acdd.yaml").is_file() {
        diagnostics.push(pass(
            "acdd.yaml",
            "CONFIG_VALID",
            format!(
                "validated acdd.yaml ({} spec collections, {} providers)",
                adapter.specs.len(),
                2 + adapter.modules.domains.len()
            ),
        ));
    }

    let workspace_profile_path = ".acdd/profile.yaml";
    let has_workspace_profile = root.join(workspace_profile_path).is_file();
    let active_profile = match resolve_profile_ref("workspace", Some(root), Some(&adapter.modules))
    {
        Ok(resolved) => {
            let opts = ProfileValidationOptions {
                domain_providers: Some(&providers),
                acdd_modules: Some(&adapter.modules),
                require_bound_commands: has_workspace_profile,
            };
            match validate_profile(&resolved, &opts) {
                Ok(()) => {
                    diagnostics.push(pass(
                        if has_workspace_profile {
                            workspace_profile_path
                        } else {
                            "embedded:workspace"
                        },
                        "PROFILE_VALID",
                        format!(
                            "validated {} gates and {} commands",
                            resolved.gates.len(),
                            resolved.commands.len()
                        ),
                    ));
                    Some(resolved)
                }
                Err(err) => {
                    diagnostics.push(fail_state_err(workspace_profile_path, &err, 1, 1));
                    None
                }
            }
        }
        Err(err) => {
            diagnostics.push(fail_state_err(workspace_profile_path, &err, 1, 1));
            None
        }
    };

    let spec_files = scan_open_specs(root, &adapter, &mut diagnostics);
    let mut parsed_count = 0usize;
    for (rel_path, content) in spec_files {
        if let Some(spec) = validate_spec_doc(
            root,
            &rel_path,
            &content,
            &adapter,
            &providers,
            active_profile.as_ref(),
            &mut diagnostics,
            &mut warnings,
        ) {
            validate_spec_dag(&rel_path, &spec, &mut diagnostics);
            parsed_count += 1;
        }
    }

    Ok(report("workspace", diagnostics, warnings, parsed_count))
}

#[derive(Clone, Copy)]
enum DocumentKind {
    Spec,
    Profile,
}

fn path_kind(path: &Path) -> Option<DocumentKind> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".yaml.example") || name.ends_with(".yml.example") {
        return Some(DocumentKind::Profile);
    }
    match path.extension()?.to_str()? {
        "md" => Some(DocumentKind::Spec),
        "yaml" | "yml" => Some(DocumentKind::Profile),
        _ => None,
    }
}

fn resolve_default_workspace_profile(
    root: &Path,
    adapter: &Adapter,
) -> Option<ResolvedGateProfile> {
    resolve_profile_ref("workspace", Some(root), Some(&adapter.modules)).ok()
}

fn validate_profile_content(
    root: &Path,
    path: &str,
    content: &str,
    adapter: &Adapter,
    providers: &CompositeDomainProvider,
    require_bound_commands: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let profile_id = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("profile");
    let resolved = match resolve_profile_from_str(
        content,
        profile_id,
        Some(path),
        Some(root),
        Some(&adapter.modules),
    ) {
            Ok(resolved) => resolved,
            Err(err) => {
                diagnostics.push(fail_state_err(path, &err, 1, 1));
                return;
            }
        };
    let opts = ProfileValidationOptions {
        domain_providers: Some(providers),
        acdd_modules: Some(&adapter.modules),
        require_bound_commands,
    };
    match validate_profile(&resolved, &opts) {
        Ok(()) => diagnostics.push(pass(
            path,
            "PROFILE_VALID",
            format!(
                "validated {} gates and {} registered commands",
                resolved.gates.len(),
                resolved.commands.len()
            ),
        )),
        Err(err) => diagnostics.push(fail_state_err(path, &err, 1, 1)),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_spec_doc(
    root: &Path,
    path: &str,
    content: &str,
    adapter: &Adapter,
    providers: &CompositeDomainProvider,
    default_profile: Option<&ResolvedGateProfile>,
    diagnostics: &mut Vec<Diagnostic>,
    warnings: &mut Vec<Value>,
) -> Option<SpecDocument> {
    let spec = match parse_spec(content) {
        Ok(spec) => spec,
        Err(err) => {
            diagnostics.push(fail_state_err(path, &err, 1, 1));
            return None;
        }
    };

    let effective_profile_ref = spec.profile.as_deref().or_else(|| {
        adapter
            .specs
            .values()
            .filter(|c| path.starts_with(&format!("{}/", c.path.trim_end_matches('/'))))
            .max_by_key(|c| c.path.len())
            .and_then(|c| c.profile.as_deref())
    });

    let resolved_owned;
    let effective_profile = if let Some(pref) = effective_profile_ref {
        match resolve_profile_ref(pref, Some(root), Some(&adapter.modules)) {
            Ok(p) => {
                let opts = ProfileValidationOptions {
                    domain_providers: Some(providers),
                    acdd_modules: Some(&adapter.modules),
                    require_bound_commands: false,
                };
                if let Err(err) = validate_profile(&p, &opts) {
                    diagnostics.push(fail_state_err(path, &err, 1, 1));
                }
                resolved_owned = p;
                Some(&resolved_owned)
            }
            Err(err) => {
                diagnostics.push(fail_state_err(path, &err, 1, 1));
                default_profile
            }
        }
    } else {
        default_profile
    };

    if let Some(profile) = effective_profile {
        for task in &spec.tasks {
            for tag in &task.tags {
                if tag != "default" && !profile.tags.contains(tag) {
                    diagnostics.push(fail(
                        ErrorCode::ProfileSchemaInvalid.as_str(),
                        path,
                        1,
                        1,
                        format!(
                            "task '{}' references unknown tag '{}' not declared in profile.tags",
                            task.task_ref, tag
                        ),
                    ));
                }
            }
        }
        if let Some(ref guidance) = spec.guidance {
            let empty_guidance = matches!(guidance, serde_yaml::Value::Null)
                || guidance.as_str().is_some_and(|s| s.trim().is_empty())
                || guidance.as_mapping().is_some_and(|m| m.is_empty())
                || guidance.as_sequence().is_some_and(|s| s.is_empty());
            if empty_guidance {
                warnings.push(json!({"path": path, "message": "spec guidance block is empty"}));
            }
        }
    }

    diagnostics.push(pass(
        path,
        "SPEC_VALID",
        format!(
            "validated spec '{}' ({} tasks)",
            spec.id,
            spec.tasks.len()
        ),
    ));
    Some(spec)
}

fn validate_spec_dag(path: &str, spec: &SpecDocument, diagnostics: &mut Vec<Diagnostic>) {
    let task_refs: BTreeSet<&str> = spec.tasks.iter().map(|t| t.task_ref.as_str()).collect();
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

    for t in &spec.tasks {
        indegree.entry(t.task_ref.as_str()).or_insert(0);
        adj.entry(t.task_ref.as_str()).or_default();
    }

    for t in &spec.tasks {
        for dep in &t.depends_on {
            if dep.contains(':') || dep.starts_with('[') {
                continue;
            }
            if !task_refs.contains(dep.as_str()) {
                diagnostics.push(fail(
                    ErrorCode::ProfileSchemaInvalid.as_str(),
                    path,
                    1,
                    1,
                    format!(
                        "task '{}' depends on unknown local task '{}'",
                        t.task_ref, dep
                    ),
                ));
                continue;
            }
            if dep == t.task_ref.as_str() {
                diagnostics.push(fail(
                    ErrorCode::ProfileSchemaInvalid.as_str(),
                    path,
                    1,
                    1,
                    format!("task '{}' depends on itself", t.task_ref),
                ));
                continue;
            }
            adj.entry(dep.as_str()).or_default().push(t.task_ref.as_str());
            *indegree.entry(t.task_ref.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&k, _)| k)
        .collect();
    let mut visited = 0usize;
    while let Some(cur) = queue.pop_front() {
        visited += 1;
        if let Some(nexts) = adj.get(cur) {
            for &nxt in nexts {
                if let Some(deg) = indegree.get_mut(nxt) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(nxt);
                    }
                }
            }
        }
    }
    if visited != task_refs.len() {
        diagnostics.push(fail(
            ErrorCode::ProfileSchemaInvalid.as_str(),
            path,
            1,
            1,
            format!("spec '{}' task dependency graph contains a cycle", spec.id),
        ));
    }
}

fn scan_open_specs(
    root: &Path,
    adapter: &Adapter,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<(String, String)> {
    fn walk(
        root: &Path,
        dir: &Path,
        found: &mut BTreeMap<String, String>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if entry.file_name() != "archive" {
                    walk(root, &path, found, diagnostics);
                }
                continue;
            }
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !ft.is_file()
                || !name_str.ends_with(".md")
                || matches!(name_str, "README.md" | "AGENTS.md")
            {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            match fs::read_to_string(&path) {
                Ok(content) => {
                    let is_open = parse_spec(&content).map_or(true, |s| {
                        matches!(s.status, SpecStatus::Planned | SpecStatus::Active)
                    });
                    if is_open {
                        found.insert(rel_str, content);
                    }
                }
                Err(error) => {
                    diagnostics.push(fail(
                        "VALIDATION_IO",
                        &rel_str,
                        1,
                        1,
                        format!("unable to read spec manifest: {error}"),
                    ));
                }
            }
        }
    }

    let mut found = BTreeMap::new();
    for collection in adapter.specs.values() {
        let dir = root.join(&collection.path);
        if dir.is_dir() {
            walk(root, &dir, &mut found, diagnostics);
        }
    }
    found.into_iter().collect()
}

fn pass(path: &str, code: &str, message: String) -> Diagnostic {
    Diagnostic {
        status: "pass",
        code: code.to_owned(),
        path: path.to_owned(),
        line: 1,
        column: 1,
        message,
    }
}

fn fail(code: &str, path: &str, line: usize, column: usize, message: String) -> Diagnostic {
    Diagnostic {
        status: "fail",
        code: code.to_owned(),
        path: path.to_owned(),
        line,
        column,
        message,
    }
}

fn fail_state_err(path: &str, err: &StateError, line: usize, column: usize) -> Diagnostic {
    fail(err.code.as_str(), path, line, column, err.to_string())
}

fn fail_from_anyhow(path: &str, err: &anyhow::Error, line: usize, column: usize) -> Diagnostic {
    if let Some(se) = StateError::from_anyhow(err) {
        return fail_state_err(path, se, line, column);
    }
    fail(
        ErrorCode::ProfileSchemaInvalid.as_str(),
        path,
        line,
        column,
        format!("{err:#}"),
    )
}

fn report(
    scope: &str,
    diagnostics: Vec<Diagnostic>,
    warnings: Vec<Value>,
    documents_validated: usize,
) -> Value {
    let failed = diagnostics.iter().filter(|d| d.status == "fail").count();
    let passed = diagnostics.iter().filter(|d| d.status == "pass").count();
    json!({
        "valid": failed == 0,
        "scope": scope,
        "summary": {
            "passed": passed,
            "failed": failed,
            "warnings": warnings.len(),
            "documents_validated": documents_validated,
        },
        "diagnostics": diagnostics,
        "warnings": warnings,
    })
}
