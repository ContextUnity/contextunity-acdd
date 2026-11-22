use super::super::semantic_context::LanguageMemberLookup;
use super::super::value_flow::SemanticResolver;
use crate::languages::LanguageProfile;
use crate::models::{compact_graph::CompactGraph, Node, ReceiverHint, Reference};
use hashbrown::HashMap;

pub use crate::languages::rust::declarative_macro_classification;

pub(in crate::linker) fn dynamic_receiver_member<'a>(
    input: &super::super::resolution::ResolutionInput<'a>,
    path: &str,
    reference: &Reference,
    owner: Option<&'a Node>,
    graph: &mut CompactGraph,
) -> bool {
    use super::super::{semantic_context::Member, value_flow::TypeTarget};

    let Some(owner) = owner.filter(|owner| owner.language == "rust") else {
        return false;
    };
    if reference.kind != "calls" {
        return false;
    }
    let (receiver, member) = if let Some((member, receiver)) =
        reference.receiver_hint.as_ref().and_then(|hint| {
            let ReceiverHint::CallResult { member, .. } = hint else {
                return None;
            };
            input
                .value_flow
                .computed_receiver(reference)
                .filter(|receiver| !matches!(receiver, TypeTarget::Unknown))
                .map(|receiver| (member, receiver))
        }) {
        (std::borrow::Cow::Borrowed(receiver), member.as_str())
    } else {
        let Some((receiver, member)) = reference.expression.rsplit_once('.') else {
            return false;
        };
        let Some(value) = fallback_receiver_value(receiver, reference.position()) else {
            return false;
        };
        (
            std::borrow::Cow::Owned(input.value_flow.resolve_value(
                owner,
                &value,
                reference.position(),
                input.semantic_context,
            )),
            member,
        )
    };
    if matches!(
        receiver.as_ref(),
        TypeTarget::Unknown | TypeTarget::Ambiguous | TypeTarget::Candidates { .. }
    ) {
        return false;
    }
    let result = input
        .semantic_context
        .lookup_member(receiver.as_ref(), member, owner);
    if matches!(result, Member::Unknown) {
        return false;
    }
    if let Member::Local(node) = result {
        if matches!(
            node.kind.as_str(),
            "method" | "function" | "function_item" | "function_signature_item"
        ) && !input.semantic_context.is_rust_inherent_member(node)
        {
            return false;
        }
    }
    super::super::emit_inferred(
        graph,
        path,
        reference,
        result,
        Some("Rust standard library"),
    );
    true
}

fn fallback_receiver_value(
    expression: &str,
    at: crate::semantic::SourcePosition,
) -> Option<crate::semantic::ValueExpr> {
    use crate::semantic::{CallSiteEvidence, TypeExpr, ValueExpr};
    if expression.len() > 512 {
        return None;
    }
    if !expression.contains('.') && !expression.contains("::") && expression.ends_with("()") {
        return None;
    }
    let mut value = None;
    for (index, raw_segment) in expression.split('.').enumerate() {
        if index > 6 {
            return None;
        }
        let mut segment = raw_segment;
        let mut unary_ops = [0u8; 4];
        let mut unary_len = 0usize;
        if index == 0 {
            loop {
                let trimmed = segment.trim();
                if let Some(inner) = trimmed
                    .strip_prefix('(')
                    .and_then(|s| s.strip_suffix(')'))
                    .filter(|inner| !inner.contains('(') && !inner.contains(')'))
                {
                    segment = inner.trim();
                    continue;
                }
                if let Some(inner) = trimmed.strip_prefix("*").filter(|_| unary_len < 4) {
                    unary_ops[unary_len] = b'*';
                    unary_len += 1;
                    segment = inner.trim_start();
                    continue;
                }
                if let Some(inner) = trimmed.strip_prefix("&mut ").filter(|_| unary_len < 4) {
                    unary_ops[unary_len] = b'm';
                    unary_len += 1;
                    segment = inner.trim_start();
                    continue;
                }
                if let Some(inner) = trimmed.strip_prefix('&').filter(|_| unary_len < 4) {
                    unary_ops[unary_len] = b'&';
                    unary_len += 1;
                    segment = inner.trim_start();
                    continue;
                }
                segment = trimmed;
                break;
            }
        }
        let (segment, try_result) = segment
            .strip_suffix('?')
            .map_or((segment, false), |s| (s, true));
        let (name, called) = segment
            .strip_suffix("()")
            .map_or((segment, false), |s| (s, true));
        let is_str_literal = index == 0 && segment.starts_with('"') && segment.ends_with('"');
        if !is_str_literal
            && (name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || (index == 0 && c == ':')))
        {
            return None;
        }
        let mut next = match value {
            None if is_str_literal => ValueExpr::Annotated {
                type_expr: TypeExpr::Named {
                    name: "str".to_owned(),
                },
            },
            None if called => ValueExpr::Call {
                callee: name.to_owned(),
                site: None,
            },
            None => ValueExpr::Alias {
                name: name.to_owned(),
            },
            Some(inner) if called && super::value_flow::is_adapter(name) => ValueExpr::Project {
                value: Box::new(inner),
                member: name.to_owned(),
            },
            Some(inner) if called => ValueExpr::Call {
                callee: name.to_owned(),
                site: Some(CallSiteEvidence {
                    position: at,
                    receiver: Some(Box::new(inner)),
                    arguments: Vec::new(),
                }),
            },
            Some(inner) => ValueExpr::Project {
                value: Box::new(inner),
                member: format!("__rust_field:{name}"),
            },
        };
        for &op in unary_ops[..unary_len].iter().rev() {
            let member = match op {
                b'*' => "__rust_deref",
                b'm' => "__rust_ref_mut",
                _ => "__rust_ref",
            };
            next = ValueExpr::Project {
                value: Box::new(next),
                member: member.to_owned(),
            };
        }
        value = Some(if try_result {
            ValueExpr::Project {
                value: Box::new(next),
                member: "?".to_owned(),
            }
        } else {
            next
        });
    }
    value
}

