use crate::languages::types::{module_stem, workspace_path, ImportPath};
use crate::models::Node;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RestrictedExport {
    pub name: String,
    pub module: Option<String>,
    pub scope: String,
}

pub(crate) fn module_namespace(name: &str) -> &str {
    name.strip_suffix(".lib")
        .or_else(|| name.strip_suffix(".main"))
        .unwrap_or(name)
}

pub(crate) fn crate_namespace(path: &str) -> String {
    let (workspace, local) = workspace_path(path);
    let namespace = if let Some((prefix, _)) = local.split_once("/src/") {
        format!("{}.src", prefix.replace('/', "."))
    } else {
        local.split('/').next().unwrap_or(local).to_owned()
    };
    if workspace.is_empty() {
        namespace
    } else {
        format!("{workspace}.{namespace}")
    }
}

pub(crate) fn within(requester: &str, scope: &str) -> bool {
    let requester = module_namespace(requester);
    requester == scope
        || requester
            .strip_prefix(scope)
            .is_some_and(|tail| tail.starts_with('.'))
}

pub(crate) fn restricted_scope(signature: &str, module: &str, path: &str) -> Option<String> {
    let restriction = signature.strip_prefix("pub(")?.split_once(')')?.0.trim();
    let module = module_namespace(module);
    let crate_root = crate_namespace(path);
    let scope = match restriction {
        "crate" => crate_root,
        "self" => module.to_owned(),
        "super" => module.rsplit_once('.')?.0.to_owned(),
        restriction => {
            let mut cur = module;
            let mut rest = restriction.strip_prefix("in ")?.trim();
            loop {
                if rest == "crate" {
                    break crate_root;
                }
                if rest == "self" {
                    break cur.to_owned();
                }
                if let Some(tail) = rest.strip_prefix("crate::") {
                    break format!("{crate_root}.{}", tail.replace("::", "."));
                }
                if rest == "super" {
                    break cur.rsplit_once('.')?.0.to_owned();
                }
                if let Some(tail) = rest.strip_prefix("super::") {
                    cur = cur.rsplit_once('.')?.0;
                    rest = tail;
                    continue;
                }
                if rest.starts_with("self::") {
                    rest = rest.strip_prefix("self::")?;
                }
                break format!("{cur}.{}", rest.replace("::", "."));
            }
        }
    };
    (within(module, &scope) && within(&scope, &crate_namespace(path))).then_some(scope)
}

pub(crate) fn restricted_exports(module: &Node) -> Vec<RestrictedExport> {
    module
        .details
        .get("rust_restricted_exports")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

pub(crate) fn rust_module_name(path: &str) -> String {
    module_stem(path).trim_end_matches("/mod").replace('/', ".")
}

pub(crate) fn normalize_root_crate_import(crate_name: &str, module: &str) -> Option<ImportPath> {
    let (head, tail) = module.split_once("::").unwrap_or((module, ""));
    if head != crate_name {
        return None;
    }
    Some(ImportPath {
        namespace: if tail.is_empty() || tail == "*" {
            "src".into()
        } else {
            format!("src.{}", tail.replace("::", "."))
        },
        relative: true,
        symbol_path: true,
    })
}

pub(crate) fn is_rust_stdlib(module: &str) -> bool {
    matches!(
        module.split("::").next(),
        Some("std" | "core" | "alloc" | "proc_macro")
    )
}
