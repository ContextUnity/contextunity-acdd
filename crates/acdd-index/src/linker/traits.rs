//! Language-owned import and receiver resolution used by the shared linker.

use super::NodeBindingIndex;
use crate::{
    languages::{
        dependency_registry::DependencyRegistry, manifests::FrameworkManifest, ImportPath,
        LanguageFamily, LanguageProfile,
    },
    interner::{LinkerInterner, PathId, SymbolId},
};
use crate::models::{Facts, Node, Reference};
use hashbrown::HashMap;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Package re-exports indexed by interned owner file and imported name.
pub type PackageExports<'a> = HashMap<PathId, HashMap<SymbolId, Option<&'a Node>>>;

pub(crate) fn package_export<'a>(
    exports: &PackageExports<'a>,
    interner: &LinkerInterner<'_>,
    path: &str,
    name: &str,
) -> Option<Option<&'a Node>> {
    exports
        .get(&interner.lookup_path(path)?)?
        .get(&interner.lookup_symbol(name)?)
        .copied()
}

pub(crate) fn insert_package_export<'a>(
    exports: &mut PackageExports<'a>,
    interner: &mut LinkerInterner<'a>,
    path: &'a str,
    name: &'a str,
    target: Option<&'a Node>,
) -> Option<Option<&'a Node>> {
    exports
        .entry(interner.intern_path(path))
        .or_default()
        .insert(interner.intern_symbol(name), target)
}

/// Cross-file indexes available while resolving one import reference.
pub type ModulesByNamespace<'a> = HashMap<LanguageFamily, HashMap<String, Vec<&'a Node>>>;

/// Select the nearest indexed module providers using the importing file's workspace and path.
pub(crate) fn import_provider_modules<'a, 'n>(
    path: &str,
    normalized: &'n ImportPath,
    family: LanguageFamily,
    modules_by_namespace: &ModulesByNamespace<'a>,
) -> (Vec<&'a Node>, &'n str) {
    let workspace = crate::languages::workspace_path(path).0;
    let mut providers = Vec::new();
    let mut selected_namespace = "";
    for local in [true, false] {
        if !local && normalized.relative {
            break;
        }
        let mut namespace = normalized.namespace.as_str();
        loop {
            if local || namespace.contains('.') {
                providers = modules_by_namespace
                    .get(&family)
                    .and_then(|namespaces| namespaces.get(namespace))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| {
                        (crate::languages::workspace_path(&node.path).0 == workspace) == local
                    })
                    .collect();
            }
            if !providers.is_empty() {
                selected_namespace = namespace;
                break;
            }
            if !normalized.symbol_path {
                break;
            }
            let Some((parent, _)) = namespace.rsplit_once('.') else {
                break;
            };
            namespace = parent;
        }
        if !providers.is_empty() {
            break;
        }
    }
    (providers, selected_namespace)
}

/// Drop stub and overload candidates only when a concrete implementation is available,
/// and disambiguate mutually exclusive Rust `#[cfg(...)]` sibling definitions.
pub(crate) fn filter_stubs_and_overloads(candidates: &mut Vec<&Node>) {
    if candidates.len() <= 1 {
        return;
    }

    let has_concrete = candidates.iter().any(|node| !is_stub_or_overload(node));
    let has_stub_or_overload = candidates.iter().any(|node| is_stub_or_overload(node));
    if has_concrete && has_stub_or_overload {
        candidates.retain(|node| !is_stub_or_overload(node));
    }
    disambiguate_rust_cfg_siblings(candidates);
}