/// Helper to verify registered Cargo ecosystem crates statically from ecosystem list
pub fn is_registered_cargo_crate(crate_name: &str) -> bool {
    matches!(
        crate_name,
        "rusqlite"
            | "tree-sitter"
            | "tree_sitter"
            | "serde"
            | "serde_json"
            | "serde_yaml"
            | "tokio"
            | "anyhow"
            | "clap"
            | "tracing"
            | "rayon"
            | "tempfile"
            | "regex"
            | "thiserror"
            | "parking_lot"
            | "crossbeam"
            | "log"
            | "git2"
            | "walkdir"
            | "ignore"
            | "sha2"
            | "hex"
            | "toml"
            | "uuid"
            | "chrono"
            | "hashbrown"
            | "indexmap"
            | "petgraph"
            | "libc"
            | "dirs"
            | "pulldown-cmark"
            | "pulldown_cmark"
            | "schemars"
            | "rmcp"
            | "zstd"
            | "cc"
            | "bstr"
            | "futures"
            | "syn"
            | "quote"
            | "proc_macro2"
            | "proc-macro2"
            | "base64"
            | "bytes"
            | "http"
            | "tower"
            | "axum"
            | "hyper"
            | "url"
    )
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) struct RustExplicitTarget {
    pub(in crate::linker) namespace: String,
    pub(in crate::linker) workspace: String,
}

#[cfg(feature = "lang-rust")]
fn explicit_target_from_sibling_crate(
    input: &super::super::resolution::ResolutionInput<'_>,
    path: &str,
    first: &str,
    tail: &str,
) -> Option<RustExplicitTarget> {
    let (pkg_scope, own_lib) = input
        .dependency_registry
        .sibling_rust_package_library_for_path(path)?;
    if own_lib != first {
        return None;
    }
    let lib_path = if pkg_scope.is_empty() {
        std::borrow::Cow::Borrowed("src/lib.rs")
    } else {
        std::borrow::Cow::Owned(format!("{pkg_scope}/src/lib.rs"))
    };
    if !input.profiles.contains_key(lib_path.as_ref()) {
        return None;
    }
    let base_namespace = if pkg_scope.is_empty() {
        "src".to_owned()
    } else {
        format!("{}.src", pkg_scope.replace('/', "."))
    };
    Some(RustExplicitTarget {
        namespace: format!("{base_namespace}.{}", tail.replace("::", ".")),
        workspace: crate::languages::workspace_path(&lib_path).0.to_owned(),
    })
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn rust_explicit_module_target(
    input: &super::super::resolution::ResolutionInput<'_>,
    profile: Option<&dyn LanguageProfile>,
    path: &str,
    file_module: &str,
    expression: &str,
) -> Option<RustExplicitTarget> {
    let profile = profile.filter(|profile| profile.id() == "rust")?;
    let local_workspace = crate::languages::workspace_path(path).0;
    if let Some(mut tail) = expression.strip_prefix("super::") {
        let mut module = file_module.rsplit_once('.')?.0;
        while let Some(rest) = tail.strip_prefix("super::") {
            module = module.rsplit_once('.')?.0;
            tail = rest;
        }
        return Some(RustExplicitTarget {
            namespace: format!("{module}.{}", tail.replace("::", ".")),
            workspace: local_workspace.to_owned(),
        });
    }
    if let Some(tail) = expression.strip_prefix("self::") {
        return Some(RustExplicitTarget {
            namespace: format!("{file_module}.{}", tail.replace("::", ".")),
            workspace: local_workspace.to_owned(),
        });
    }
    if expression.starts_with("crate::") {
        return profile
            .normalize_import(path, expression)
            .map(|module| RustExplicitTarget {
                namespace: module.namespace,
                workspace: local_workspace.to_owned(),
            });
    }
    let (first, tail) = expression.split_once("::")?;
    if tail.is_empty() || first == "Self" {
        return None;
    }
    if let Some(target) = explicit_target_from_sibling_crate(input, path, first, tail) {
        return Some(target);
    }
    let mut targets = input
        .rust_library_modules
        .iter()
        .filter(|(_, (library, _))| *library == first);
    if let Some((target_workspace, (library, _))) = targets.next() {
        if targets.next().is_none()
            && (input.dependency_registry.sibling_rust_library_for_path(path) == Some(*library)
                || input.dependency_registry.declares_for_path(
                    crate::languages::LanguageFamily("rust"),
                    path,
                    first,
                ))
        {
            let base_namespace = if target_workspace.is_empty() {
                "src".to_owned()
            } else {
                format!("{}.src", target_workspace.replace('/', "."))
            };
            return Some(RustExplicitTarget {
                namespace: format!("{base_namespace}.{}", tail.replace("::", ".")),
                workspace: (*target_workspace).to_owned(),
            });
        }
    }
    if let Some((namespace, workspace)) =
        crate::languages::rust::linker::resolve_workspace_crate_namespace(
            input.profiles.keys().copied(),
            input.dependency_registry,
            path,
            first,
            tail,
        )
    {
        return Some(RustExplicitTarget {
            namespace,
            workspace,
        });
    }
    let root_crate = input.dependency_registry.root_rust_crate_for_path(path)?;
    if first != root_crate {
        return None;
    }
    let lib_path = if local_workspace.is_empty() {
        std::borrow::Cow::Borrowed("src/lib.rs")
    } else {
        std::borrow::Cow::Owned(format!("{local_workspace}/src/lib.rs"))
    };
    if !input.profiles.contains_key(lib_path.as_ref()) {
        return None;
    }
    let base_namespace = if local_workspace.is_empty() {
        "src".to_owned()
    } else {
        format!("{local_workspace}.src")
    };
    Some(RustExplicitTarget {
        namespace: format!("{base_namespace}.{}", tail.replace("::", ".")),
        workspace: local_workspace.to_owned(),
    })
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn explicit_reexport_targets<'a>(
    input: &super::super::resolution::ResolutionInput<'a>,
    file_module: &str,
    target: &RustExplicitTarget,
    lookup_key: &mut String,
) -> Vec<&'a Node> {
    let Some(rust_modules) = input
        .modules_by_namespace
        .get(&crate::languages::LanguageFamily("rust"))
    else {
        return Vec::new();
    };
    let mut split = target.namespace.len();
    let mut out = Vec::new();
    while let Some(dot) = target.namespace[..split].rfind('.') {
        let module_ns = &target.namespace[..dot];
        let rest = &target.namespace[dot + 1..];
        if let Some(modules) = rust_modules.get(module_ns) {
            let (head, tail) = rest.split_once('.').unwrap_or((rest, ""));
            for module in modules
                .iter()
                .filter(|m| crate::languages::workspace_path(&m.path).0 == target.workspace)
            {
                for forwarded in input.rust_forwarding.targets(module, head, file_module) {
                    if tail.is_empty() {
                        out.push(*forwarded);
                    } else {
                        lookup_key.clear();
                        lookup_key.push_str(&forwarded.qualname);
                        lookup_key.push('.');
                        lookup_key.push_str(tail);
                        if let Some(nodes) = input.by_qual.get(lookup_key.as_str()) {
                            out.extend_from_slice(nodes);
                        }
                    }
                }
            }
            if !out.is_empty() {
                break;
            }
        }
        split = dot;
    }
    out
}

