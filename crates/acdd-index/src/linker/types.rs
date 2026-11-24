//! Core leaf types and pure helper functions shared across linker subsystems.

use crate::interner::{LinkerInterner, PathId, SymbolId};
use crate::languages::types::{ImportPath, LanguageFamily};
use crate::models::Node;
use hashbrown::HashMap;
use std::borrow::Cow;

/// Per-node symbol IDs collected for declaration and rebinding lookups.
pub type NodeBindingIndex = HashMap<SymbolId, Vec<SymbolId>>;

pub(crate) fn has_node_binding(interner: &LinkerInterner<'_>, bindings: &NodeBindingIndex, owner: &str, name: &str) -> bool {
    let (Some(owner), Some(name)) = (interner.lookup_symbol(owner), interner.lookup_symbol(name)) else {
        return false;
    };
    bindings.get(&owner).is_some_and(|bindings| bindings.binary_search(&name).is_ok())
}

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

pub(crate) fn qualified<'a>(buffer: &'a mut String, namespace: &str, name: &str) -> &'a str {
    buffer.clear();
    buffer.push_str(namespace);
    buffer.push('.');
    buffer.push_str(name);
    buffer
}

#[derive(Clone, Copy)]
pub(crate) enum ReceiverType<'a> {
    Local(&'a Node),
    External(&'a str, usize),
    Ambiguous,
    Unknown,
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