/// Disambiguate Rust candidate sets whose definitions are pairwise mutually exclusive
/// under `#[cfg(...)]` attributes. Non-exclusive siblings remain ambiguous (fail-closed).
pub(crate) fn disambiguate_rust_cfg_siblings(candidates: &mut Vec<&Node>) {
    if candidates.len() <= 1
        || !candidates.iter().all(|node| node.language == "rust")
    {
        return;
    }
    let parsed: Vec<Option<CfgExpr>> = candidates.iter().map(|node| node_cfg_expr(node)).collect();
    if parsed.iter().any(Option::is_none) {
        return;
    }
    let exprs: Vec<&CfgExpr> = parsed.iter().filter_map(Option::as_ref).collect();
    for i in 0..exprs.len() {
        for j in (i + 1)..exprs.len() {
            if !cfg_mutually_exclusive(exprs[i], exprs[j]) {
                return;
            }
        }
    }
    // All candidates are pairwise mutually exclusive under #[cfg(...)].
    // Prefer a positive (non-`not(...)`) branch deterministically by node id.
    let best_idx = (0..candidates.len())
        .min_by(|&a, &b| {
            let a_neg = matches!(exprs[a], CfgExpr::Not(_));
            let b_neg = matches!(exprs[b], CfgExpr::Not(_));
            a_neg
                .cmp(&b_neg)
                .then_with(|| candidates[a].id.cmp(&candidates[b].id))
        })
        .unwrap_or(0);
    let chosen = candidates[best_idx];
    candidates.clear();
    candidates.push(chosen);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CfgExpr {
    Atom(String),
    KeyValue(String, String),
    Not(Box<CfgExpr>),
    All(Vec<CfgExpr>),
    Any(Vec<CfgExpr>),
}

fn node_cfg_expr(node: &Node) -> Option<CfgExpr> {
    let decorators = node.details.get("decorators")?.as_array()?;
    let mut clauses = Vec::new();
    for raw in decorators.iter().filter_map(|value| value.as_str()) {
        if let Some(expr) = parse_cfg_attribute(raw) {
            clauses.push(expr);
        }
    }
    match clauses.len() {
        0 => None,
        1 => clauses.pop(),
        _ => Some(CfgExpr::All(clauses)),
    }
}

fn parse_cfg_attribute(raw: &str) -> Option<CfgExpr> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("#[")?.strip_suffix(']')?.trim();
    let body = inner.strip_prefix("cfg")?.trim();
    let body = body.strip_prefix('(')?.strip_suffix(')')?.trim();
    parse_cfg_expr(body)
}

fn parse_cfg_expr(input: &str) -> Option<CfgExpr> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix("not").map(str::trim_start) {
        if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            return Some(CfgExpr::Not(Box::new(parse_cfg_expr(inner)?)));
        }
    }
    for (prefix, is_all) in [("all", true), ("any", false)] {
        if let Some(rest) = s.strip_prefix(prefix).map(str::trim_start) {
            if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
                let items = split_cfg_args(inner)
                    .into_iter()
                    .map(parse_cfg_expr)
                    .collect::<Option<Vec<_>>>()?;
                if items.is_empty() {
                    return None;
                }
                return Some(if is_all {
                    CfgExpr::All(items)
                } else {
                    CfgExpr::Any(items)
                });
            }
        }
    }
    if let Some((key, value)) = s.split_once('=') {
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        if !key.is_empty() && !value.is_empty() {
            return Some(CfgExpr::KeyValue(key.to_owned(), value.to_owned()));
        }
        return None;
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Some(match s {
            "unix" => CfgExpr::KeyValue("target_family".to_owned(), "unix".to_owned()),
            "windows" => CfgExpr::KeyValue("target_family".to_owned(), "windows".to_owned()),
            _ => CfgExpr::Atom(s.to_owned()),
        });
    }
    None
}