fn emit_self_unresolved(graph: &mut CompactGraph, path: &str, reference: &Reference) -> bool {
    graph.reference_coverage(
        path,
        reference,
        "unresolved",
        "Self has no indexed impl receiver".into(),
    );
    true
}

fn emit_self_target(
    graph: &mut CompactGraph,
    path: &str,
    reference: &Reference,
    receiver: &Node,
    kind: &str,
    evidence: String,
) -> bool {
    graph.reference_edge(path, reference, receiver.id.clone(), kind, "inferred");
    graph.reference_coverage(path, reference, "resolved", evidence.into());
    true
}

pub(in crate::linker) fn resolve_self_reference<'a>(
    profile: Option<&dyn LanguageProfile>,
    reference: &Reference,
    owner: Option<&'a Node>,
    path: &str,
    member_lookup: &LanguageMemberLookup<'_, 'a>,
    graph: &mut CompactGraph,
) -> bool {
    if profile.is_some_and(|profile| profile.id() == "rust") && reference.kind == "calls" {
        if let Some(crate::linker::value_flow::Symbol::Callable(target)) =
            owner.and_then(|owner| member_lookup.rust_alias_member(owner, reference))
        {
            graph.reference_edge(path, reference, target.id.clone(), "calls", "exact");
            graph.reference_coverage(
                path,
                reference,
                "resolved",
                format!("Rust type alias associated member: {}", target.qualname).into(),
            );
            return true;
        }
    }
    if !profile.is_some_and(|profile| profile.id() == "rust")
        || !(reference.expression == "Self"
            || reference.expression.starts_with("Self::")
            || reference.expression.starts_with("Self."))
    {
        return false;
    }
    let Some(owner) = owner else {
        return emit_self_unresolved(graph, path, reference);
    };
    let Some(receiver) = member_lookup.rust_receiver(owner) else {
        if let Some(recv_ty) = owner
            .details
            .get("receiver_type")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|ty| profile.is_some_and(|p| p.builtin_type(ty)))
        {
            let member = reference
                .expression
                .strip_prefix("Self::")
                .or_else(|| reference.expression.strip_prefix("Self."));
            let valid = member.is_none_or(|member| {
                profile.is_some_and(|p| p.builtin_member(recv_ty, member))
                    || matches!(member, "try_from" | "from" | "default")
            });
            if valid {
                graph.reference_coverage(
                    path,
                    reference,
                    "external",
                    format!(
                        "Rust standard library built-in: {} on {recv_ty}",
                        reference.expression
                    )
                    .into(),
                );
                return true;
            }
        }
        return emit_self_unresolved(graph, path, reference);
    };
    let member = reference
        .expression
        .strip_prefix("Self::")
        .or_else(|| reference.expression.strip_prefix("Self."));
    if let Some(member) = member {
        let targets = member_lookup.rust(receiver, member, true, owner);
        match targets {
            [target] => {
                graph.reference_edge(
                    path,
                    reference,
                    target.id.clone(),
                    reference.kind.as_str(),
                    "inferred",
                );
                graph.reference_coverage(
                    path,
                    reference,
                    "resolved",
                    format!(
                        "Self::{member} resolves to declared associated member on {}",
                        receiver.qualname
                    )
                    .into(),
                );
            }
            [] => graph.reference_coverage(
                path,
                reference,
                "unresolved",
                format!(
                    "Self::{member} has no declared associated member on {}",
                    receiver.qualname
                )
                .into(),
            ),
            _ => graph.reference_coverage(
                path,
                reference,
                "ambiguous",
                format!(
                    "Self::{member} has multiple visible associated members on {}",
                    receiver.qualname
                )
                .into(),
            ),
        }
        return true;
    }
    if reference.kind == "calls" && matches!(receiver.kind.as_str(), "struct" | "enum") {
        return emit_self_target(
            graph,
            path,
            reference,
            receiver,
            "calls",
            format!("Self calls indexed constructor {}", receiver.qualname),
        );
    }
    if reference.kind == "references" {
        return emit_self_target(
            graph,
            path,
            reference,
            receiver,
            "references",
            format!("Self refers to indexed receiver type {}", receiver.qualname),
        );
    }
    false
}

