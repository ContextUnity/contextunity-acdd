//! Language-owned import and receiver resolution used by the shared linker.

pub use crate::models::FactsLookup;

use super::NodeBindingIndex;
use crate::models::{
    tags::{EdgeKind, NodeKind},
    Facts, Node, Reference,
};
use crate::{
    interner::{LinkerInterner, PathId, SymbolId},
    languages::{
        self,
        dependency_registry::{parser::ModulePrefix, DependencyRegistry},
        manifests::FrameworkManifest,
        ImportPath, LanguageFamily, LanguageProfile,
    },
};
use hashbrown::HashMap;
use std::{
    collections::BTreeSet,
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
    if candidates.len() <= 1 || !candidates.iter().all(|node| node.language == "rust") {
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
    let flags = crate::models::NodeDetailFlags::from_value(&node.details);
    flags.is_stub || flags.is_overload
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

/// Shared workspace inputs passed to [`LanguageLinker::normalize_file_imports`].
pub struct ImportNormalizationContext<'ctx, 'a> {
    /// Optional workspace root for root-relative import resolution.
    pub root: Option<&'ctx Path>,
    /// Workspace dependency and manifest registry.
    pub dependencies: &'ctx DependencyRegistry,
    /// Indexed source profiles keyed by workspace-relative path.
    pub profiles: &'ctx HashMap<&'a str, &'static dyn LanguageProfile>,
    /// Indexed module stems keyed by [`LanguageFamily`].
    pub family_module_stems: &'ctx HashMap<LanguageFamily, std::collections::HashSet<&'a str>>,
    /// Indexed Rust `src/lib.rs` root modules keyed by workspace scope.
    #[cfg(feature = "lang-rust")]
    pub rust_library_modules: &'ctx HashMap<&'a str, (&'a str, &'a Node)>,
}

/// Borrowed inputs passed to [`LanguageLinker::compose_file_relations`].
#[allow(private_interfaces)]
pub struct FileRelationContext<'ctx, 'a> {
    /// Workspace-relative file path.
    pub path: &'a str,
    /// Extracted file facts.
    pub facts: &'a Facts,
    /// Catalog-restored or file-local references.
    pub references: &'ctx [Reference],
    /// Workspace dependency registry.
    pub dependencies: &'ctx DependencyRegistry,
    /// Shared semantic context.
    pub semantic_context: &'ctx super::semantic_context::Context<'a>,
    /// Nodes grouped by file path.
    pub by_module: &'ctx HashMap<&'a str, Vec<&'a Node>>,
    /// Nodes grouped by interned symbol ID.
    pub by_id: &'ctx HashMap<SymbolId, &'a Node>,
    /// Prepared framework semantic facts.
    pub prepared_semantic_facts: &'ctx super::framework_relations::PreparedSemanticFacts<'a>,
    /// Mutable per-file output graph.
    pub graph: &'ctx mut crate::models::compact_graph::CompactGraph,
}

/// Owns language-specific import, receiver, and incremental-linking rules.
pub trait LanguageLinker: Send + Sync {
    /// Reports whether multi-file packages/namespaces are admitted as single-package imports.
    fn allows_multi_file_modules(&self) -> bool {
        false
    }

    /// Reports whether pre-indexed module stems for this profile's family are needed during import normalization.
    fn needs_family_module_stems(&self) -> bool {
        false
    }

    /// Normalizes all import references for a single file into `out`.
    fn normalize_file_imports<'a>(
        &self,
        profile: &dyn LanguageProfile,
        path: &'a str,
        facts: &'a Facts,
        ctx: &ImportNormalizationContext<'_, 'a>,
        out: &mut HashMap<(&'a str, &'a str), Option<ImportPath>>,
    ) {
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
        {
            let module = reference.module.as_deref().unwrap_or(&reference.expression);
            out.entry((path, module)).or_insert_with(|| {
                normalize_import_with_prefixes(profile, path, module, ctx.root, ctx.dependencies)
            });
        }
    }

    /// Appends language-specific module namespace aliases during namespace index construction.
    fn module_namespaces(
        &self,
        _profile: &dyn LanguageProfile,
        _node: &Node,
        _dependency_registry: &DependencyRegistry,
        _out: &mut Vec<String>,
    ) {
    }

    /// Resolves a builtin receiver property type when supported by the language's semantic context.
    #[allow(private_interfaces)]
    fn resolve_builtin_property<'a>(
        &self,
        _path: &str,
        _receiver_name: &str,
        _field: &str,
        _semantic_context: &super::semantic_context::Context<'a>,
    ) -> Option<super::value_flow::TypeTarget<'a>> {
        None
    }

    /// Classifies builtin receiver origin tags for resolved builtin member references.
    fn builtin_member_origin(
        &self,
        _receiver: &super::value_flow::TypeTarget<'_>,
        default_origin: Option<&'static str>,
        _computed_call: bool,
    ) -> Option<&'static str> {
        default_origin
    }

    /// Appends language-specific cross-symbol semantic relations for one file.
    fn compose_file_relations<'a>(&self, _ctx: &mut FileRelationContext<'_, 'a>) {}

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
        _all: &'a dyn crate::FactsLookup,
        _modules_by_namespace: &ModulesByNamespace<'a>,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _root: Option<&Path>,
    ) -> PackageExports<'a> {
        HashMap::new()
    }

    /// Returns files whose full facts are required to relink affected owners.
    fn required_full_facts(
        &self,
        _facts: &dyn crate::FactsLookup,
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

impl LanguageLinker for GenericLanguageLinker {
    fn allows_multi_file_modules(&self) -> bool {
        true
    }

    fn module_namespaces(
        &self,
        profile: &dyn LanguageProfile,
        node: &Node,
        dependency_registry: &DependencyRegistry,
        out: &mut Vec<String>,
    ) {
        for mp in dependency_registry.module_prefixes_for_path(profile.family(), &node.path) {
            if let Some((full, rel_mod)) = prefixed_module_namespace(profile, &node.path, mp) {
                for alias in [full, rel_mod] {
                    if !alias.is_empty() && !out.contains(&alias) {
                        out.push(alias);
                    }
                }
            }
        }
    }

    fn tracks_external_aliases(&self) -> bool {
        true
    }

    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        resolve_default_import_with_options(context, true)
    }
}