fn split_cfg_args(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut in_quotes = false;
    let mut start = 0usize;
    for (idx, ch) in input.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => depth += 1,
            ')' if !in_quotes => depth = depth.saturating_sub(1),
            ',' if !in_quotes && depth == 0 => {
                let item = input[start..idx].trim();
                if !item.is_empty() {
                    parts.push(item);
                }
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = input[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn cfg_mutually_exclusive(a: &CfgExpr, b: &CfgExpr) -> bool {
    match (a, b) {
        (CfgExpr::All(xs), other) | (other, CfgExpr::All(xs)) => {
            xs.iter().any(|x| cfg_mutually_exclusive(x, other))
        }
        (CfgExpr::Any(xs), other) | (other, CfgExpr::Any(xs)) => {
            !xs.is_empty() && xs.iter().all(|x| cfg_mutually_exclusive(x, other))
        }
        (CfgExpr::Not(inner), other) | (other, CfgExpr::Not(inner)) => cfg_implies(other, inner),
        (CfgExpr::KeyValue(k1, v1), CfgExpr::KeyValue(k2, v2)) => {
            if k1 == k2 && v1 != v2 {
                return matches!(
                    k1.as_str(),
                    "target_os"
                        | "target_arch"
                        | "target_endian"
                        | "target_env"
                        | "target_pointer_width"
                        | "target_vendor"
                        | "target_abi"
                        | "target_family"
                );
            }
            // target_os implies target_family for unix/windows
            os_family_conflict(k1, v1, k2, v2) || os_family_conflict(k2, v2, k1, v1)
        }
        _ => false,
    }
}

fn cfg_implies(premise: &CfgExpr, conclusion: &CfgExpr) -> bool {
    if premise == conclusion {
        return true;
    }
    match conclusion {
        CfgExpr::Any(xs) => xs.iter().any(|x| cfg_implies(premise, x)),
        CfgExpr::All(xs) => !xs.is_empty() && xs.iter().all(|x| cfg_implies(premise, x)),
        _ => false,
    }
}

fn os_family_conflict(k_os: &str, v_os: &str, k_fam: &str, v_fam: &str) -> bool {
    if k_os != "target_os" || k_fam != "target_family" {
        return false;
    }
    let os_is_unix = matches!(
        v_os,
        "linux"
            | "macos"
            | "freebsd"
            | "netbsd"
            | "openbsd"
            | "dragonfly"
            | "ios"
            | "android"
            | "solaris"
            | "illumos"
    );
    (os_is_unix && v_fam == "windows") || (v_os == "windows" && v_fam == "unix")
}

fn is_stub_or_overload(node: &Node) -> bool {
    node.details
        .get("is_stub")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        || node
            .details
            .get("is_overload")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}

/// Input for a language linker resolving one import.
pub struct ImportContext<'ctx, 'a, 'input> {
    /// Workspace-relative importing file.
    pub path: &'input str,
    /// Canonical symbol and path identities shared by linker indexes.
    pub interner: &'ctx LinkerInterner<'a>,
    /// Dependency data and framework manifests admitted for the indexed workspace.
    pub dependencies: &'ctx DependencyRegistry,
    /// Parsed import reference.
    pub reference: &'input Reference,
    /// Rust's bounded canonical named-forwarding index, when Rust is enabled.
    #[cfg(feature = "lang-rust")]
    pub(crate) rust_forwarding: Option<&'ctx crate::languages::rust::linker::RustForwarding<'a>>,
    /// Normalized import path from the language profile.
    pub normalized: Option<&'input ImportPath>,
    /// Namespace prefix that selected the indexed module.
    pub selected_namespace: &'input str,
    /// Language family of the importing file.
    pub family: LanguageFamily,
    /// Candidate modules for the import.
    pub modules: &'ctx mut Vec<&'a Node>,
    /// Candidate symbols selected by the linker.
    pub candidates: &'ctx mut Vec<&'a Node>,
    /// Modules grouped under language-specific import namespaces.
    pub modules_by_namespace: &'ctx ModulesByNamespace<'a>,
    /// Nodes grouped by source file.
    pub by_module: &'ctx HashMap<&'a str, Vec<&'a Node>>,
    /// Nodes grouped by fully qualified name.
    pub by_qual: &'ctx HashMap<&'a str, Vec<&'a Node>>,
    /// Explicit package re-exports discovered for the workspace.
    pub package_exports: &'ctx PackageExports<'a>,
    /// Reusable buffer for qualified-name lookup.
    pub lookup_key: &'ctx mut String,
}

impl ImportContext<'_, '_, '_> {
    /// Returns the imported symbol suffix, or an empty suffix for a module import.
    pub fn tail(&self) -> Option<&str> {
        let normalized = self.normalized?;
        Some(if normalized.symbol_path {
            normalized
                .namespace
                .strip_prefix(self.selected_namespace)
                .unwrap_or("")
                .trim_start_matches('.')
        } else if self.reference.expression == "*"
            || self.reference.module.as_deref() == Some(&self.reference.expression)
        {
            ""
        } else {
            &self.reference.expression
        })
    }
}

/// Language-specific import-resolution details returned to the common graph builder.
#[derive(Default)]
pub struct ImportResolution<'a> {
    /// Paired declaration-only module used when the runtime module lacks a symbol.
    pub paired_stub: Option<&'a Node>,
    /// The selected import names a child module rather than a declaration.
    pub child_module: bool,
    /// The selected declaration comes from a paired stub.
    pub stub_symbol: bool,
    /// The selected declaration is re-exported through a package initializer.
    pub reexport_symbol: bool,
}