fn emit_rust_const_match(
    graph: &mut CompactGraph,
    path: &str,
    reference: &Reference,
    target: &Node,
    reason: String,
) -> bool {
    graph.reference_edge(
        path,
        reference,
        target.id.clone(),
        reference.kind.as_str(),
        "exact",
    );
    graph.reference_coverage(path, reference, "resolved", reason.into());
    true
}

fn resolve_module_scoped_constant<'a>(
    expr: &str,
    owner: &Node,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
) -> Option<&'a Node> {
    let mut segments = expr.split("::");
    let first = segments.next()?;
    let mut cur_scope = owner.qualname.as_str();
    while !cur_scope.is_empty() {
        let Some(mod_node) = semantic_context
            .nodes_in_path(cur_scope, path)
            .find(|n| matches!(n.kind.as_str(), "module" | "module_declaration"))
        else {
            cur_scope = cur_scope.rsplit_once('.').map_or("", |(p, _)| p);
            continue;
        };
        let Some(super::super::semantic_context::Imported::Nodes(targets, _, _)) =
            semantic_context.imported_alias(mod_node, first)
        else {
            return None;
        };
        let mut current: Vec<&Node> = targets.clone();
        let rest: Vec<&str> = segments.collect();
        for (idx, seg) in rest.iter().enumerate() {
            if idx + 1 == rest.len() {
                for parent in &current {
                    let q = format!("{}.{seg}", parent.qualname);
                    if let Some(target) = semantic_context
                        .nodes_in_path(&q, &parent.path)
                        .find(|n| n.kind == "constant")
                    {
                        return Some(target);
                    }
                }
                return None;
            }
            let mut next = Vec::new();
            for parent in &current {
                if matches!(parent.kind.as_str(), "module" | "module_declaration") {
                    if let Some(super::super::semantic_context::Imported::Nodes(sub, _, _)) =
                        semantic_context.imported_alias(parent, seg)
                    {
                        next.extend(sub.iter().copied());
                    }
                }
            }
            current = next;
        }
        return None;
    }
    None
}

fn resolve_qualified_rust_constant<'a>(
    reference: &Reference,
    owner: &Node,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
    graph: &mut CompactGraph,
) -> bool {
    let expr = reference.expression.as_str();
    let Some((head, member)) = expr.rsplit_once("::") else {
        return false;
    };
    if let crate::linker::value_flow::Symbol::Type(
        crate::linker::value_flow::TypeTarget::Local(type_node),
    ) = semantic_context.resolve_symbol(
        owner,
        head,
        reference.position(),
        crate::linker::value_flow::SymbolRole::Type,
    ) {
        if reference.kind == "references" || type_node.kind == "enum" {
            let qualified = format!("{}.{}", type_node.qualname, member);
            if let Some(target) = semantic_context
                .nodes_in_path(&qualified, &type_node.path)
                .find(|node| node.kind == "constant")
            {
                return emit_rust_const_match(
                    graph,
                    path,
                    reference,
                    target,
                    format!("Rust associated constant/variant {}", target.qualname),
                );
            }
        }
    }
    if reference.kind == "references" {
        if let Some(target) =
            resolve_module_scoped_constant(expr, owner, path, semantic_context)
        {
            return emit_rust_const_match(
                graph,
                path,
                reference,
                target,
                format!("Rust module constant {}", target.qualname),
            );
        }
    }
    false
}