pub(crate) fn normalize_prefix_namespace(prefix: &str) -> String {
    prefix
        .trim_end_matches(['/', '\\', '.', ':'])
        .replace(['/', '\\', ':'], ".")
}

fn strip_prefix_segments<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return None;
    }
    if value == prefix {
        return Some("");
    }
    value
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
}

pub(crate) fn prefixed_module_namespace(
    profile: &dyn LanguageProfile,
    path: &str,
    mp: &ModulePrefix,
) -> Option<(String, String)> {
    let (_, local_path) = languages::workspace_path(path);
    let (_, root_local) = languages::workspace_path(&mp.source_root);
    let rel_path = if root_local.is_empty() {
        local_path
    } else {
        local_path
            .strip_prefix(root_local)
            .and_then(|rest| rest.strip_prefix('/'))?
    };
    let rel_mod = profile.module_name(rel_path);
    let norm_prefix = normalize_prefix_namespace(&mp.prefix);
    if norm_prefix.is_empty() {
        return None;
    }
    let full = if rel_mod.is_empty() {
        norm_prefix
    } else {
        format!("{norm_prefix}.{rel_mod}")
    };
    Some((full, rel_mod))
}

fn map_module_prefix(
    namespace: &str,
    prefixes: &[&ModulePrefix],
    symbol_path: bool,
) -> Option<ImportPath> {
    for mp in prefixes {
        let norm_prefix = normalize_prefix_namespace(&mp.prefix);
        if let Some(rest) = strip_prefix_segments(namespace, &norm_prefix) {
            let (_, root_local) = languages::workspace_path(&mp.source_root);
            let root_ns = if root_local.is_empty() {
                String::new()
            } else {
                normalize_prefix_namespace(root_local)
            };
            let mapped = match (root_ns.is_empty(), rest.is_empty()) {
                (true, true) => String::new(),
                (true, false) => rest.to_owned(),
                (false, true) => root_ns,
                (false, false) => format!("{root_ns}.{rest}"),
            };
            if !mapped.is_empty() {
                return Some(ImportPath {
                    namespace: mapped,
                    relative: false,
                    symbol_path,
                });
            }
        }
    }
    None
}