/// Owns language-specific import, receiver, and incremental-linking rules.
pub trait LanguageLinker: Send + Sync {
    /// Resolves a parsed import against indexed modules and declarations.
    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        resolve_default_import(context)
    }

    /// Reports whether a receiver name is valid for its owning declaration.
    fn resolve_receiver(&self, profile: &dyn LanguageProfile, name: &str, owner: &Node) -> bool {
        profile.receiver(name, owner)
    }

    /// Returns a declared framework's user rules for the owning source path.
    ///
    /// Framework data stays behind the existing language-linker seam. The
    /// dependency registry enforces package declaration and workspace scope.
    fn framework_manifest<'a>(
        &self,
        dependencies: &'a DependencyRegistry,
        family: LanguageFamily,
        path: &str,
        framework: &str,
        package: &str,
    ) -> Option<&'a FrameworkManifest> {
        dependencies.framework_manifest_for_path(family, path, framework, package)
    }

    /// Resolves a member through language-specific companion declarations.
    #[allow(clippy::too_many_arguments)]
    fn resolve_imported_member<'a>(
        &self,
        _interner: &LinkerInterner<'a>,
        _module: &'a Node,
        _member: &str,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _by_qual: &HashMap<&'a str, Vec<&'a Node>>,
        _exports: &PackageExports<'a>,
        _lookup_key: &mut String,
    ) -> Vec<&'a Node> {
        Vec::new()
    }

    /// Builds language-specific package re-export indexes before parallel linking.
    fn package_exports<'a>(
        &self,
        _interner: &mut LinkerInterner<'a>,
        _node_rebindings: &NodeBindingIndex,
        _all: &'a BTreeMap<String, Facts>,
        _modules_by_namespace: &ModulesByNamespace<'a>,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _root: Option<&Path>,
    ) -> PackageExports<'a> {
        HashMap::new()
    }

    /// Builds package re-export indexes from borrowed per-file facts.
    ///
    /// The borrowed form avoids cloning facts during incremental relinking.
    fn package_exports_borrowed<'a>(
        &self,
        _interner: &mut LinkerInterner<'a>,
        _node_rebindings: &NodeBindingIndex,
        _all: &BTreeMap<&'a str, &'a Facts>,
        _modules_by_namespace: &ModulesByNamespace<'a>,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _root: Option<&Path>,
    ) -> PackageExports<'a> {
        HashMap::new()
    }

    /// Returns files whose full facts are required to relink affected owners.
    fn required_full_facts(
        &self,
        _facts: &BTreeMap<String, Facts>,
        _affected: &BTreeSet<String>,
        _catalog: &[(&str, &str)],
    ) -> BTreeSet<String> {
        BTreeSet::new()
    }

    /// Returns files whose borrowed full facts are needed to relink affected owners.
    ///
    /// Implementations keep the returned set scoped to the affected import graph.
    fn required_full_facts_borrowed(
        &self,
        _facts: &BTreeMap<String, &Facts>,
        _affected: &BTreeSet<String>,
        _catalog: &[(&str, &str)],
    ) -> BTreeSet<String> {
        BTreeSet::new()
    }

    /// Reports whether a file's references can change identity when it changes.
    fn needs_reference_identity(&self, _path: &str, _facts: &Facts) -> bool {
        false
    }

    /// Compares language-specific reference identity across an incremental update.
    fn reference_identity_changed(&self, _path: &str, _old: &Facts, _new: &Facts) -> bool {
        false
    }

    /// Reports whether an indexed path is a declaration-only companion file.
    fn is_declaration_only(&self, _path: &str) -> bool {
        false
    }

    /// Reports whether unresolved external imports provide call aliases.
    fn tracks_external_aliases(&self) -> bool {
        false
    }
}