pub(super) fn resolve_rust_constant<'a>(
    reference: &Reference,
    owner: Option<&Node>,
    path: &str,
    value_flow: &crate::linker::value_flow::ValueFlowIndex<'a>,
    semantic_context: &super::super::semantic_context::Context<'a>,
    graph: &mut CompactGraph,
) -> bool {
    if !matches!(reference.kind.as_str(), "calls" | "references")
        || reference.expression.contains('.')
    {
        return false;
    }
    let Some(owner) = owner else {
        return false;
    };
    let expr = reference.expression.as_str();
    if expr.contains("::") {
        return resolve_qualified_rust_constant(reference, owner, path, semantic_context, graph);
    }
    if reference.kind != "references"
        || value_flow
            .lookup(owner, expr, reference.position())
            .is_some()
    {
        return false;
    }
    let mut cur_scope = owner.qualname.as_str();
    while !cur_scope.is_empty() {
        let q = format!("{cur_scope}.{expr}");
        if let Some(target) = semantic_context
            .nodes_in_path(&q, path)
            .find(|node| node.kind == "constant")
        {
            return emit_rust_const_match(
                graph,
                path,
                reference,
                target,
                format!("Rust lexical constant {}", target.qualname),
            );
        }
        if let Some(mod_node) = semantic_context
            .nodes_in_path(cur_scope, path)
            .find(|n| matches!(n.kind.as_str(), "module" | "module_declaration"))
        {
            if let Some(super::super::semantic_context::Imported::Nodes(targets, pos, _)) =
                semantic_context.imported_alias(mod_node, expr)
            {
                if *pos <= reference.position() {
                    if let Some(target) = targets.iter().copied().find(|n| n.kind == "constant") {
                        return emit_rust_const_match(
                            graph,
                            path,
                            reference,
                            target,
                            format!("Rust imported constant {}", target.qualname),
                        );
                    }
                }
            }
        }
        cur_scope = cur_scope.rsplit_once('.').map_or("", |(p, _)| p);
    }
    false
}

fn find_macro_in_module<'a>(
    mod_node: &'a Node,
    mod_qualname: &str,
    tail: &str,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
) -> Option<&'a Node> {
    let direct = format!("{mod_qualname}.{tail}");
    if let Some(m) = semantic_context
        .nodes_in_path(&direct, path)
        .find(|n| n.kind == "macro")
    {
        return Some(m);
    }
    let re_imp = semantic_context.imported_alias(mod_node, tail)?;
    match re_imp {
        super::super::semantic_context::Imported::Nodes(re_nodes, _, _) => {
            re_nodes.iter().copied().find(|n| n.kind == "macro")
        }
        super::super::semantic_context::Imported::External(target_path, _, _, _) => {
            let re_clean = target_path
                .strip_prefix("crate::")
                .unwrap_or(target_path)
                .replace("::", ".");
            let full_re = format!("{mod_qualname}.{re_clean}");
            semantic_context
                .nodes_in_path(&full_re, path)
                .find(|n| n.kind == "macro")
        }
        _ => None,
    }
}

fn collect_macro_scopes(owner: Option<&Node>) -> Vec<&str> {
    let mut scopes = Vec::new();
    if let Some(o) = owner {
        let mut cur = o.qualname.as_str();
        loop {
            scopes.push(cur);
            if let Some((parent, _)) = cur.rsplit_once('.') {
                cur = parent;
            } else {
                break;
            }
        }
    }
    scopes.push("");
    scopes
}

fn find_direct_macro_in_scopes<'a>(
    scopes: &[&str],
    clean: &str,
    head: &str,
    tail: &str,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
) -> Option<&'a Node> {
    for scope in scopes {
        let full_name = if scope.is_empty() {
            clean.to_owned()
        } else {
            format!("{scope}.{clean}")
        };
        if let Some(node) = semantic_context
            .nodes_in_path(&full_name, path)
            .find(|n| n.kind == "macro")
        {
            return Some(node);
        }
        if tail.is_empty() {
            continue;
        }
        let mod_name = if scope.is_empty() {
            head.to_string()
        } else {
            format!("{scope}.{head}")
        };
        for mod_node in semantic_context.nodes_in_path(&mod_name, path) {
            if matches!(mod_node.kind.as_str(), "module" | "module_declaration") {
                if let Some(m) =
                    find_macro_in_module(mod_node, &mod_node.qualname, tail, path, semantic_context)
                {
                    return Some(m);
                }
            }
        }
    }
    None
}

fn collect_visible_macro_owners<'a>(
    scopes: &[&str],
    owner: Option<&'a Node>,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
) -> Vec<&'a Node> {
    let mut visible_owners: Vec<&Node> = Vec::new();
    if let Some(o) = owner {
        visible_owners.push(o);
    }
    for scope in scopes {
        for n in semantic_context.nodes_in_path(scope, path) {
            if matches!(n.kind.as_str(), "module" | "module_declaration")
                && !visible_owners.iter().any(|v| v.id == n.id)
            {
                visible_owners.push(n);
            }
        }
    }
    visible_owners
}

fn find_macro_in_imported_nodes<'a>(
    nodes: &[&'a Node],
    tail: &str,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
) -> Option<&'a Node> {
    if tail.is_empty() {
        return nodes.iter().copied().find(|n| n.kind == "macro");
    }
    nodes
        .iter()
        .copied()
        .filter(|mod_node| matches!(mod_node.kind.as_str(), "module" | "module_declaration"))
        .find_map(|mod_node| {
            find_macro_in_module(mod_node, &mod_node.qualname, tail, path, semantic_context)
        })
}

