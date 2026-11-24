//! Core leaf types and pure helper functions shared across linker subsystems.

use crate::interner::{LinkerInterner, PathId, SymbolId};
use crate::languages::types::{ImportPath, LanguageFamily};
use crate::models::Node;
use hashbrown::HashMap;
use std::borrow::Cow;

/// Per-node symbol IDs collected for declaration and rebinding lookups.
pub type NodeBindingIndex = HashMap<SymbolId, Vec<SymbolId>>;

/// Package re-exports indexed by interned owner file and imported name.
pub type PackageExports<'a> = HashMap<PathId, HashMap<SymbolId, Option<&'a Node>>>;

/// Cross-file indexes available while resolving one import reference.
pub type ModulesByNamespace<'a> = HashMap<LanguageFamily, HashMap<String, Vec<&'a Node>>>;

/// Read-only property lookup interface decoupled from full semantic context.
// Public until Wave 2a narrows LanguageLinker to pub(crate).
pub trait BuiltinPropertyLookup {
    /// Returns the configured property type name for `receiver.member` in `path`.
    fn property_type_for_path(&self, path: &str, receiver: &str, member: &str) -> Option<String>;
}

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

/// Select the nearest indexed module providers using the importing file's workspace and path.
pub(crate) fn import_provider_modules<'a, 'n>(
    path: &str,
    normalized: &'n ImportPath,
    family: LanguageFamily,
    modules_by_namespace: &ModulesByNamespace<'a>,
) -> (Vec<&'a Node>, &'n str) {
    let workspace = crate::languages::types::workspace_path(path).0;
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
                        (crate::languages::types::workspace_path(&node.path).0 == workspace)
                            == local
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

pub(crate) fn qualified<'a>(buffer: &'a mut String, namespace: &str, name: &str) -> &'a str {
    buffer.clear();
    buffer.push_str(namespace);
    buffer.push('.');
    buffer.push_str(name);
    buffer
}

#[derive(Clone, Copy)]
pub(in crate::linker) enum ReceiverType<'a> {
    Local(&'a Node),
    External(&'a str, usize),
    Ambiguous,
    Unknown,
}

pub(in crate::linker) fn receiver_language_requires_static(node: &Node) -> bool {
    matches!(node.language.as_str(), "typescript" | "javascript" | "vue")
}

fn strip_trailing_generics(name: &str) -> &str {
    let Some((base, generic)) = name.split_once('<') else {
        return name;
    };
    if !name.ends_with('>') {
        return name;
    }
    let mut depth = 1usize;
    let closes_at_end = generic.char_indices().all(|(index, character)| {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 && index + character.len_utf8() != generic.len() {
                    return false;
                }
            }
            _ => {}
        }
        depth > 0 || index + character.len_utf8() == generic.len()
    }) && depth == 0;
    if closes_at_end {
        base.trim_end()
    } else {
        name
    }
}

pub(in crate::linker) fn strip_rust_references(raw: &str, require_lifetime_space: bool) -> Option<&str> {
    let mut name = raw.trim().trim_matches(['\'', '"']);
    while let Some(rest) = name.strip_prefix('&') {
        name = rest.trim_start();
        if name.starts_with('\'') {
            if let Some((_, after)) = name.split_once(char::is_whitespace) {
                name = after.trim_start();
            } else if require_lifetime_space {
                return None;
            }
        }
        name = name.strip_prefix("mut ").unwrap_or(name).trim_start();
    }
    Some(name)
}

pub(crate) fn receiver_type_name(annotation: &str) -> Option<Cow<'_, str>> {
    let mut name = strip_rust_references(annotation, true)?;
    name = strip_trailing_generics(name);
    if let Some((base, _)) = name.split_once('[') {
        name = base.trim_end();
    }

    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '_' | '.' | ':' | '$')
        })
    {
        return None;
    }
    Some(if name.contains("::") {
        Cow::Owned(name.replace("::", "."))
    } else {
        Cow::Borrowed(name)
    })
}

pub(in crate::linker) fn receiver_head(annotation: &str) -> &str {
    let name = strip_rust_references(annotation, false).unwrap_or("");
    let head = name
        .split_once("::")
        .or_else(|| name.split_once('.'))
        .map(|(h, _)| h)
        .unwrap_or(name);
    let head = head.split_once('<').map(|(h, _)| h).unwrap_or(head);
    head.split_once('[').map(|(h, _)| h).unwrap_or(head).trim()
}