/// Generic linker used by profiles without language-specific resolution rules.
pub struct GenericLanguageLinker;

/// Shared generic linker instance.
pub static GENERIC_LINKER: GenericLanguageLinker = GenericLanguageLinker;

impl LanguageLinker for GenericLanguageLinker {}

pub(crate) fn resolve_default_import<'ctx, 'a, 'input>(
    context: &mut ImportContext<'ctx, 'a, 'input>,
) -> ImportResolution<'a> {
    let Some(tail) = context.tail().map(str::to_owned) else {
        return ImportResolution::default();
    };
    let Some(module) = context
        .modules
        .first()
        .copied()
        .filter(|_| context.modules.len() == 1)
    else {
        return ImportResolution::default();
    };
    if tail.is_empty() {
        context.candidates.push(module);
        return ImportResolution::default();
    }
    context.lookup_key.clear();
    context.lookup_key.push_str(&module.qualname);
    context.lookup_key.push('.');
    context.lookup_key.push_str(&tail);
    let target = context.lookup_key.as_str();
    context.candidates.extend(
        context
            .by_module
            .get(module.path.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| {
                if tail == "default" {
                    node.details["default_export"] == true
                } else {
                    node.kind != "component" && node.qualname == target
                }
            }),
    );
    ImportResolution::default()
}

#[cfg(test)]
mod tests {
    use super::filter_stubs_and_overloads;
    use crate::models::Node;

    fn node(id: &str, is_stub: bool, is_overload: bool) -> Node {
        Node {
            native_semantic: None,
            id: id.to_owned(),
            kind: "function".to_owned(),
            name: id.to_owned(),
            qualname: id.to_owned(),
            path: "module.ts".to_owned(),
            line: 1,
            end_line: 1,
            is_test: false,
            language: "typescript".to_owned(),
            generated: false,
            details: serde_json::json!({
                "is_stub": is_stub,
                "is_overload": is_overload,
            }),
        }
    }

    #[test]
    fn filter_stubs_and_overloads_keeps_concrete_candidates_when_present() {
        let stub = node("stub", true, false);
        let implementation = node("implementation", false, false);
        let overload = node("overload", false, true);
        let mut candidates = vec![&stub, &implementation, &overload];

        filter_stubs_and_overloads(&mut candidates);

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["implementation"]
        );
    }

    #[test]
    fn filter_stubs_and_overloads_keeps_ambiguous_stubs_without_implementation() {
        let stub = node("stub", true, false);
        let overload = node("overload", false, true);
        let mut candidates = vec![&stub, &overload];

        filter_stubs_and_overloads(&mut candidates);

        assert_eq!(candidates.len(), 2);
    }
}