fn find_macro_in_external_target<'a>(
    target_path: &str,
    scopes: &[&str],
    tail: &str,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
) -> Option<&'a Node> {
    let target_clean = target_path
        .strip_prefix("crate::")
        .unwrap_or(target_path)
        .replace("::", ".");
    for scope in scopes {
        let full_target = if scope.is_empty() {
            target_clean.clone()
        } else {
            format!("{scope}.{target_clean}")
        };
        if tail.is_empty() {
            if let Some(m) = semantic_context
                .nodes_in_path(&full_target, path)
                .find(|n| n.kind == "macro")
            {
                return Some(m);
            }
            continue;
        }
        let mod_tail = format!("{full_target}.{tail}");
        if let Some(m) = semantic_context
            .nodes_in_path(&mod_tail, path)
            .find(|n| n.kind == "macro")
        {
            return Some(m);
        }
        for mod_node in semantic_context.nodes_in_path(&full_target, path) {
            if let Some(m) =
                find_macro_in_module(mod_node, &full_target, tail, path, semantic_context)
            {
                return Some(m);
            }
        }
    }
    None
}

fn find_imported_macro<'a>(
    scopes: &[&str],
    owner: Option<&'a Node>,
    head: &str,
    tail: &str,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'a>,
) -> Option<&'a Node> {
    let visible_owners = collect_visible_macro_owners(scopes, owner, path, semantic_context);
    for owner_node in visible_owners {
        let Some(imported) = semantic_context.imported_alias(owner_node, head) else {
            continue;
        };
        let matched = match imported {
            super::super::semantic_context::Imported::Nodes(nodes, _, _) => {
                find_macro_in_imported_nodes(nodes, tail, path, semantic_context)
            }
            super::super::semantic_context::Imported::External(target_path, _, _, _) => {
                find_macro_in_external_target(target_path, scopes, tail, path, semantic_context)
            }
            _ => None,
        };
        if matched.is_some() {
            return matched;
        }
    }
    None
}

fn resolve_local_macro_call(
    profile: Option<&dyn LanguageProfile>,
    reference: &Reference,
    owner: Option<&Node>,
    path: &str,
    semantic_context: &super::super::semantic_context::Context<'_>,
    graph: &mut CompactGraph,
) -> bool {
    let clean = reference.expression.replace("::", ".");
    let (head, tail) = clean.split_once('.').unwrap_or((&clean, ""));
    let scopes = collect_macro_scopes(owner);
    let local_macro = find_direct_macro_in_scopes(&scopes, &clean, head, tail, path, semantic_context)
        .or_else(|| find_imported_macro(&scopes, owner, head, tail, path, semantic_context));

    if let Some(target) = local_macro {
        graph.reference_edge(path, reference, target.id.clone(), "calls", "exact");
        graph.reference_coverage(
            path,
            reference,
            "resolved",
            "1 lexically justified candidates; implementor resolved=true, heuristic=false".into(),
        );
        macro_analysis_gap(profile, reference, &[target], path, graph);
        return true;
    }
    if let Some((provider, pack)) = declarative_macro_classification(&reference.expression) {
        graph.reference_coverage(
            path,
            reference,
            "external",
            format!("{pack}: declarative macro {provider}::{}", reference.expression).into(),
        );
        return true;
    }
    false
}

fn extract_flow_trait_object<'a>(
    target: &crate::linker::value_flow::TypeTarget<'a>,
) -> Option<&'a Node> {
    match target {
        crate::linker::value_flow::TypeTarget::Local(node) if node.kind == "trait" => Some(*node),
        crate::linker::value_flow::TypeTarget::RustReference { target, .. }
        | crate::linker::value_flow::TypeTarget::RustDeref { target, .. } => {
            extract_flow_trait_object(target)
        }
        _ => None,
    }
}

pub(in crate::linker) fn trait_object_call<'a>(
    profile: Option<&dyn LanguageProfile>,
    reference: &Reference,
    owner: Option<&Node>,
    path: &str,
    value_flow: &crate::linker::value_flow::ValueFlowIndex<'a>,
    semantic_context: &super::super::semantic_context::Context<'a>,
    graph: &mut CompactGraph,
) -> bool {
    if !profile.is_some_and(|profile| profile.id() == "rust") {
        return false;
    }
    if resolve_rust_constant(reference, owner, path, value_flow, semantic_context, graph) {
        return true;
    }
    if reference.kind != "calls" {
        return false;
    }
    if let Some(ReceiverHint::CallResult { callee, .. }) = &reference.receiver_hint {
        if callee == "macro_invocation"
            && resolve_local_macro_call(profile, reference, owner, path, semantic_context, graph)
        {
            return true;
        }
    }
    let Some(owner) = owner else {
        return false;
    };
    let Some((receiver, member)) = reference.expression.split_once('.') else {
        return false;
    };
    let flow_trait_object = value_flow
        .lookup(owner, receiver, reference.position())
        .and_then(extract_flow_trait_object);
    let param_dyn_trait = owner
        .parameter_type(receiver)
        .and_then(extract_dyn_trait_name);
    if param_dyn_trait.is_none() && flow_trait_object.is_none() {
        return false;
    }

    graph.push_coverage(
        path,
        reference.line,
        "trait_object_dispatch",
        "unresolved",
        format!(
            "analysis gap: Rust trait-object call through {} may dispatch to multiple implementations; concrete runtime target is unknown",
            receiver
        ),
    );
    let trait_node = flow_trait_object.or_else(|| {
        let trait_name = param_dyn_trait?;
        match semantic_context.resolve_symbol(
            owner,
            trait_name,
            reference.position(),
            crate::linker::value_flow::SymbolRole::Type,
        ) {
            crate::linker::value_flow::Symbol::Type(
                crate::linker::value_flow::TypeTarget::Local(node),
            ) if node.kind == "trait" => Some(node),
            _ => None,
        }
    });
    if let Some((declaration, implementors)) = trait_node
        .and_then(|trait_node| semantic_context.dynamic_dispatch(trait_node, member))
    {
        graph.reference_edge(path, reference, declaration.id.clone(), "calls", "inferred");
        for target in implementors {
            graph.reference_edge(
                path,
                reference,
                target.id.clone(),
                "possible_calls",
                "possible",
            );
        }
    }
    true
}