pub(crate) fn normalize_import_with_prefixes(
    profile: &dyn LanguageProfile,
    path: &str,
    module: &str,
    root: Option<&Path>,
    dependencies: &DependencyRegistry,
) -> Option<ImportPath> {
    let prefixes = dependencies.module_prefixes_for_path(profile.family(), path);
    if let Some(normalized) = profile.normalize_import_with_root(root, path, module) {
        if !normalized.relative && !prefixes.is_empty() {
            if let Some(mapped) =
                map_module_prefix(&normalized.namespace, &prefixes, normalized.symbol_path)
            {
                return Some(mapped);
            }
        }
        return Some(normalized);
    }
    (!prefixes.is_empty())
        .then(|| map_module_prefix(&normalize_prefix_namespace(module), &prefixes, true))
        .flatten()
}

pub(crate) fn resolve_default_import<'ctx, 'a, 'input>(
    context: &mut ImportContext<'ctx, 'a, 'input>,
) -> ImportResolution<'a> {
    resolve_default_import_with_options(context, false)
}

pub(crate) fn resolve_default_import_with_options<'ctx, 'a, 'input>(
    context: &mut ImportContext<'ctx, 'a, 'input>,
    allow_multi_file: bool,
) -> ImportResolution<'a> {
    let Some(tail) = context.tail().map(str::to_owned) else {
        return ImportResolution::default();
    };
    if context.modules.is_empty() || (!allow_multi_file && context.modules.len() != 1) {
        return ImportResolution::default();
    }
    if tail.is_empty() || tail == "*" {
        context.candidates.extend(context.modules.iter().copied());
        return ImportResolution::default();
    }
    if let Some(owner_tail) = tail.strip_suffix(".*") {
        for module in context.modules.iter().copied() {
            context.lookup_key.clear();
            context.lookup_key.push_str(&module.qualname);
            if !owner_tail.is_empty() {
                context.lookup_key.push('.');
                context.lookup_key.push_str(owner_tail);
            }
            context.lookup_key.push('.');
            let prefix = context.lookup_key.as_str();
            context.candidates.extend(
                context
                    .by_module
                    .get(module.path.as_str())
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| {
                        node.kind != NodeKind::Component.as_str()
                            && node.qualname.starts_with(prefix)
                    }),
            );
        }
        return ImportResolution::default();
    }
    for module in context.modules.iter().copied() {
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
                        node.kind != NodeKind::Component.as_str() && node.qualname == target
                    }
                }),
        );
    }
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

    #[test]
    fn generic_linker_resolves_multi_file_wildcard_and_external_alias_contracts() {
        use super::{GENERIC_LINKER, ImportContext, LanguageLinker, ModulesByNamespace, PackageExports};
        use crate::interner::LinkerInterner;
        use crate::languages::{dependency_registry::DependencyRegistry, ImportPath, LanguageFamily};
        use crate::models::{Node, Reference};
        use hashbrown::HashMap;

        assert!(GENERIC_LINKER.allows_multi_file_modules());
        assert!(GENERIC_LINKER.tracks_external_aliases());

        let mod_a = Node {
            native_semantic: None,
            id: "mod:a".into(),
            kind: "module".into(),
            name: "a".into(),
            qualname: "acme.util".into(),
            path: "acme/util/a.go".into(),
            line: 1,
            end_line: 10,
            is_test: false,
            language: "go".into(),
            generated: false,
            details: serde_json::json!({}),
        };
        let mod_b = Node {
            path: "acme/util/b.go".into(),
            id: "mod:b".into(),
            name: "b".into(),
            ..mod_a.clone()
        };
        let fn_alpha = Node {
            id: "fn:alpha".into(),
            kind: "function".into(),
            name: "Alpha".into(),
            qualname: "acme.util.Alpha".into(),
            ..mod_a.clone()
        };
        let fn_beta = Node {
            id: "fn:beta".into(),
            kind: "function".into(),
            name: "Beta".into(),
            qualname: "acme.util.Beta".into(),
            ..mod_b.clone()
        };

        let mut modules = vec![&mod_a, &mod_b];
        let mut by_module: HashMap<&str, Vec<&Node>> = HashMap::new();
        by_module.insert("acme/util/a.go", vec![&mod_a, &fn_alpha]);
        by_module.insert("acme/util/b.go", vec![&mod_b, &fn_beta]);
        let by_qual: HashMap<&str, Vec<&Node>> = HashMap::new();
        let modules_by_namespace: ModulesByNamespace<'_> = HashMap::new();
        let package_exports: PackageExports<'_> = HashMap::new();
        let interner = LinkerInterner::default();
        let dependencies = DependencyRegistry::default();

        // Multi-file package wildcard `acme.util.*` resolves declarations across both files.
        let reference = Reference {
            source: "mod:main".into(),
            kind: "imports".into(),
            expression: "*".into(),
            module: Some("acme.util.*".into()),
            alias: Some("*".into()),
            line: 1,
            column: 1,
            dynamic: false,
            receiver_hint: None,
        };
        let normalized = ImportPath {
            namespace: "acme.util.*".into(),
            relative: false,
            symbol_path: true,
        };
        #[allow(clippy::too_many_arguments)]
        fn resolve_ids<'a>(
            selected_namespace: &str,
            modules: &mut Vec<&'a Node>,
            interner: &LinkerInterner<'a>,
            dependencies: &DependencyRegistry,
            reference: &Reference,
            normalized: &ImportPath,
            modules_by_namespace: &ModulesByNamespace<'a>,
            by_module: &HashMap<&'a str, Vec<&'a Node>>,
            by_qual: &HashMap<&'a str, Vec<&'a Node>>,
            package_exports: &PackageExports<'a>,
        ) -> Vec<String> {
            let mut candidates = Vec::new();
            let mut lookup_key = String::new();
            GENERIC_LINKER.resolve_import(&mut ImportContext {
                path: "cmd/main.go",
                interner,
                dependencies,
                reference,
                #[cfg(feature = "lang-rust")]
                rust_forwarding: None,
                normalized: Some(normalized),
                selected_namespace,
                family: LanguageFamily("go"),
                modules,
                candidates: &mut candidates,
                modules_by_namespace,
                by_module,
                by_qual,
                package_exports,
                lookup_key: &mut lookup_key,
            });
            candidates.into_iter().map(|n| n.id.clone()).collect()
        }

        let root_a = Node {
            qualname: "acme".into(),
            ..mod_a.clone()
        };
        let root_b = Node {
            qualname: "acme".into(),
            ..mod_b.clone()
        };
        let mut root_modules = vec![&root_a, &root_b];
        for (selected_namespace, target_modules, expected) in [
            ("acme.util", &mut modules, ["mod:a", "mod:b"]),
            ("acme", &mut root_modules, ["fn:alpha", "fn:beta"]),
        ] {
            assert_eq!(
                resolve_ids(
                    selected_namespace,
                    target_modules,
                    &interner,
                    &dependencies,
                    &reference,
                    &normalized,
                    &modules_by_namespace,
                    &by_module,
                    &by_qual,
                    &package_exports,
                ),
                expected
            );
        }

        // Verify ModulePrefix stripping and GenericLanguageLinker::module_namespaces across ecosystems.
        let prefix_cases = [
            (
                super::ModulePrefix {
                    ecosystem: crate::languages::dependency_registry::Ecosystem::Go,
                    prefix: "example.com/app".into(),
                    source_root: "".into(),
                    origin: "go.mod".into(),
                },
                "example.com/app/pkg",
                "pkg",
            ),
            (
                super::ModulePrefix {
                    ecosystem: crate::languages::dependency_registry::Ecosystem::Php,
                    prefix: "App\\".into(),
                    source_root: "src".into(),
                    origin: "composer.json".into(),
                },
                "App\\Service\\User",
                "src.Service.User",
            ),
            (
                super::ModulePrefix {
                    ecosystem: crate::languages::dependency_registry::Ecosystem::CSharp,
                    prefix: "Acme.Core".into(),
                    source_root: "".into(),
                    origin: "App.csproj".into(),
                },
                "Acme.Core.Billing.Invoice",
                "Billing.Invoice",
            ),
        ];
        for (prefix, raw_import, expected_namespace) in prefix_cases {
            let norm = super::normalize_prefix_namespace(raw_import);
            let mapped = super::map_module_prefix(&norm, &[&prefix], true)
                .expect("module prefix maps workspace import");
            assert_eq!(mapped.namespace, expected_namespace);
        }
    }
}