fn extract_dyn_trait_name(ty: &str) -> Option<&str> {
    let mut s = ty.trim();
    loop {
        if let Some(rest) = s.strip_prefix('&') {
            s = rest.trim_start();
            if s.starts_with('\'') {
                s = s.split_once(char::is_whitespace)?.1.trim_start();
            }
            s = s.strip_prefix("mut ").unwrap_or(s).trim_start();
            continue;
        }
        if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            s = inner.trim();
            continue;
        }
        let mut unwrapped = None;
        for wrapper in ["Box", "Arc", "Rc", "Pin"] {
            if let Some(rest) = s.strip_prefix(wrapper).map(str::trim_start) {
                if let Some(inner) = rest.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
                    unwrapped = Some(inner.trim());
                    break;
                }
            }
        }
        if let Some(inner) = unwrapped {
            s = inner;
            continue;
        }
        break;
    }
    let rest = s
        .strip_prefix("dyn")
        .filter(|rest| rest.starts_with(char::is_whitespace))?
        .trim_start();
    let mut depth = 0usize;
    let mut end_idx = rest.len();
    for (idx, ch) in rest.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => {
                if depth == 0 {
                    end_idx = idx;
                    break;
                }
                depth -= 1;
            }
            '+' if depth == 0 => {
                end_idx = idx;
                break;
            }
            _ if ch.is_whitespace() && depth == 0 => {
                end_idx = idx;
                break;
            }
            _ => {}
        }
    }
    let candidate = rest[..end_idx].trim();
    (!candidate.is_empty()).then_some(candidate)
}

pub(in crate::linker) fn macro_analysis_gap(
    profile: Option<&dyn LanguageProfile>,
    reference: &Reference,
    candidates: &[&Node],
    path: &str,
    graph: &mut CompactGraph,
) {
    if reference.kind != "calls"
        || !profile.is_some_and(|profile| profile.id() == "rust")
        || !candidates.iter().any(|candidate| candidate.kind == "macro")
        || candidates
            .iter()
            .any(|candidate| candidate.kind == "macro" && candidate.details["rust_macro_projected"] == true)
    {
        return;
    }
    graph.push_coverage(
        path,
        reference.line,
        "macro_expansion_not_analyzed",
        "unresolved",
        format!(
            "analysis gap: Rust macro invocation {} is linked to its declaration, but its expansion is not analyzed",
            reference.expression
        ),
    );
}

pub(in crate::linker) fn macro_reference_candidate(
    profile: Option<&dyn LanguageProfile>,
    owner: Option<&Node>,
    candidate: &Node,
) -> bool {
    profile.is_some_and(|profile| profile.id() == "rust")
        && owner.is_some_and(|owner| owner.kind == "macro")
        && matches!(candidate.kind.as_str(), "function" | "method")
}

pub(in crate::linker) fn impl_source<'a>(
    implementation: &Node,
    owner_scope: &str,
    file_module: &str,
    by_qual: &HashMap<&'a str, Vec<&'a Node>>,
    by_name: &HashMap<&'a str, Vec<&'a Node>>,
    aliases: &HashMap<&str, HashMap<&'a str, Vec<&'a Node>>>,
    lookup_key: &mut String,
) -> (Option<String>, bool) {
    let raw = implementation
        .name
        .split('<')
        .next()
        .unwrap_or(&implementation.name)
        .trim();
    let clean: std::borrow::Cow<'_, str> = if raw.contains("::") {
        std::borrow::Cow::Owned(raw.replace("::", "."))
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    let (head, rest) = clean.split_once('.').unwrap_or((clean.as_ref(), ""));
    let mut scope = owner_scope
        .rsplit_once('.')
        .map_or("", |(parent, _)| parent);
    let imported = loop {
        if let Some(targets) = aliases.get(scope).and_then(|entries| entries.get(head)) {
            break Some(targets);
        }
        if scope.is_empty() {
            break None;
        }
        scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
    };
    let is_nominal = |node: &&Node| matches!(node.kind.as_str(), "struct" | "enum");
    let mut types: Vec<&Node> = if let Some(imported) = imported {
        if rest.is_empty() {
            imported.iter().copied().filter(is_nominal).collect()
        } else {
            imported
                .iter()
                .flat_map(|node| {
                    by_qual
                        .get(super::super::qualified(lookup_key, &node.qualname, rest))
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(move |target| target.path == node.path)
                })
                .filter(is_nominal)
                .collect()
        }
    } else {
        by_qual
            .get(super::super::qualified(lookup_key, file_module, &clean))
            .into_iter()
            .flatten()
            .copied()
            .filter(is_nominal)
            .collect()
    };
    let mut source_heuristic = false;
    if types.is_empty() && imported.is_none() {
        types = by_name
            .get(clean.as_ref())
            .into_iter()
            .flatten()
            .copied()
            .filter(is_nominal)
            .collect();
        source_heuristic = !types.is_empty();
    }
    (
        (types.len() == 1).then(|| types[0].id.clone()),
        source_heuristic,
    )
}

#[cfg(all(test, feature = "lang-rust"))]
mod self_crate_resolution_tests {
    use crate::ast::extract;
    use crate::languages::dependency_registry::DependencyRegistry;
    use crate::linker;
    use acdd_core::fs::ScopedTempDir;
    use std::collections::BTreeMap;
    use std::fs;

    #[test]
    fn rust_self_crate_resolution_excludes_library_files_and_keeps_sibling_targets() {
        let dir = ScopedTempDir::create("contextunity-test-rust-self-crate").unwrap();
        let root = dir.path();

        for (pkg_prefix, crate_name) in [("", "my_crate"), ("crates/sub_crate/", "sub_crate")] {
            fs::create_dir_all(root.join(format!("{pkg_prefix}src/bin"))).unwrap();
            fs::create_dir_all(root.join(format!("{pkg_prefix}tests"))).unwrap();
            fs::create_dir_all(root.join(format!("{pkg_prefix}examples"))).unwrap();
            fs::create_dir_all(root.join(format!("{pkg_prefix}benches"))).unwrap();
            fs::write(
                root.join(format!("{pkg_prefix}Cargo.toml")),
                format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\n"),
            )
            .unwrap();
        }

        let files = [
            (
                "src/lib.rs",
                "pub mod sub;\npub fn target() {}\npub fn bad_self_call() { my_crate::target(); }\npub fn good_crate_call() { crate::target(); }\n",
            ),
            (
                "src/sub.rs",
                "use my_crate::target as imported_target;\npub fn bad_sub_direct() { my_crate::target(); }\npub fn bad_sub_use() { imported_target(); }\n",
            ),
            ("src/main.rs", "fn main_calls() { my_crate::target(); }\n"),
            ("src/bin/cli.rs", "fn bin_calls() { my_crate::target(); }\n"),
            ("tests/it.rs", "#[test]\nfn test_calls() { my_crate::target(); }\n"),
            ("examples/demo.rs", "fn example_calls() { my_crate::target(); }\n"),
            ("benches/perf.rs", "fn bench_calls() { my_crate::target(); }\n"),
            (
                "crates/sub_crate/src/lib.rs",
                "pub fn target() {}\npub fn bad_subcrate_self() { sub_crate::target(); }\n",
            ),
            (
                "crates/sub_crate/tests/it.rs",
                "#[test]\nfn subcrate_test_calls() { sub_crate::target(); }\n",
            ),
        ];

        let mut facts = BTreeMap::new();
        for &(rel, src) in &files {
            fs::write(root.join(rel), src).unwrap();
            facts.insert(rel.to_owned(), extract(rel, "rust", src).unwrap());
        }
        let registry = DependencyRegistry::collect(Some(root));
        let graph = linker::link_with_registry(&facts, None, Some(root), &registry);

        let has_call = |src_path: &str, src_fn: &str, dst_path: &str| {
            let src_id = &facts[src_path]
                .nodes
                .iter()
                .find(|n| n.name == src_fn)
                .unwrap()
                .id;
            let dst_id = &facts[dst_path]
                .nodes
                .iter()
                .find(|n| n.name == "target")
                .unwrap()
                .id;
            graph
                .edges
                .iter()
                .any(|e| e.kind == "calls" && &e.src == src_id && &e.dst == dst_id)
        };

        for (src_path, src_fn, dst_path, expect_linked, label) in [
            ("src/lib.rs", "bad_self_call", "src/lib.rs", false, "src/lib.rs must reject my_crate::target()"),
            ("src/lib.rs", "good_crate_call", "src/lib.rs", true, "src/lib.rs crate::target() links"),
            ("src/sub.rs", "bad_sub_direct", "src/lib.rs", false, "src/sub.rs must reject my_crate::target()"),
            ("src/sub.rs", "bad_sub_use", "src/lib.rs", false, "src/sub.rs must reject use my_crate::target"),
            ("src/main.rs", "main_calls", "src/lib.rs", true, "src/main.rs my_crate::target() links"),
            ("src/bin/cli.rs", "bin_calls", "src/lib.rs", true, "src/bin/cli.rs my_crate::target() links"),
            ("tests/it.rs", "test_calls", "src/lib.rs", true, "tests/it.rs my_crate::target() links"),
            ("examples/demo.rs", "example_calls", "src/lib.rs", true, "examples/demo.rs my_crate::target() links"),
            ("benches/perf.rs", "bench_calls", "src/lib.rs", true, "benches/perf.rs my_crate::target() links"),
            ("crates/sub_crate/src/lib.rs", "bad_subcrate_self", "crates/sub_crate/src/lib.rs", false, "subcrate src/lib.rs must reject sub_crate::target()"),
            ("crates/sub_crate/tests/it.rs", "subcrate_test_calls", "crates/sub_crate/src/lib.rs", true, "subcrate tests/it.rs sub_crate::target() links"),
        ] {
            assert_eq!(has_call(src_path, src_fn, dst_path), expect_linked, "{label}");
        }
    }
}
